use super::{Error, Result};

const MAX_FREQ: u32 = 124;

// PPMd-H suballocator parameters, per the format-level spec at
// rar-research/doc/PPMD_ALGORITHM_SPECIFICATION.md §4.
const ALLOC_UNIT_BYTES: usize = 12;
const N_BUCKETS: usize = 38;
const MAX_BUCKET_UNITS: usize = 128;
// Spec §4.3: countdown reset value after a successful glue pass.
const GLUE_RESET: u32 = 255;
// Sentinel offset for "no allocation" (binary contexts carry their state
// inline; their array_offset field is unused but typed `u32`).
const NULL_OFFSET: u32 = u32::MAX;

// Lookup tables built once from the spec's bucket-construction algorithm
// (§4.2):
//   for i in 0..38: step = if i >= 12 { 4 } else { i / 4 + 1 }
//                   repeat step times: units_to_index[k++] = i
//                   index_to_units[i] = k
struct AllocTables {
    units_to_index: [u8; MAX_BUCKET_UNITS],
    index_to_units: [u8; N_BUCKETS],
}

fn build_alloc_tables() -> AllocTables {
    let mut units_to_index = [0u8; MAX_BUCKET_UNITS];
    let mut index_to_units = [0u8; N_BUCKETS];
    let mut k: usize = 0;
    for (i, index_to_unit) in index_to_units.iter_mut().enumerate() {
        let step = if i >= 12 { 4 } else { i / 4 + 1 };
        for _ in 0..step {
            if k < units_to_index.len() {
                units_to_index[k] = i as u8;
            }
            k += 1;
        }
        *index_to_unit = k.min(u8::MAX as usize) as u8;
    }
    AllocTables {
        units_to_index,
        index_to_units,
    }
}

fn alloc_tables() -> &'static AllocTables {
    use std::sync::OnceLock;
    static TABLES: OnceLock<AllocTables> = OnceLock::new();
    TABLES.get_or_init(build_alloc_tables)
}

// Spec §4.1 layout: contexts grow downward from `hi_unit`, state arrays
// grow upward from `lo_unit`. Tracking the side matters for adjacency
// patterns (gluing) and for which bump pointer advances.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AllocSide {
    Lo, // state arrays
    Hi, // context headers
}

#[derive(Debug, Clone)]
struct Suballocator {
    // Original pool size in bytes (C's p->Size). Preserved across restarts so
    // the text/units split — which depends on the exact byte size, not the
    // unit-truncated one — stays identical to the reference each restart.
    size_bytes: usize,
    // Total unit budget (= dict_mb * 1 MiB / 12, capped at u32::MAX).
    pool_units: u32,
    // Byte slack at the pool top (= Size % UNIT_SIZE). The C reference sets
    // HiUnit = Base + Size with Size not necessarily a unit multiple, so all
    // its pointers sit at `unit*12 + rem`. We keep unit offsets but fold this
    // remainder into byte-domain comparisons (text capacity / fallback bump)
    // so restart timing matches the reference exactly.
    rem: u32,
    // Text region capacity in bytes. text_ptr reaching this triggers a
    // restart. Mirrors C's UnitsStart byte pointer (= Size - 84*text_units).
    text_capacity_bytes: usize,
    // Bump pointers (in units). lo_bump grows up, hi_bump grows down;
    // valid bumpable space is [lo_bump, hi_bump). Both start at units_start.
    units_start: u32,
    lo_bump: u32,
    hi_bump: u32,
    // Free lists hold released block offsets (in units), one list per bucket.
    free_lists: [Vec<u32>; N_BUCKETS],
    // Countdown for the glue pass (spec §4.3). Starts at 0, gets refreshed
    // to GLUE_RESET after each successful glue.
    glue_count: u32,
    // Test escape hatch: when reset hasn't been called (pool_units == 0),
    // act as unbounded so unit tests bypassing decode_init still work.
    unbounded: bool,
}

impl Default for Suballocator {
    fn default() -> Self {
        const EMPTY: Vec<u32> = Vec::new();
        Self {
            size_bytes: 0,
            pool_units: 0,
            rem: 0,
            text_capacity_bytes: 0,
            units_start: 0,
            lo_bump: 0,
            hi_bump: 0,
            free_lists: [EMPTY; N_BUCKETS],
            glue_count: 0,
            unbounded: true,
        }
    }
}

impl Suballocator {
    fn reset(&mut self, pool_bytes: usize) {
        let pool_units_usize = pool_bytes / ALLOC_UNIT_BYTES;
        // Cap at u32::MAX-1 (NULL_OFFSET is u32::MAX). Even 256 MiB / 12 =
        // ~22M fits comfortably, so this is theoretical.
        let pool_units = pool_units_usize.min(u32::MAX as usize - 1) as u32;
        // Ppmd7_RestartModel byte layout:
        //   text_units  = Size / 8 / UNIT_SIZE
        //   UnitsStart  = HiUnit - 7 * text_units * UNIT_SIZE
        // i.e. the units region is exactly 7*text_units units carved off the
        // top; the leftover slack (Size % 96, plus Size % 12) all stays in the
        // text region. The previous code took `pool_units / 8` for text and
        // gave the slack to the units region — 5+ extra bump units versus the
        // reference, which desynchronised restart timing on large inputs.
        let text_units = (pool_bytes / 8 / ALLOC_UNIT_BYTES) as u32;
        let units_region = text_units.saturating_mul(7).min(pool_units);
        self.size_bytes = pool_bytes;
        self.pool_units = pool_units;
        self.rem = (pool_bytes % ALLOC_UNIT_BYTES) as u32;
        self.units_start = pool_units - units_region;
        self.lo_bump = self.units_start;
        self.hi_bump = pool_units;
        // UnitsStart byte pointer = units_start*12 + rem (= Size - 84*text_units).
        self.text_capacity_bytes = self.units_start as usize * ALLOC_UNIT_BYTES + self.rem as usize;
        for fl in &mut self.free_lists {
            fl.clear();
        }
        self.glue_count = 0;
        self.unbounded = pool_bytes == 0;
    }

    fn bucket_for(units: usize) -> Option<usize> {
        if units == 0 || units > MAX_BUCKET_UNITS {
            return None;
        }
        Some(alloc_tables().units_to_index[units - 1] as usize)
    }

    fn bucket_units(bucket: usize) -> usize {
        alloc_tables().index_to_units[bucket] as usize
    }

    fn alloc(&mut self, units: usize, side: AllocSide, text_len_bytes: usize) -> Option<u32> {
        let bucket = Self::bucket_for(units)?;
        // Reference ordering differs by side. Lo-side state arrays go through
        // `Ppmd7_AllocUnits` (free_list first, then lo-bump, then rare). Hi-side
        // 1-unit context headers go through the inlined sequence in
        // `Ppmd7_CreateSuccessors` (hi-bump first, then free_list[0], then
        // `Ppmd7_AllocUnitsRare(0)`). The two are not interchangeable: every
        // time a free 1-unit block exists alongside hi-side headroom, the two
        // orderings consume different memory cells. Adjacency at the next glue
        // diverges, and the drift compounds across millions of decode steps.
        match side {
            AllocSide::Hi => {
                if let Some(offset) = self.try_bump(bucket, side) {
                    return Some(offset);
                }
                if let Some(offset) = self.free_lists[bucket].pop() {
                    return Some(offset);
                }
            }
            AllocSide::Lo => {
                if let Some(offset) = self.free_lists[bucket].pop() {
                    return Some(offset);
                }
                if let Some(offset) = self.try_bump(bucket, side) {
                    return Some(offset);
                }
            }
        }
        // Rare path (mirrors Ppmd7_AllocUnitsRare):
        //   1. If glue_count == 0, glue and retry the bucket's free list.
        //   2. Walk upward through larger buckets; if found, split.
        //   3. Otherwise shrink units_start (consume text-reservation).
        if self.glue_count == 0 {
            self.glue();
            self.glue_count = GLUE_RESET;
            if let Some(offset) = self.free_lists[bucket].pop() {
                return Some(offset);
            }
        }
        for i in (bucket + 1)..N_BUCKETS {
            if let Some(offset) = self.free_lists[i].pop() {
                let large_size = Self::bucket_units(i) as u32;
                let need = Self::bucket_units(bucket) as u32;
                let leftover = large_size - need;
                self.emit_run(offset + need, leftover);
                return Some(offset);
            }
        }
        self.glue_count = self.glue_count.saturating_sub(1);
        self.fallback_bump(bucket, text_len_bytes)
    }

    // Spec / ref Ppmd7_AllocUnitsRare bump-fallback: when no free block at
    // any bucket size satisfies the request, shrink `units_start` downward
    // (into the text-reservation zone). Succeeds only if the new units
    // floor would still sit above the current text-write position.
    fn fallback_bump(&mut self, bucket: usize, text_len_bytes: usize) -> Option<u32> {
        if self.unbounded {
            return None;
        }
        let need_units = Self::bucket_units(bucket) as u32;
        let need_bytes = need_units as u64 * ALLOC_UNIT_BYTES as u64;
        // UnitsStart byte pointer includes the pool-top remainder, matching
        // the C reference (all its pointers sit at unit*12 + rem).
        let units_start_bytes = self.units_start as u64 * ALLOC_UNIT_BYTES as u64 + self.rem as u64;
        // Match C: `(us - Text) > numBytes`. Strict greater-than (not ≥).
        if units_start_bytes <= text_len_bytes as u64 + need_bytes {
            return None;
        }
        let new_units_start = self.units_start - need_units;
        self.units_start = new_units_start;
        // Lower the text capacity so text_has_room reflects the new boundary.
        self.text_capacity_bytes = new_units_start as usize * ALLOC_UNIT_BYTES + self.rem as usize;
        Some(new_units_start)
    }

    fn try_bump(&mut self, bucket: usize, side: AllocSide) -> Option<u32> {
        let block = Self::bucket_units(bucket) as u32;
        if self.unbounded {
            // No real pool; hand out monotonic offsets so each alloc is
            // distinguishable but never adjacent to another (so gluing is
            // a no-op for the tests).
            let off = self.lo_bump;
            self.lo_bump = self.lo_bump.saturating_add(block);
            return Some(off);
        }
        // hi_bump - lo_bump is the bumpable headroom in units.
        if self.hi_bump.saturating_sub(self.lo_bump) < block {
            return None;
        }
        match side {
            AllocSide::Lo => {
                let off = self.lo_bump;
                self.lo_bump += block;
                Some(off)
            }
            AllocSide::Hi => {
                self.hi_bump -= block;
                Some(self.hi_bump)
            }
        }
    }

    fn free(&mut self, offset: u32, units: usize) {
        if offset == NULL_OFFSET {
            return;
        }
        if let Some(bucket) = Self::bucket_for(units) {
            self.free_lists[bucket].push(offset);
        }
    }

    // Ppmd7_SplitBlock: carve a block of `old_units` (bucket I2U value) down
    // to `new_units` (bucket I2U value) in place. The first `new_units`
    // stay at `base_offset`; the (old_units - new_units) residue is pushed
    // onto the appropriate smaller bucket(s). The "kept" prefix is NOT
    // pushed — the caller is still using it as live storage.
    fn split_in_place(&mut self, base_offset: u32, old_units: u32, new_units: u32) {
        let nu = old_units - new_units;
        let residue_offset = base_offset + new_units;
        let Some(mut i) = Self::bucket_for(nu as usize) else {
            return;
        };
        if Self::bucket_units(i) as u32 != nu {
            // Inexact fit: the C reference pushes the smaller piece first
            // (at offset + bucket_units(i-1)) onto bucket index `nu - k - 1`,
            // then the i-1-sized prefix onto bucket i-1. Order across
            // buckets is irrelevant; per-bucket LIFO is unaffected.
            let k = Self::bucket_units(i - 1) as u32;
            let small_bucket = (nu - k - 1) as usize;
            self.free_lists[small_bucket].push(residue_offset + k);
            i -= 1;
        }
        self.free_lists[i].push(residue_offset);
    }

    fn glue(&mut self) {
        self.glue_inner()
    }

    // Faithful port of Ppmd7_GlueFreeBlocks (§4.4). The earlier
    // sort-by-offset version diverged from the reference in three ways that
    // all change the resulting free-list state — and therefore the timing of
    // the next allocation failure / model restart:
    //   1. Threading order: the reference walks free_list[0..38], each list
    //      head-first (most-recent first), prepending every node to a single
    //      glue list. We rebuild that exact order so the fill pass re-buckets
    //      runs in the same sequence (per-bucket LIFO must match).
    //   2. Merge cap: a merged run may not reach 0x10000 units (NU is 16-bit).
    //   3. Fill split: runs are emitted as repeated 128-unit blocks, then the
    //      <=128 remainder via Ppmd7_SplitBlock's exact-or-two-piece rule —
    //      not "largest bucket fitting" greedily.
    fn glue_inner(&mut self) {
        use std::collections::HashMap;
        // Step 1: thread free blocks into the reference's list order.
        // list[k] = (offset, nu). Reference prepends; we emulate with a
        // front-growing build then it is naturally bucket-37-first.
        let mut list: Vec<(u32, u32)> = Vec::new();
        for bucket in 0..N_BUCKETS {
            let nu = Self::bucket_units(bucket) as u32;
            // Reference walks the bucket list head-first (recent->oldest) and
            // prepends each node. Iterating our Vec recent->oldest and
            // prepending reproduces that; doing it per-bucket with the whole
            // bucket prepended keeps later buckets in front.
            for &off in self.free_lists[bucket].iter().rev() {
                list.insert(0, (off, nu));
            }
            self.free_lists[bucket].clear();
        }
        if list.is_empty() {
            return;
        }

        // Step 2: glue pass. Absorb the physically-next free block while the
        // combined size stays < 0x10000. `nu_at` maps a block's start offset
        // to its current size; absorbed blocks are set to 0.
        let mut nu_at: HashMap<u32, u32> = HashMap::with_capacity(list.len() * 2);
        for &(off, nu) in &list {
            nu_at.insert(off, nu);
        }
        for &(off, _) in &list {
            let mut cur = match nu_at.get(&off) {
                Some(&n) if n != 0 => n,
                _ => continue,
            };
            loop {
                let next_off = off + cur;
                match nu_at.get(&next_off) {
                    Some(&n2) => {
                        let new = cur + n2;
                        if new >= 0x10000 {
                            break;
                        }
                        cur = new;
                        nu_at.insert(next_off, 0);
                    }
                    None => break,
                }
            }
            nu_at.insert(off, cur);
        }

        // Step 3: fill pass in list order.
        for &(off, _) in &list {
            let nu = match nu_at.get(&off) {
                Some(&n) if n != 0 => n,
                _ => continue,
            };
            self.emit_run(off, nu);
        }
    }

    // Re-bucket a single merged run exactly as Ppmd7_GlueFreeBlocks' fill /
    // Ppmd7_SplitBlock do: peel 128-unit (bucket 37) blocks, then split the
    // <=128 remainder into its exact bucket, or two pieces when inexact.
    fn emit_run(&mut self, mut offset: u32, mut remaining: u32) {
        const LAST_BUCKET: usize = N_BUCKETS - 1; // 37 == 128 units
        while remaining > MAX_BUCKET_UNITS as u32 {
            self.push_free(LAST_BUCKET, offset);
            offset += MAX_BUCKET_UNITS as u32;
            remaining -= MAX_BUCKET_UNITS as u32;
        }
        if remaining == 0 {
            return;
        }
        let Some(mut i) = Self::bucket_for(remaining as usize) else {
            return;
        };
        if Self::bucket_units(i) as u32 != remaining {
            let k = Self::bucket_units(i - 1) as u32;
            let small_bucket = (remaining - k - 1) as usize;
            self.push_free(small_bucket, offset + k);
            i -= 1;
        }
        self.push_free(i, offset);
    }

    fn push_free(&mut self, bucket: usize, offset: u32) {
        self.free_lists[bucket].push(offset);
    }

    fn text_has_room(&self, text_len: usize) -> bool {
        // When pool isn't initialised (test fixtures that bypass decode_init),
        // text_capacity is 0 — treat that as "unbounded" so we don't break
        // out-of-band callers.
        self.text_capacity_bytes == 0 || text_len < self.text_capacity_bytes
    }
}
const BIN_SCALE: u32 = 1 << 14;
const INT_BITS: u32 = 7;
const PERIOD_BITS: u8 = 7;
const TOP: u32 = 1 << 24;
const BOT: u32 = 1 << 15;
const INIT_BIN_ESC: [u16; 8] = [
    0x3cdd, 0x1f3f, 0x59bf, 0x48f3, 0x64a1, 0x5abc, 0x6632, 0x6051,
];
const EXP_ESCAPE: [u8; 16] = [25, 14, 9, 7, 5, 5, 4, 4, 4, 3, 3, 3, 2, 2, 2, 2];

pub trait PpmdByteReader {
    fn read_ppmd_byte(&mut self) -> Result<u8>;
}

#[derive(Debug, Clone)]
pub struct PpmdDecoder {
    min_context: usize,
    max_context: usize,
    found_state: StateRef,
    order_fall: usize,
    init_esc: u32,
    prev_success: u32,
    max_order: usize,
    hi_bits_flag: u32,
    run_length: i32,
    init_rl: i32,
    ns2bs_indx: [u8; 256],
    ns2indx: [u8; 256],
    bin_summ: [[u16; 64]; 128],
    see: [[See; 16]; 25],
    dummy_see: See,
    // The model's memory, laid out as Ppmd7's: the text area grows up from
    // byte 0, and contexts and state arrays sit at the units the suballocator
    // hands out (byte `unit * 12 + rem`). It is exactly the size the stream
    // declares, and nothing else in the model grows with the input.
    heap: Vec<u8>,
    text_len: usize,
    range: RangeDecoder,
    allocated: bool,
    suballoc: Suballocator,
}

#[derive(Debug, Clone)]
pub struct PpmdEncoder {
    model: PpmdDecoder,
    range: RangeEncoder,
    esc_char: u8,
}

// A context is one unit, as Ppmd7's CPpmd7_Context:
//   0..2 num_stats, 2..4 summ_freq, 4..8 stats (unit of the state array),
//   8..12 suffix (a context id, or NO_SUFFIX for the root).
// A binary context keeps its single state at 2..8 in place of summ_freq and
// stats; only multi-state contexts read summ_freq. A context's id is the unit
// it lives at.
const CTX_SUMM: usize = 2;
const CTX_STATS: usize = 4;
const CTX_SUFFIX: usize = 8;
// A state is symbol, freq and a 4-byte successor: two to a unit.
const STATE_BYTES: usize = 6;

const NO_SUFFIX: u32 = u32::MAX;

#[derive(Debug, Clone, Copy)]
struct State {
    symbol: u8,
    freq: u8,
    successor: Successor,
}

// Where a state leads: nothing (0), a position in the text area (1 + pos),
// or a context (CONTEXT_TAG | id). Text positions and context units stay
// below 256 MiB (the largest model), so both fit in 31 bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Successor(u32);

const CONTEXT_TAG: u32 = 0x8000_0000;

impl Successor {
    const fn none() -> Self {
        Self(0)
    }

    fn raw(pos: usize) -> Self {
        debug_assert!(pos < (CONTEXT_TAG - 1) as usize);
        Self((pos as u32).wrapping_add(1) & !CONTEXT_TAG)
    }

    fn context(index: usize) -> Self {
        debug_assert!(index < CONTEXT_TAG as usize);
        Self(CONTEXT_TAG | index as u32)
    }

    fn as_raw(self) -> Option<usize> {
        (self.0 != 0 && self.0 & CONTEXT_TAG == 0).then(|| (self.0 - 1) as usize)
    }

    fn as_context(self) -> Option<usize> {
        (self.0 & CONTEXT_TAG != 0).then_some((self.0 & !CONTEXT_TAG) as usize)
    }
}

#[derive(Debug, Clone, Copy)]
struct StateRef {
    context: usize,
    index: usize,
}

#[derive(Debug, Clone, Copy)]
struct See {
    summ: u16,
    shift: u8,
    count: u8,
}

#[derive(Debug, Clone)]
struct RangeDecoder {
    range: u32,
    code: u32,
    low: u32,
}

impl PpmdDecoder {
    pub fn new() -> Self {
        let mut ns2bs_indx = [0u8; 256];
        ns2bs_indx[0] = 0;
        ns2bs_indx[1] = 2;
        ns2bs_indx[2..11].fill(4);
        ns2bs_indx[11..].fill(6);

        let mut ns2indx = [0u8; 256];
        ns2indx[0] = 0;
        ns2indx[1] = 1;
        ns2indx[2] = 2;
        let mut m = 3u8;
        let mut k = 1u8;
        for item in ns2indx.iter_mut().skip(3) {
            *item = m;
            k -= 1;
            if k == 0 {
                m += 1;
                k = m - 2;
            }
        }

        Self {
            min_context: 0,
            max_context: 0,
            found_state: StateRef {
                context: 0,
                index: 0,
            },
            order_fall: 0,
            init_esc: 0,
            prev_success: 0,
            max_order: 0,
            hi_bits_flag: 0,
            run_length: 0,
            init_rl: 0,
            ns2bs_indx,
            ns2indx,
            bin_summ: [[0; 64]; 128],
            see: [[See {
                summ: 0,
                shift: 0,
                count: 0,
            }; 16]; 25],
            dummy_see: See {
                summ: 0,
                shift: PERIOD_BITS,
                count: 64,
            },
            heap: Vec::new(),
            text_len: 0,
            range: RangeDecoder::new(),
            allocated: false,
            suballoc: Suballocator::default(),
        }
    }

    // Units occupied by a context's state array. Single-state (binary)
    // contexts store the state inline, so the array is 0 units. Otherwise
    // ceil(n/2) units fit n states at 2-per-unit (states are 6 bytes each).
    fn state_array_units(n: usize) -> usize {
        if n < 2 {
            0
        } else {
            n.div_ceil(2)
        }
    }

    // Size the model's memory to `bytes`. The old model goes first, so a
    // block that changes the size never holds both.
    fn reset_pool(&mut self, bytes: usize) {
        if self.heap.len() != bytes {
            self.heap = Vec::new();
            self.heap = vec![0; bytes];
        }
        self.suballoc.reset(bytes);
    }

    // ---- Model memory ---------------------------------------------------

    fn pos(&self, unit: usize) -> usize {
        unit * ALLOC_UNIT_BYTES + self.suballoc.rem as usize
    }

    fn u16_at(&self, p: usize) -> u16 {
        u16::from_le_bytes([self.heap[p], self.heap[p + 1]])
    }

    fn set_u16(&mut self, p: usize, value: u16) {
        self.heap[p..p + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn u32_at(&self, p: usize) -> u32 {
        u32::from_le_bytes([
            self.heap[p],
            self.heap[p + 1],
            self.heap[p + 2],
            self.heap[p + 3],
        ])
    }

    fn set_u32(&mut self, p: usize, value: u32) {
        self.heap[p..p + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn num_stats(&self, c: usize) -> usize {
        self.u16_at(self.pos(c)) as usize
    }

    fn set_num_stats(&mut self, c: usize, n: usize) {
        let p = self.pos(c);
        self.set_u16(p, n as u16);
    }

    fn summ_freq(&self, c: usize) -> u16 {
        self.u16_at(self.pos(c) + CTX_SUMM)
    }

    fn set_summ_freq(&mut self, c: usize, value: u16) {
        let p = self.pos(c) + CTX_SUMM;
        self.set_u16(p, value);
    }

    fn stats(&self, c: usize) -> u32 {
        self.u32_at(self.pos(c) + CTX_STATS)
    }

    fn set_stats(&mut self, c: usize, unit: u32) {
        let p = self.pos(c) + CTX_STATS;
        self.set_u32(p, unit);
    }

    fn suffix(&self, c: usize) -> Option<usize> {
        let suffix = self.u32_at(self.pos(c) + CTX_SUFFIX);
        (suffix != NO_SUFFIX).then_some(suffix as usize)
    }

    // Byte position of a context's first state.
    fn states_base(&self, c: usize) -> usize {
        if self.num_stats(c) == 1 {
            self.pos(c) + CTX_SUMM
        } else {
            self.pos(self.stats(c) as usize)
        }
    }

    fn read_state(&self, p: usize) -> State {
        State {
            symbol: self.heap[p],
            freq: self.heap[p + 1],
            successor: Successor(self.u32_at(p + 2)),
        }
    }

    fn write_state(&mut self, p: usize, state: State) {
        self.heap[p] = state.symbol;
        self.heap[p + 1] = state.freq;
        self.set_u32(p + 2, state.successor.0);
    }

    fn state_at(&self, c: usize, index: usize) -> State {
        self.read_state(self.states_base(c) + index * STATE_BYTES)
    }

    fn swap_states(&mut self, c: usize, i: usize, j: usize) {
        let base = self.states_base(c);
        let a = self.read_state(base + i * STATE_BYTES);
        let b = self.read_state(base + j * STATE_BYTES);
        self.write_state(base + i * STATE_BYTES, b);
        self.write_state(base + j * STATE_BYTES, a);
    }

    fn find_symbol(&self, c: usize, symbol: u8) -> Option<usize> {
        let base = self.states_base(c);
        (0..self.num_stats(c)).find(|i| self.heap[base + i * STATE_BYTES] == symbol)
    }

    // A context id the model handed out never fails this; it guards the
    // `Result` paths against a reference that went stale.
    fn state_pos(&self, state: StateRef) -> Result<usize> {
        let invalid = Error::InvalidData("RAR PPMd state reference is invalid");
        let fits = state
            .context
            .checked_mul(ALLOC_UNIT_BYTES)
            .and_then(|p| p.checked_add(self.suballoc.rem as usize + ALLOC_UNIT_BYTES))
            .is_some_and(|end| end <= self.heap.len());
        if !fits || state.index >= self.num_stats(state.context) {
            return Err(invalid);
        }
        Ok(self.states_base(state.context) + state.index * STATE_BYTES)
    }

    fn state(&self, state: StateRef) -> Result<State> {
        Ok(self.read_state(self.state_pos(state)?))
    }

    fn set_freq(&mut self, state: StateRef, freq: u8) -> Result<()> {
        let p = self.state_pos(state)?;
        self.heap[p + 1] = freq;
        Ok(())
    }

    fn set_successor(&mut self, state: StateRef, successor: Successor) -> Result<()> {
        let p = self.state_pos(state)?;
        self.set_u32(p + 2, successor.0);
        Ok(())
    }

    // ---- Decoding -------------------------------------------------------

    pub fn decode_init(
        &mut self,
        first_byte: u8,
        input: &mut impl PpmdByteReader,
        esc_char: &mut u8,
    ) -> Result<()> {
        let reset = first_byte & 0x20 != 0;
        let max_mb = if reset {
            Some(input.read_ppmd_byte()?)
        } else {
            None
        };
        if first_byte & 0x40 != 0 {
            *esc_char = input.read_ppmd_byte()?;
        }
        self.range.init(input)?;
        if reset {
            let mut max_order = ((first_byte & 0x1f) as usize) + 1;
            if max_order > 16 {
                max_order = 16 + (max_order - 16) * 3;
            }
            if max_order == 1 {
                return Err(Error::InvalidData("RAR PPMd order is invalid"));
            }
            let dictionary_mb = max_mb.unwrap_or(0) as usize + 1;
            self.reset_pool(dictionary_mb * 1024 * 1024);
            self.init_model(max_order);
            self.allocated = true;
        } else if !self.allocated {
            return Err(Error::InvalidData("RAR PPMd block reuses missing model"));
        }
        Ok(())
    }

    pub fn decode_symbol(&mut self, input: &mut impl PpmdByteReader) -> Result<Option<u8>> {
        self.decode_symbol_inner(input)
    }

    fn decode_symbol_inner(&mut self, input: &mut impl PpmdByteReader) -> Result<Option<u8>> {
        let mut mask = [true; 256];
        let min = self.min_context;
        if self.num_stats(min) != 1 {
            let summ_freq = self.summ_freq(min) as u32;
            if summ_freq > self.range.range {
                return Err(Error::InvalidData("RAR PPMd range is invalid"));
            }
            let mut count = self.range.get_threshold(summ_freq)?;
            let mut hi_cnt = 0u32;
            let mut found = None;
            let base = self.states_base(min);
            let n = self.num_stats(min);
            for index in 0..n {
                let state = self.read_state(base + index * STATE_BYTES);
                if count < state.freq as u32 {
                    found = Some((index, hi_cnt, state.freq as u32, state.symbol));
                    break;
                }
                count -= state.freq as u32;
                hi_cnt += state.freq as u32;
            }
            if let Some((index, start, size, symbol)) = found {
                self.range.decode(start, size);
                self.found_state = StateRef {
                    context: min,
                    index,
                };
                if index == 0 {
                    self.update1_0()?;
                } else {
                    self.prev_success = 0;
                    self.update1()?;
                }
                self.range.normalize(input)?;
                return Ok(Some(symbol));
            }
            if hi_cnt >= summ_freq {
                return Err(Error::InvalidData("RAR PPMd frequency sum is invalid"));
            }
            self.prev_success = 0;
            self.range.decode(hi_cnt, summ_freq - hi_cnt);
            self.hi_bits_flag = hi_bits_flag(self.state(self.found_state)?.symbol, 3);
            for index in 0..n {
                mask[self.heap[base + index * STATE_BYTES] as usize] = false;
            }
        } else {
            let state = self.state_at(min, 0);
            let idx = self.bin_summ_index(state)?;
            let prob = self.bin_summ[idx.0][idx.1] as u32;
            let size0 = (self.range.range >> 14).wrapping_mul(prob);
            let next_prob = update_prob_1(prob);
            if self.range.code.wrapping_sub(self.range.low) < size0 {
                self.bin_summ[idx.0][idx.1] = (next_prob + (1 << INT_BITS)) as u16;
                self.range.decode_bit0(size0);
                self.found_state = StateRef {
                    context: min,
                    index: 0,
                };
                self.update_bin()?;
                self.range.normalize(input)?;
                return Ok(Some(state.symbol));
            }
            self.bin_summ[idx.0][idx.1] = next_prob as u16;
            self.init_esc = EXP_ESCAPE[(next_prob >> 10) as usize] as u32;
            self.range.decode_bit1(size0);
            mask[state.symbol as usize] = false;
            self.prev_success = 0;
        }

        loop {
            self.range.normalize(input)?;
            let mut mc = self.min_context;
            let num_masked = self.num_stats(mc);
            loop {
                self.order_fall += 1;
                let Some(suffix) = self.suffix(mc) else {
                    return Ok(None);
                };
                mc = suffix;
                if self.num_stats(mc) != num_masked {
                    break;
                }
            }
            self.min_context = mc;

            let base = self.states_base(mc);
            let n = self.num_stats(mc);
            let hi_cnt = (0..n)
                .map(|i| self.read_state(base + i * STATE_BYTES))
                .filter(|state| mask[state.symbol as usize])
                .map(|state| state.freq as u32)
                .sum::<u32>();
            let (see_ref, esc_freq) = self.make_esc_freq(num_masked)?;
            let freq_sum = hi_cnt + esc_freq;
            if freq_sum > self.range.range {
                return Err(Error::InvalidData("RAR PPMd escape range is invalid"));
            }
            let mut count = self.range.get_threshold(freq_sum)?;
            if count < hi_cnt {
                let mut start = 0u32;
                for index in 0..n {
                    let state = self.read_state(base + index * STATE_BYTES);
                    if !mask[state.symbol as usize] {
                        continue;
                    }
                    let freq = state.freq as u32;
                    if count < freq {
                        let symbol = state.symbol;
                        self.range.decode(start, freq);
                        self.update_see(see_ref);
                        self.found_state = StateRef { context: mc, index };
                        self.update2()?;
                        self.range.normalize(input)?;
                        return Ok(Some(symbol));
                    }
                    count -= freq;
                    start += freq;
                }
                return Err(Error::InvalidData("RAR PPMd masked symbol is invalid"));
            }
            if count >= freq_sum {
                return Err(Error::InvalidData("RAR PPMd escape symbol is invalid"));
            }
            self.range.decode(hi_cnt, freq_sum - hi_cnt);
            self.add_see_summ(see_ref, freq_sum);
            for index in 0..n {
                mask[self.heap[base + index * STATE_BYTES] as usize] = false;
            }
        }
    }

    fn encode_symbol(&mut self, symbol: u8, output: &mut RangeEncoder) -> Result<()> {
        let mut mask = [true; 256];
        let min = self.min_context;
        if self.num_stats(min) != 1 {
            let summ_freq = self.summ_freq(min) as u32;
            let mut start = 0u32;
            let mut found = None;
            let base = self.states_base(min);
            let n = self.num_stats(min);
            for index in 0..n {
                let state = self.read_state(base + index * STATE_BYTES);
                if state.symbol == symbol {
                    found = Some((index, start, state.freq as u32));
                    break;
                }
                start += state.freq as u32;
            }
            if let Some((index, start, size)) = found {
                output.encode(start, size, summ_freq);
                self.found_state = StateRef {
                    context: min,
                    index,
                };
                if index == 0 {
                    self.update1_0()?;
                } else {
                    self.prev_success = 0;
                    self.update1()?;
                }
                output.normalize();
                return Ok(());
            }
            if start >= summ_freq {
                return Err(Error::InvalidData("RAR PPMd frequency sum is invalid"));
            }
            self.prev_success = 0;
            output.encode(start, summ_freq - start, summ_freq);
            self.hi_bits_flag = hi_bits_flag(self.state(self.found_state)?.symbol, 3);
            for index in 0..n {
                mask[self.heap[base + index * STATE_BYTES] as usize] = false;
            }
        } else {
            let state = self.state_at(min, 0);
            let idx = self.bin_summ_index(state)?;
            let prob = self.bin_summ[idx.0][idx.1] as u32;
            let size0 = (output.range >> 14).wrapping_mul(prob);
            let next_prob = update_prob_1(prob);
            if state.symbol == symbol {
                self.bin_summ[idx.0][idx.1] = (next_prob + (1 << INT_BITS)) as u16;
                output.encode_bit0(size0);
                self.found_state = StateRef {
                    context: min,
                    index: 0,
                };
                self.update_bin()?;
                output.normalize();
                return Ok(());
            }
            self.bin_summ[idx.0][idx.1] = next_prob as u16;
            self.init_esc = EXP_ESCAPE[(next_prob >> 10) as usize] as u32;
            output.encode_bit1(size0);
            mask[state.symbol as usize] = false;
            self.prev_success = 0;
        }

        loop {
            output.normalize();
            let mut mc = self.min_context;
            let num_masked = self.num_stats(mc);
            loop {
                self.order_fall += 1;
                let Some(suffix) = self.suffix(mc) else {
                    return Err(Error::InvalidData("RAR PPMd symbol is not encodable"));
                };
                mc = suffix;
                if self.num_stats(mc) != num_masked {
                    break;
                }
            }
            self.min_context = mc;

            let base = self.states_base(mc);
            let n = self.num_stats(mc);
            let hi_cnt = (0..n)
                .map(|i| self.read_state(base + i * STATE_BYTES))
                .filter(|state| mask[state.symbol as usize])
                .map(|state| state.freq as u32)
                .sum::<u32>();
            let (see_ref, esc_freq) = self.make_esc_freq(num_masked)?;
            let freq_sum = hi_cnt + esc_freq;
            let mut start = 0u32;
            let mut found = None;
            for index in 0..n {
                let state = self.read_state(base + index * STATE_BYTES);
                if !mask[state.symbol as usize] {
                    continue;
                }
                let freq = state.freq as u32;
                if state.symbol == symbol {
                    found = Some((index, start, freq));
                    break;
                }
                start += freq;
            }
            if let Some((index, start, freq)) = found {
                output.encode(start, freq, freq_sum);
                self.update_see(see_ref);
                self.found_state = StateRef { context: mc, index };
                self.update2()?;
                output.normalize();
                return Ok(());
            }
            output.encode(hi_cnt, freq_sum - hi_cnt, freq_sum);
            self.add_see_summ(see_ref, freq_sum);
            for index in 0..n {
                mask[self.heap[base + index * STATE_BYTES] as usize] = false;
            }
        }
    }

    // Expects the pool sized by `reset_pool`.
    fn init_model(&mut self, max_order: usize) {
        self.text_len = 0;
        // Spec §5.2 RestartModel: clear free lists, reserve root context
        // (1 unit from Hi) + its 256-state array (128 units from Lo).
        // Reuse the exact original byte size (C's p->Size) so the text/units
        // split and the pool-top remainder survive the restart unchanged.
        self.suballoc.reset(self.suballoc.size_bytes);
        // A fresh pool of at least 1 MiB always holds these.
        let root = self
            .suballoc
            .alloc(1, AllocSide::Hi, 0)
            .expect("a fresh PPMd pool holds the root context") as usize;
        let array = self
            .suballoc
            .alloc(128, AllocSide::Lo, 0)
            .expect("a fresh PPMd pool holds the root state array");
        self.max_order = max_order;
        self.order_fall = max_order;
        self.init_rl = -(max_order.min(12) as i32) - 1;
        self.run_length = self.init_rl;
        self.prev_success = 0;

        self.set_num_stats(root, 256);
        self.set_summ_freq(root, 257);
        self.set_stats(root, array);
        let p = self.pos(root) + CTX_SUFFIX;
        self.set_u32(p, NO_SUFFIX);
        let base = self.pos(array as usize);
        for symbol in 0..=255u8 {
            self.write_state(
                base + symbol as usize * STATE_BYTES,
                State {
                    symbol,
                    freq: 1,
                    successor: Successor::none(),
                },
            );
        }
        self.min_context = root;
        self.max_context = root;
        self.found_state = StateRef {
            context: root,
            index: 0,
        };

        for i in 0..128 {
            for (k, &init_bin_esc) in INIT_BIN_ESC.iter().enumerate() {
                let value = BIN_SCALE - u32::from(init_bin_esc) / (i as u32 + 2);
                for m in (0..64).step_by(8) {
                    self.bin_summ[i][k + m] = value as u16;
                }
            }
        }
        for i in 0..25 {
            let summ = ((5 * i + 10) << (PERIOD_BITS - 4)) as u16;
            for k in 0..16 {
                self.see[i][k] = See {
                    summ,
                    shift: PERIOD_BITS - 4,
                    count: 4,
                };
            }
        }
        self.dummy_see = See {
            summ: 0,
            shift: PERIOD_BITS,
            count: 64,
        };
    }

    fn bin_summ_index(&mut self, state: State) -> Result<(usize, usize)> {
        let suffix = self
            .suffix(self.min_context)
            .ok_or(Error::InvalidData("RAR PPMd binary context has no suffix"))?;
        let suffix_stats = self.num_stats(suffix);
        self.hi_bits_flag = hi_bits_flag(self.state(self.found_state)?.symbol, 3);
        let row = state.freq as usize - 1;
        let col = self.prev_success as usize
            + ((self.run_length >> 26) as usize & 0x20)
            + self.ns2bs_indx[suffix_stats - 1] as usize
            + hi_bits_flag(state.symbol, 4) as usize
            + self.hi_bits_flag as usize;
        Ok((row, col))
    }

    fn make_esc_freq(&mut self, num_masked: usize) -> Result<(SeeRef, u32)> {
        let mc = self.min_context;
        let num_stats = self.num_stats(mc);
        if num_stats == 256 {
            return Ok((SeeRef::Dummy, 1));
        }
        if num_masked >= num_stats {
            return Err(Error::InvalidData("RAR PPMd masked-state count is invalid"));
        }
        let non_masked = num_stats
            .checked_sub(num_masked)
            .ok_or(Error::InvalidData("RAR PPMd masked-state count is invalid"))?;
        let suffix = self.suffix(mc).unwrap_or(mc);
        let suffix_stats = self.num_stats(suffix);
        // Spec §9.1: the subtraction is unsigned C wraparound — when
        // `suffix.num_stats < min_context.num_stats`, the underflow yields a
        // very large value and the `non_masked < suffix_delta` comparison
        // evaluates to TRUE. Replicate the wraparound here.
        let suffix_delta = suffix_stats.wrapping_sub(num_stats);
        let col = (non_masked < suffix_delta) as usize
            + 2 * ((self.summ_freq(mc) as usize) < 11 * num_stats) as usize
            + 4 * (num_masked > non_masked) as usize
            + self.hi_bits_flag as usize;
        let row = self.ns2indx[non_masked - 1] as usize;
        let see = &mut self.see[row][col];
        let summ = see.summ;
        let r = (summ >> see.shift) as u32;
        see.summ = summ.wrapping_sub(r as u16);
        Ok((SeeRef::Table(row, col), r + u32::from(r == 0)))
    }

    fn update_see(&mut self, see_ref: SeeRef) {
        let see = match see_ref {
            SeeRef::Dummy => &mut self.dummy_see,
            SeeRef::Table(row, col) => &mut self.see[row][col],
        };
        if see.shift < PERIOD_BITS {
            see.count = see.count.wrapping_sub(1);
            if see.count == 0 {
                see.summ = see.summ.wrapping_shl(1);
                see.count = 3 << see.shift;
                see.shift += 1;
            }
        }
    }

    fn add_see_summ(&mut self, see_ref: SeeRef, value: u32) {
        let see = match see_ref {
            SeeRef::Dummy => &mut self.dummy_see,
            SeeRef::Table(row, col) => &mut self.see[row][col],
        };
        see.summ = see.summ.wrapping_add(value as u16);
    }

    fn update1_0(&mut self) -> Result<()> {
        let fs = self.found_state;
        let freq = self.state(fs)?.freq as u32;
        let summ_freq = self.summ_freq(fs.context) as u32;
        self.prev_success = u32::from(2 * freq > summ_freq);
        self.run_length += self.prev_success as i32;
        self.set_summ_freq(fs.context, (summ_freq as u16).wrapping_add(4));
        self.set_freq(fs, (freq + 4) as u8)?;
        if freq + 4 > MAX_FREQ {
            self.rescale();
        }
        self.next_context()
    }

    fn update1(&mut self) -> Result<()> {
        let fs = self.found_state;
        let freq = self.state(fs)?.freq as u32 + 4;
        let summ_freq = self.summ_freq(fs.context);
        self.set_summ_freq(fs.context, summ_freq.wrapping_add(4));
        self.set_freq(fs, freq as u8)?;
        if fs.index > 0
            && self.state_at(fs.context, fs.index).freq
                > self.state_at(fs.context, fs.index - 1).freq
        {
            self.swap_states(fs.context, fs.index, fs.index - 1);
            self.found_state.index -= 1;
            if freq > MAX_FREQ {
                self.rescale();
            }
        }
        self.next_context()
    }

    fn update2(&mut self) -> Result<()> {
        let fs = self.found_state;
        let freq = self.state(fs)?.freq as u32 + 4;
        self.run_length = self.init_rl;
        let summ_freq = self.summ_freq(fs.context);
        self.set_summ_freq(fs.context, summ_freq.wrapping_add(4));
        self.set_freq(fs, freq as u8)?;
        if freq > MAX_FREQ {
            self.rescale();
        }
        self.update_model()
    }

    fn update_bin(&mut self) -> Result<()> {
        let fs = self.found_state;
        let freq = self.state(fs)?.freq;
        self.set_freq(fs, freq.wrapping_add(u8::from(freq < 128)))?;
        self.prev_success = 1;
        self.run_length += 1;
        self.next_context()
    }

    fn next_context(&mut self) -> Result<()> {
        let successor = self.state(self.found_state)?.successor;
        if let Some(context) = successor.as_context() {
            if self.order_fall == 0 {
                self.max_context = context;
                self.min_context = context;
                return Ok(());
            }
        }
        self.update_model()
    }

    fn update_model(&mut self) -> Result<()> {
        let fs = self.state(self.found_state)?;
        let found_symbol = fs.symbol;
        if fs.freq < (MAX_FREQ / 4) as u8 {
            if let Some(suffix) = self.suffix(self.min_context) {
                let base = self.states_base(suffix);
                if self.num_stats(suffix) == 1 {
                    if self.heap[base + 1] < 32 {
                        self.heap[base + 1] += 1;
                    }
                } else if let Some(mut index) = self.find_symbol(suffix, found_symbol) {
                    let freq_at = |s: &Self, i: usize| s.heap[base + i * STATE_BYTES + 1];
                    if index > 0 && freq_at(self, index) >= freq_at(self, index - 1) {
                        self.swap_states(suffix, index, index - 1);
                        index -= 1;
                    }
                    if freq_at(self, index) < (MAX_FREQ - 9) as u8 {
                        self.heap[base + index * STATE_BYTES + 1] += 2;
                        let summ_freq = self.summ_freq(suffix);
                        self.set_summ_freq(suffix, summ_freq.wrapping_add(2));
                    }
                }
            }
        }

        if self.order_fall == 0 {
            let Some(context) = self.create_successors() else {
                self.init_model(self.max_order);
                return Ok(());
            };
            self.max_context = context;
            self.min_context = context;
            self.set_successor(self.found_state, Successor::context(context))?;
            return Ok(());
        }

        // Match Ppmd7_UpdateModel exactly: advance text first, then check
        // `text >= UnitsStart`. The previous "check then push" version
        // restarted one symbol later than the reference, advancing the range
        // coder by one extra symbol before resetting — which left the
        // post-restart state out of sync. The text never reaches the units:
        // it stops one byte short of UnitsStart, and the model restarts.
        self.heap[self.text_len] = found_symbol;
        self.text_len += 1;
        let max_successor = Successor::raw(self.text_len);
        if !self.suballoc.text_has_room(self.text_len) {
            self.init_model(self.max_order);
            return Ok(());
        }
        let mut min_successor = fs.successor;
        if min_successor != Successor::none() {
            if min_successor.as_raw().is_some() {
                let Some(context) = self.create_successors() else {
                    self.init_model(self.max_order);
                    return Ok(());
                };
                min_successor = Successor::context(context);
            }
            self.order_fall -= 1;
            if self.order_fall == 0 && self.max_context != self.min_context {
                self.text_len -= 1;
            }
        } else {
            self.set_successor(self.found_state, max_successor)?;
            min_successor = Successor::context(self.min_context);
        }

        let mc = self.min_context;
        let mut c = self.max_context;
        self.min_context = min_successor.as_context().unwrap_or(self.min_context);
        self.max_context = self.min_context;
        if c == mc {
            return Ok(());
        }

        let ns = self.num_stats(mc) as u32;
        let s0 = self
            .summ_freq(mc)
            .checked_sub(ns as u16)
            .map(u32::from)
            .and_then(|value| value.checked_sub(fs.freq as u32 - 1))
            .ok_or(Error::InvalidData("RAR PPMd model frequency is invalid"))?;
        while c != mc {
            let ns1 = self.num_stats(c) as u32;
            let mut sum;
            if ns1 != 1 {
                sum = self.summ_freq(c) as u32;
                sum += u32::from(2 * ns1 < ns) + 2 * u32::from(4 * ns1 <= ns && sum <= 8 * ns1);
            } else {
                let p = self.states_base(c) + 1;
                let old = self.heap[p];
                let freq = if old < (MAX_FREQ / 4 - 1) as u8 {
                    old * 2
                } else {
                    (MAX_FREQ - 4) as u8
                };
                self.heap[p] = freq;
                sum = freq as u32 + self.init_esc + u32::from(ns > 3);
            }

            let mut cf = (sum + 6)
                .checked_mul(2)
                .and_then(|value| value.checked_mul(fs.freq as u32))
                .ok_or(Error::InvalidData("RAR PPMd model frequency overflows"))?;
            let sf = s0
                .checked_add(sum)
                .ok_or(Error::InvalidData("RAR PPMd model frequency overflows"))?;
            if sf == 0 {
                return Err(Error::InvalidData("RAR PPMd model frequency is invalid"));
            }
            if cf < 6 * sf {
                cf = 1 + u32::from(cf > sf) + u32::from(cf >= 4 * sf);
                sum += 3;
            } else {
                cf = 4
                    + u32::from(cf >= 9 * sf)
                    + u32::from(cf >= 12 * sf)
                    + u32::from(cf >= 15 * sf);
                sum += cf;
            }
            let state = State {
                symbol: found_symbol,
                freq: cf as u8,
                successor: if self.order_fall == 0 {
                    min_successor
                } else {
                    max_successor
                },
            };
            if !self.add_state(c, state) {
                self.init_model(self.max_order);
                return Ok(());
            }
            let sum = u16::try_from(sum)
                .map_err(|_| Error::InvalidData("RAR PPMd model frequency overflows"))?;
            self.set_summ_freq(c, sum);
            c = self.suffix(c).unwrap_or(mc);
        }
        Ok(())
    }

    fn create_successors(&mut self) -> Option<usize> {
        let successor = self.state(self.found_state).ok()?.successor;
        let Some(up_branch) = successor.as_raw() else {
            return successor.as_context().filter(|_| self.order_fall == 0);
        };
        let mut c = self.min_context;
        let mut ps = Vec::new();
        if self.order_fall != 0 {
            ps.push(self.found_state);
        }
        while let Some(suffix) = self.suffix(c) {
            c = suffix;
            let found_symbol = self.state(self.found_state).ok()?.symbol;
            let index = self.find_symbol(c, found_symbol)?;
            let successor = self.state_at(c, index).successor;
            if successor.as_raw() != Some(up_branch) {
                if let Some(context) = successor.as_context() {
                    c = context;
                    if ps.is_empty() {
                        return Some(c);
                    }
                    break;
                }
                return None;
            }
            ps.push(StateRef { context: c, index });
        }
        if ps.is_empty() {
            return Some(c);
        }

        if up_branch >= self.text_len {
            return None;
        }
        let new_sym = self.heap[up_branch];
        let up_successor = Successor::raw(up_branch + 1);
        let new_freq = if self.num_stats(c) == 1 {
            self.state_at(c, 0).freq
        } else {
            let state = self.state_at(c, self.find_symbol(c, new_sym)?);
            let cf = state.freq as u32 - 1;
            let s0 = self.summ_freq(c) as u32 - self.num_stats(c) as u32 - cf;
            (1 + if 2 * cf <= s0 {
                u32::from(5 * cf > s0)
            } else {
                (2 * cf + 3 * s0 - 1) / (2 * s0)
            }) as u8
        };

        while let Some(state_ref) = ps.pop() {
            let context = self.new_binary_context(
                State {
                    symbol: new_sym,
                    freq: new_freq,
                    successor: up_successor,
                },
                c,
            )?;
            self.set_successor(state_ref, Successor::context(context))
                .ok()?;
            c = context;
        }
        Some(c)
    }

    // A one-state context: one unit from the Hi side, its state inline.
    fn new_binary_context(&mut self, state: State, suffix: usize) -> Option<usize> {
        let context = self.suballoc.alloc(1, AllocSide::Hi, self.text_len)? as usize;
        let p = self.pos(context);
        self.set_u16(p, 1);
        self.write_state(p + CTX_SUMM, state);
        self.set_u32(p + CTX_SUFFIX, suffix as u32);
        Some(context)
    }

    // Append a state, growing the array as ExpandUnits does: if the same
    // bucket still fits, it stays put; otherwise allocate the new size first,
    // copy, then release the old block. A binary context moves its inline
    // state into a fresh one-unit array. False when the pool is exhausted.
    fn add_state(&mut self, c: usize, state: State) -> bool {
        let n = self.num_stats(c);
        let old_units = Self::state_array_units(n);
        let new_units = Self::state_array_units(n + 1);
        if old_units == 0
            || Suballocator::bucket_for(old_units) != Suballocator::bucket_for(new_units)
        {
            let Some(new) = self.suballoc.alloc(new_units, AllocSide::Lo, self.text_len) else {
                return false;
            };
            let from = self.states_base(c);
            let to = self.pos(new as usize);
            self.heap.copy_within(from..from + n * STATE_BYTES, to);
            if old_units > 0 {
                let old = self.stats(c);
                self.suballoc.free(old, old_units);
            }
            self.set_stats(c, new);
        }
        let p = self.pos(self.stats(c) as usize) + n * STATE_BYTES;
        self.write_state(p, state);
        self.set_num_stats(c, n + 1);
        true
    }

    // Rescale shrinks a state array. Mirrors Ppmd7_Rescale's branch at
    // Ppmd7.c:901-919: if the target bucket has a free block, swap to it
    // (pop i1, push old to i0); otherwise SplitBlock the existing block in
    // place — the first i1 units stay at the current address as the new
    // array, the (i0 - i1)-unit residue is bucketed onto smaller free lists.
    // No bump is consumed on the in-place path, which matters because over
    // many rescales the reference never grows lo_bump from shrinks.
    //
    // Collapse-to-unary (new_n == 1, new_units == 0) is handled by Rescale's
    // earlier branch (Ppmd7.c:881-898), which frees `stats` at bucket
    // U2I(n0) and copies the surviving state into the inline OneState slot.
    //
    // Only the block moves; the caller writes the states into it.
    fn shrink_state_array(&mut self, ctx: usize, old_n: usize, new_n: usize) {
        let old_units = Self::state_array_units(old_n);
        let new_units = Self::state_array_units(new_n);
        if old_units == 0 || new_units >= old_units {
            return;
        }
        let old_offset = self.stats(ctx);
        if new_units == 0 {
            self.suballoc.free(old_offset, old_units);
            return;
        }
        let Some(i0) = Suballocator::bucket_for(old_units) else {
            return;
        };
        let Some(i1) = Suballocator::bucket_for(new_units) else {
            return;
        };
        if i0 == i1 {
            return;
        }
        if let Some(swap_offset) = self.suballoc.free_lists[i1].pop() {
            // Swap path: take a same-sized block from the target bucket,
            // return the oversized block to its bucket.
            self.suballoc.free(old_offset, old_units);
            self.set_stats(ctx, swap_offset);
        } else {
            // SplitBlock in place: keep the first I2U(i1) units at the same
            // address, bucket the residue. Matches Ppmd7_SplitBlock.
            let i0_units = Suballocator::bucket_units(i0) as u32;
            let i1_units = Suballocator::bucket_units(i1) as u32;
            self.suballoc.split_in_place(old_offset, i0_units, i1_units);
        }
    }

    fn rescale(&mut self) {
        let ctx = self.min_context;
        let original_state_count = self.num_stats(ctx);
        let base = self.states_base(ctx);
        let mut states = (0..original_state_count)
            .map(|i| self.read_state(base + i * STATE_BYTES))
            .collect::<Vec<_>>();
        let found = self.found_state.index;
        if found != 0 {
            let state = states.remove(found);
            states.insert(0, state);
            self.found_state.index = 0;
        }
        let mut sum_freq = states[0].freq as u32;
        let mut esc_freq = self.summ_freq(ctx) as u32 - sum_freq;
        let adder = u32::from(self.order_fall != 0);
        sum_freq = (sum_freq + 4 + adder) >> 1;
        states[0].freq = sum_freq as u8;
        for index in 1..states.len() {
            let freq = states[index].freq as u32;
            esc_freq -= freq;
            let freq = (freq + adder) >> 1;
            sum_freq += freq;
            states[index].freq = freq as u8;
            let mut j = index;
            while j > 0 && states[j].freq > states[j - 1].freq {
                states.swap(j, j - 1);
                j -= 1;
            }
        }
        while states.last().is_some_and(|state| state.freq == 0) {
            states.pop();
            esc_freq += 1;
        }
        self.found_state.index = 0;
        if states.len() == 1 {
            let mut freq = states[0].freq as u32;
            while esc_freq > 1 {
                esc_freq >>= 1;
                freq = (freq + 1) >> 1;
            }
            states[0].freq = freq as u8;
            self.shrink_state_array(ctx, original_state_count, 1);
            self.set_num_stats(ctx, 1);
            let p = self.pos(ctx) + CTX_SUMM;
            self.write_state(p, states[0]);
            return;
        }
        self.set_summ_freq(ctx, (sum_freq + esc_freq - (esc_freq >> 1)) as u16);
        self.shrink_state_array(ctx, original_state_count, states.len());
        self.set_num_stats(ctx, states.len());
        let base = self.pos(self.stats(ctx) as usize);
        for (i, state) in states.iter().enumerate() {
            self.write_state(base + i * STATE_BYTES, *state);
        }
    }
}

impl PpmdEncoder {
    pub fn new(max_order: usize, esc_char: u8, dictionary_mb: usize) -> Result<Self> {
        if !(2..=64).contains(&max_order) {
            return Err(Error::InvalidData("RAR PPMd order is invalid"));
        }
        // The block header stores the size in one byte (1..=256 MiB), which
        // also keeps text positions and context indices within 31 bits.
        if !(1..=256).contains(&dictionary_mb) {
            return Err(Error::InvalidData("RAR PPMd dictionary size is invalid"));
        }
        let mut model = PpmdDecoder::new();
        model.reset_pool(dictionary_mb * 1024 * 1024);
        model.init_model(max_order);
        model.allocated = true;
        Ok(Self {
            model,
            range: RangeEncoder::new(),
            esc_char,
        })
    }

    /// Carries a model from one block into the next.
    ///
    /// A block that clears the reset bit in its header leaves the reader's
    /// model alone and only re-reads the range coder, so a solid chain can keep
    /// one model across every member in it. Each block still gets a fresh range
    /// coder, which is what lets a member's packed bytes start on a byte
    /// boundary of their own.
    pub fn continuing(model: PpmdDecoder, esc_char: u8) -> Self {
        Self {
            model,
            range: RangeEncoder::new(),
            esc_char,
        }
    }

    /// Ends the block and hands the model back for the next one to continue.
    pub fn finish_keeping_model(mut self) -> Result<(Vec<u8>, PpmdDecoder)> {
        self.model.encode_symbol(self.esc_char, &mut self.range)?;
        self.model.encode_symbol(2, &mut self.range)?;
        Ok((self.range.finish(), self.model))
    }

    pub fn encode_literal(&mut self, symbol: u8) -> Result<()> {
        self.model.encode_symbol(symbol, &mut self.range)?;
        if symbol == self.esc_char {
            self.model.encode_symbol(1, &mut self.range)?;
        }
        Ok(())
    }

    pub fn encode_repeat_offset_one(&mut self, length: usize) -> Result<()> {
        if !(4..=259).contains(&length) {
            return Err(Error::InvalidData(
                "RAR PPMd offset-one repeat length is invalid",
            ));
        }
        self.model.encode_symbol(self.esc_char, &mut self.range)?;
        self.model.encode_symbol(5, &mut self.range)?;
        self.model
            .encode_symbol((length - 4) as u8, &mut self.range)?;
        Ok(())
    }

    pub fn encode_match(&mut self, offset: usize, length: usize) -> Result<()> {
        if !(2..=0x1000001).contains(&offset) || !(32..=287).contains(&length) {
            return Err(Error::InvalidData("RAR PPMd match is invalid"));
        }
        let encoded_offset = offset - 2;
        self.model.encode_symbol(self.esc_char, &mut self.range)?;
        self.model.encode_symbol(4, &mut self.range)?;
        self.model
            .encode_symbol(((encoded_offset >> 16) & 0xff) as u8, &mut self.range)?;
        self.model
            .encode_symbol(((encoded_offset >> 8) & 0xff) as u8, &mut self.range)?;
        self.model
            .encode_symbol((encoded_offset & 0xff) as u8, &mut self.range)?;
        self.model
            .encode_symbol((length - 32) as u8, &mut self.range)?;
        Ok(())
    }

    pub fn encode_vm_filter_record(&mut self, record: &[u8]) -> Result<()> {
        self.model.encode_symbol(self.esc_char, &mut self.range)?;
        self.model.encode_symbol(3, &mut self.range)?;
        for &byte in record {
            self.model.encode_symbol(byte, &mut self.range)?;
        }
        Ok(())
    }

    /// Bits spent so far, to fractional precision. log2 of the range register
    /// falls as symbols narrow the interval and rises by eight for every byte
    /// normalisation flushes, so the difference between two readings is the
    /// cost of whatever was coded between them. The RAR 2.9 tokeniser reads
    /// this to price escape tokens against the literals they would replace.
    pub(crate) fn spent_bits(&self) -> f64 {
        8.0 * self.range.out.len() as f64 - f64::from(self.range.range.max(1)).log2()
    }
}

#[derive(Debug, Clone, Copy)]
enum SeeRef {
    Dummy,
    Table(usize, usize),
}

impl RangeDecoder {
    fn new() -> Self {
        Self {
            range: 0xffff_ffff,
            code: 0,
            low: 0,
        }
    }

    fn init(&mut self, input: &mut impl PpmdByteReader) -> Result<()> {
        self.code = 0;
        self.range = 0xffff_ffff;
        self.low = 0;
        for _ in 0..4 {
            self.code = (self.code << 8) | input.read_ppmd_byte()? as u32;
        }
        if self.code == 0xffff_ffff {
            return Err(Error::InvalidData("RAR PPMd range code is invalid"));
        }
        Ok(())
    }

    fn get_threshold(&mut self, total: u32) -> Result<u32> {
        if total == 0 {
            return Err(Error::InvalidData("RAR PPMd frequency sum is zero"));
        }
        self.range /= total;
        Ok(self.code.wrapping_sub(self.low) / self.range)
    }

    fn decode(&mut self, start: u32, size: u32) {
        let start = start.wrapping_mul(self.range);
        self.low = self.low.wrapping_add(start);
        self.range = self.range.wrapping_mul(size);
    }

    fn decode_bit0(&mut self, size0: u32) {
        self.range = size0;
    }

    fn decode_bit1(&mut self, size0: u32) {
        self.low = self.low.wrapping_add(size0);
        self.range = (self.range & !(BIN_SCALE - 1)).wrapping_sub(size0);
    }

    fn normalize(&mut self, input: &mut impl PpmdByteReader) -> Result<()> {
        while (self.low ^ self.low.wrapping_add(self.range)) < TOP
            || (self.range < BOT && {
                self.range = self.low.wrapping_neg() & (BOT - 1);
                true
            })
        {
            self.code = (self.code << 8) | input.read_ppmd_byte()? as u32;
            self.range <<= 8;
            self.low <<= 8;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct RangeEncoder {
    low: u32,
    range: u32,
    out: Vec<u8>,
}

impl RangeEncoder {
    fn new() -> Self {
        Self {
            low: 0,
            range: 0xffff_ffff,
            out: Vec::new(),
        }
    }

    fn encode(&mut self, start: u32, size: u32, total: u32) {
        self.range /= total;
        self.low = self.low.wrapping_add(start.wrapping_mul(self.range));
        self.range = self.range.wrapping_mul(size);
    }

    fn encode_bit0(&mut self, size0: u32) {
        self.range = size0;
    }

    fn encode_bit1(&mut self, size0: u32) {
        self.low = self.low.wrapping_add(size0);
        self.range = (self.range & !(BIN_SCALE - 1)).wrapping_sub(size0);
    }

    fn normalize(&mut self) {
        while (self.low ^ self.low.wrapping_add(self.range)) < TOP
            || (self.range < BOT && {
                self.range = self.low.wrapping_neg() & (BOT - 1);
                true
            })
        {
            self.out.push((self.low >> 24) as u8);
            self.range <<= 8;
            self.low <<= 8;
        }
    }

    fn finish(mut self) -> Vec<u8> {
        for _ in 0..4 {
            self.out.push((self.low >> 24) as u8);
            self.low <<= 8;
        }
        self.out
    }
}

fn update_prob_1(prob: u32) -> u32 {
    prob - ((prob + (1 << 5)) >> INT_BITS)
}

fn hi_bits_flag(symbol: u8, bits: u32) -> u32 {
    ((symbol as u32 + 0xc0) >> (8 - bits)) & (1 << bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_tables_match_spec_pattern() {
        let t = alloc_tables();
        // Spec §4.2: bucket sizes are 1,2,3,4 then groups of 4 at strides 1,2,3,4.
        // First 12 buckets cover 1..4, then 6,8,10,12, then 15,18,21,24.
        assert_eq!(t.index_to_units[0], 1);
        assert_eq!(t.index_to_units[1], 2);
        assert_eq!(t.index_to_units[2], 3);
        assert_eq!(t.index_to_units[3], 4);
        assert_eq!(t.index_to_units[4], 6);
        assert_eq!(t.index_to_units[5], 8);
        assert_eq!(t.index_to_units[6], 10);
        assert_eq!(t.index_to_units[7], 12);
        assert_eq!(t.index_to_units[8], 15);
        assert_eq!(t.index_to_units[9], 18);
        assert_eq!(t.index_to_units[10], 21);
        assert_eq!(t.index_to_units[11], 24);
        // From bucket 12 onward, stride is 4: 28, 32, 36, 40 ... up to 128.
        assert_eq!(t.index_to_units[12], 28);
        assert_eq!(t.index_to_units[N_BUCKETS - 1], 128);

        // 1-unit requests resolve to bucket 0; 2 → 1; 5,6 → 4 (the 6-unit bucket).
        assert_eq!(t.units_to_index[0], 0);
        assert_eq!(t.units_to_index[1], 1);
        assert_eq!(t.units_to_index[4], 4); // 5 units
        assert_eq!(t.units_to_index[5], 4); // 6 units (same bucket as 5)
    }

    #[test]
    fn suballoc_alloc_until_exhaustion() {
        // Pool needs the text reservation (1/8) plus units area. 16 units
        // total gives 2-unit text region + 14 units of headroom for bumping.
        let mut s = Suballocator::default();
        s.reset(16 * ALLOC_UNIT_BYTES);
        // 14 units between lo_bump and hi_bump → can hand out 7 × 2-unit blocks.
        for _ in 0..7 {
            assert!(s.alloc(2, AllocSide::Lo, 0).is_some());
        }
        assert!(s.alloc(2, AllocSide::Lo, 0).is_none()); // pool full
    }

    #[test]
    fn suballoc_reuses_freed_bucket() {
        let mut s = Suballocator::default();
        s.reset(16 * ALLOC_UNIT_BYTES);
        let offsets: Vec<u32> = (0..7)
            .map(|_| s.alloc(2, AllocSide::Lo, 0).expect("alloc"))
            .collect();
        assert!(s.alloc(2, AllocSide::Lo, 0).is_none()); // full
        s.free(offsets[0], 2); // freed → bucket 1 has 1 slot
        assert!(s.alloc(2, AllocSide::Lo, 0).is_some()); // reuses freed slot
        assert!(s.alloc(2, AllocSide::Lo, 0).is_none()); // truly full again
    }

    #[test]
    fn suballoc_uninitialised_pool_acts_unbounded() {
        let mut s = Suballocator::default();
        // pool_bytes=0; the suballoc shouldn't reject any allocation since the
        // model hasn't been told its budget yet.
        for _ in 0..1000 {
            assert!(s.alloc(2, AllocSide::Lo, 0).is_some());
        }
        assert!(s.text_has_room(usize::MAX / 2));
    }

    // Spec §4.3: free blocks that are address-adjacent should merge into a
    // larger run during glue, and that merged run should be visible in the
    // bucket whose size matches.
    #[test]
    fn glue_merges_adjacent_free_blocks() {
        let mut s = Suballocator::default();
        s.reset(16 * ALLOC_UNIT_BYTES);
        // Allocate 4 consecutive 1-unit blocks from the Lo side. They will
        // be at offsets [units_start, units_start+1, units_start+2,
        // units_start+3], i.e. adjacent.
        let a = s.alloc(1, AllocSide::Lo, 0).unwrap();
        let b = s.alloc(1, AllocSide::Lo, 0).unwrap();
        let c = s.alloc(1, AllocSide::Lo, 0).unwrap();
        let d = s.alloc(1, AllocSide::Lo, 0).unwrap();
        assert_eq!(b, a + 1);
        assert_eq!(c, a + 2);
        assert_eq!(d, a + 3);
        // Free them — bucket 0 now has 4 entries, bucket 3 (size 4) has none.
        s.free(a, 1);
        s.free(b, 1);
        s.free(c, 1);
        s.free(d, 1);
        assert_eq!(s.free_lists[0].len(), 4);
        assert!(s.free_lists[3].is_empty());
        s.glue();
        // After gluing, the 4-adjacent run becomes one 4-unit block in bucket 3.
        assert!(s.free_lists[0].is_empty());
        assert_eq!(s.free_lists[3].len(), 1);
        // And we can satisfy a 4-unit request from the merged block.
        assert_eq!(s.alloc(4, AllocSide::Lo, 0), Some(a));
    }

    // Non-adjacent free blocks shouldn't merge even though their bucket sizes
    // are compatible.
    #[test]
    fn glue_keeps_non_adjacent_blocks_separate() {
        let mut s = Suballocator::default();
        s.reset(16 * ALLOC_UNIT_BYTES);
        // Two 1-unit blocks separated by a still-live 2-unit allocation.
        let a = s.alloc(1, AllocSide::Lo, 0).unwrap();
        let _live = s.alloc(2, AllocSide::Lo, 0).unwrap();
        let b = s.alloc(1, AllocSide::Lo, 0).unwrap();
        s.free(a, 1);
        s.free(b, 1);
        s.glue();
        // Still two separate 1-unit free entries; no merged 2-unit block.
        assert_eq!(s.free_lists[0].len(), 2);
    }

    // A merged run larger than the biggest bucket (128 units) should be
    // chunked into multiple bucket-sized pieces during redistribution.
    #[test]
    fn glue_splits_oversized_run_into_buckets() {
        // 256-unit pool (text=32, bumpable=224). Adjacent 64-unit + 64-unit
        // blocks combine to 128, which is the biggest bucket — exactly one
        // entry. Push it to 192 and we get a 128 + a 64 (closest bucket).
        let mut s = Suballocator::default();
        s.reset(256 * ALLOC_UNIT_BYTES);
        // Three adjacent 64-unit allocs → run of 192 units after freeing.
        // 64 is bucket 18 in the spec table.
        let bucket_64 = Suballocator::bucket_for(64).unwrap();
        assert_eq!(Suballocator::bucket_units(bucket_64), 64);
        let a = s.alloc(64, AllocSide::Lo, 0).unwrap();
        let b = s.alloc(64, AllocSide::Lo, 0).unwrap();
        let c = s.alloc(64, AllocSide::Lo, 0).unwrap();
        assert_eq!(b, a + 64);
        assert_eq!(c, a + 128);
        s.free(a, 64);
        s.free(b, 64);
        s.free(c, 64);
        s.glue();
        // Should emit one 128-unit block + one 64-unit block.
        let bucket_128 = Suballocator::bucket_for(128).unwrap();
        assert_eq!(s.free_lists[bucket_128].len(), 1);
        assert_eq!(s.free_lists[bucket_64].len(), 1);
        // The 128 block starts at `a`, the 64 follows.
        assert_eq!(s.free_lists[bucket_128][0], a);
        assert_eq!(s.free_lists[bucket_64][0], a + 128);
    }

    // glue_count debounce: a single bump failure runs the glue pass exactly
    // once, then subsequent failures decrement instead of re-gluing.
    #[test]
    fn glue_count_debounces_subsequent_failures() {
        let mut s = Suballocator::default();
        s.reset(16 * ALLOC_UNIT_BYTES);
        // Fill the pool to capacity (14 bumpable units in a 16-unit pool;
        // text region is 2 units). All 14 calls succeed — glue_count stays 0.
        // Pass a text_len that occupies the entire 2-unit text region so
        // the fallback_bump path can't grab any more.
        let text_len_full = 2 * ALLOC_UNIT_BYTES;
        for _ in 0..14 {
            assert!(s.alloc(1, AllocSide::Lo, text_len_full).is_some());
        }
        assert_eq!(s.glue_count, 0);
        // 15th call fails: triggers glue (no-op, no free blocks) then
        // walks up larger buckets (none), then fallback_bump fails (text
        // already fills text region), and on the fallback path we
        // decrement glue_count once. Mirrors C's `p->GlueCount--` inside
        // the bump-fallback branch of AllocUnitsRare.
        assert!(s.alloc(1, AllocSide::Lo, text_len_full).is_none());
        assert_eq!(s.glue_count, GLUE_RESET - 1);
        // Subsequent failure decrements again (no re-glue because count > 0).
        assert!(s.alloc(1, AllocSide::Lo, text_len_full).is_none());
        assert_eq!(s.glue_count, GLUE_RESET - 2);
    }

    // After exhaustion, freeing two adjacent blocks lets a larger request
    // succeed via glue.
    #[test]
    fn glue_recovers_capacity_for_larger_request() {
        let mut s = Suballocator::default();
        s.reset(16 * ALLOC_UNIT_BYTES);
        // Fill the pool to capacity (14 × 1-unit) without triggering a
        // failure, so glue_count remains 0.
        let mut held = Vec::new();
        for _ in 0..14 {
            held.push(s.alloc(1, AllocSide::Lo, 0).unwrap());
        }
        // Free two adjacent 1-unit blocks at the bottom of the units region.
        s.free(held[0], 1);
        s.free(held[1], 1);
        // alloc(2) needs bucket 1 (size 2). Empty, bump exhausted, but
        // glue_count == 0 so glue fires, merging into one 2-unit block at
        // held[0]. Retry pop succeeds.
        assert_eq!(s.alloc(2, AllocSide::Lo, 0), Some(held[0]));
    }

    #[test]
    fn state_array_units_handles_boundaries() {
        assert_eq!(PpmdDecoder::state_array_units(0), 0);
        assert_eq!(PpmdDecoder::state_array_units(1), 0); // binary: inline
        assert_eq!(PpmdDecoder::state_array_units(2), 1);
        assert_eq!(PpmdDecoder::state_array_units(3), 2);
        assert_eq!(PpmdDecoder::state_array_units(4), 2);
        assert_eq!(PpmdDecoder::state_array_units(255), 128);
        assert_eq!(PpmdDecoder::state_array_units(256), 128);
    }

    struct Bytes<'a> {
        input: &'a [u8],
    }

    impl PpmdByteReader for Bytes<'_> {
        fn read_ppmd_byte(&mut self) -> Result<u8> {
            let Some((&byte, rest)) = self.input.split_first() else {
                return Err(Error::NeedMoreInput);
            };
            self.input = rest;
            Ok(byte)
        }
    }

    #[test]
    fn decode_init_rejects_truncated_range_header_without_panic() {
        let mut decoder = PpmdDecoder::new();
        let mut input = Bytes { input: &[0, 0] };
        let mut esc = 0;

        assert_eq!(
            decoder.decode_init(0x20 | 1, &mut input, &mut esc),
            Err(Error::NeedMoreInput)
        );
    }

    #[test]
    fn decode_init_rejects_reuse_before_model_allocation() {
        let mut decoder = PpmdDecoder::new();
        let mut input = Bytes {
            input: &[0, 0, 0, 0],
        };
        let mut esc = 0;

        assert_eq!(
            decoder.decode_init(0, &mut input, &mut esc),
            Err(Error::InvalidData("RAR PPMd block reuses missing model"))
        );
    }

    #[test]
    fn decode_init_accepts_max_wire_order_without_growing_unbounded_model() {
        let mut decoder = PpmdDecoder::new();
        let mut input = Bytes {
            input: &[0, 0, 0, 0, 0],
        };
        let mut esc = 0;

        decoder
            .decode_init(0x20 | 0x1f, &mut input, &mut esc)
            .unwrap();

        assert_eq!(decoder.max_order, 64);
        assert_eq!(decoder.num_stats(decoder.min_context), 256);
        // The whole model is the declared 1 MiB, allocated once.
        assert_eq!(decoder.heap.len(), 1024 * 1024);
        assert_eq!(decoder.heap.capacity(), 1024 * 1024);
    }

    #[test]
    fn encoder_rejects_orders_outside_model_bounds() {
        assert!(matches!(
            PpmdEncoder::new(1, 2, 1),
            Err(Error::InvalidData("RAR PPMd order is invalid"))
        ));
        assert!(matches!(
            PpmdEncoder::new(65, 2, 1),
            Err(Error::InvalidData("RAR PPMd order is invalid"))
        ));
    }

    #[test]
    fn encoder_rejects_dictionary_sizes_the_header_cannot_hold() {
        for mb in [0, 257] {
            assert!(matches!(
                PpmdEncoder::new(4, 2, mb),
                Err(Error::InvalidData("RAR PPMd dictionary size is invalid"))
            ));
        }
        assert!(PpmdEncoder::new(4, 2, 256).is_ok());
    }

    #[test]
    fn successor_round_trips() {
        assert_eq!(Successor::none().as_raw(), None);
        assert_eq!(Successor::none().as_context(), None);
        let last_pos = 256 * 1024 * 1024;
        assert_eq!(Successor::raw(0).as_raw(), Some(0));
        assert_eq!(Successor::raw(last_pos).as_raw(), Some(last_pos));
        assert_eq!(Successor::raw(last_pos).as_context(), None);
        assert_eq!(Successor::context(0).as_context(), Some(0));
        assert_eq!(Successor::context(last_pos).as_context(), Some(last_pos));
        assert_eq!(Successor::context(0).as_raw(), None);
    }

    #[test]
    fn range_decoder_rejects_zero_total_without_panic() {
        let mut decoder = RangeDecoder::new();
        let mut input = Bytes {
            input: &[0, 0, 0, 0],
        };
        decoder.init(&mut input).unwrap();

        assert_eq!(
            decoder.get_threshold(0),
            Err(Error::InvalidData("RAR PPMd frequency sum is zero"))
        );
    }

    fn state(symbol: u8, freq: u8) -> State {
        State {
            symbol,
            freq,
            successor: Successor::none(),
        }
    }

    // A model with a 1 MiB pool and its root context.
    fn small_model() -> PpmdDecoder {
        let mut decoder = PpmdDecoder::new();
        decoder.reset_pool(1024 * 1024);
        decoder.init_model(4);
        decoder
    }

    #[test]
    fn contexts_and_state_arrays_live_in_the_pool() {
        let mut decoder = small_model();
        let root = decoder.min_context;
        let c = decoder.new_binary_context(state(b'a', 3), root).unwrap();
        assert_eq!(decoder.num_stats(c), 1);
        assert_eq!(decoder.suffix(c), Some(root));
        for (i, symbol) in (b'b'..=b'z').enumerate() {
            assert!(decoder.add_state(c, state(symbol, i as u8)));
        }
        assert_eq!(decoder.num_stats(c), 26);
        assert_eq!(decoder.state_at(c, 0).symbol, b'a');
        assert_eq!(decoder.state_at(c, 0).freq, 3);
        for (i, symbol) in (b'b'..=b'z').enumerate() {
            assert_eq!(decoder.state_at(c, i + 1).symbol, symbol);
            assert_eq!(decoder.state_at(c, i + 1).freq, i as u8);
        }
        assert_eq!(decoder.find_symbol(c, b'q'), Some(16));
        // The root is untouched by the growth around it.
        assert_eq!(decoder.num_stats(root), 256);
        assert_eq!(decoder.suffix(root), None);
        assert_eq!(decoder.heap.len(), 1024 * 1024);
    }

    #[test]
    fn context_allocation_fails_when_the_pool_is_full() {
        let mut decoder = small_model();
        let root = decoder.min_context;
        let mut made = 0;
        while decoder.new_binary_context(state(b'a', 1), root).is_some() {
            made += 1;
        }
        // Everything but the text reservation and the root holds contexts.
        assert!(made > 1024 * 1024 * 7 / 8 / 12 - 200, "{made}");
        assert_eq!(decoder.heap.len(), 1024 * 1024);
    }

    #[test]
    fn make_esc_freq_rejects_invalid_masked_state_count() {
        let mut decoder = small_model();
        let root = decoder.min_context;
        let c = decoder.new_binary_context(state(b'a', 1), root).unwrap();
        assert!(decoder.add_state(c, state(b'b', 1)));
        decoder.set_summ_freq(c, 2);
        decoder.min_context = c;

        assert!(matches!(
            decoder.make_esc_freq(2),
            Err(Error::InvalidData("RAR PPMd masked-state count is invalid"))
        ));
    }

    #[test]
    fn update_model_rejects_invalid_frequency_arithmetic() {
        let mut decoder = small_model();
        let root = decoder.min_context;
        let c = decoder.new_binary_context(state(b'a', 10), root).unwrap();
        assert!(decoder.add_state(c, state(b'b', 1)));
        decoder.set_summ_freq(c, 1);
        decoder.min_context = c;
        decoder.max_context = root;
        decoder.found_state = StateRef {
            context: c,
            index: 0,
        };
        decoder.order_fall = 1;

        assert!(matches!(
            decoder.update_model(),
            Err(Error::InvalidData("RAR PPMd model frequency is invalid"))
        ));
    }

    #[test]
    fn update_paths_reject_invalid_state_reference_without_panic() {
        let mut decoder = small_model();
        decoder.found_state = StateRef {
            context: 10_000_000,
            index: 0,
        };

        assert_eq!(
            decoder.update1_0(),
            Err(Error::InvalidData("RAR PPMd state reference is invalid"))
        );

        decoder.found_state = StateRef {
            context: decoder.max_context,
            index: 999,
        };
        assert_eq!(
            decoder.update_bin(),
            Err(Error::InvalidData("RAR PPMd state reference is invalid"))
        );
    }

    // Deterministic text with little repetition, so a 1 MiB model runs out
    // of room and restarts.
    fn restart_text(n: usize) -> Vec<u8> {
        let mut x = 0x2545_f491_4f6c_dd1du64;
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let word_len = 1 + (x % 9) as usize;
            for i in 0..word_len {
                out.push(b'a' + ((x >> (8 + 5 * i)) % 26) as u8);
            }
            out.push(b' ');
        }
        out.truncate(n);
        out
    }

    // Pins the model's statistics, restart timing and coding: the packed
    // stream must stay byte-identical to what the pre-compaction model
    // (82fa74c) produced.
    #[test]
    fn encoding_through_a_restart_matches_the_reference_stream() {
        const ORDER: usize = 8;
        const ESC: u8 = 2;
        let data = restart_text(512 * 1024);
        let mut encoder = PpmdEncoder::new(ORDER, ESC, 1).unwrap();
        let mut restarts = 0;
        let mut text_len = 0;
        for &byte in &data {
            encoder.encode_literal(byte).unwrap();
            // The text area only shrinks when the model restarts.
            if encoder.model.text_len < text_len {
                restarts += 1;
            }
            text_len = encoder.model.text_len;
        }
        let (packed, _) = encoder.finish_keeping_model().unwrap();
        assert!(restarts >= 1, "the model never restarted");
        assert_eq!(
            (packed.len(), crate::crc32::crc32(&packed)),
            (306_285, 0xbbdb_72ef)
        );

        let mut wire = vec![0u8]; // dictionary: 1 MiB
        wire.extend_from_slice(&packed);
        let mut input = Bytes { input: &wire };
        let mut decoder = PpmdDecoder::new();
        let mut esc = ESC;
        decoder
            .decode_init(0x20 | (ORDER as u8 - 1), &mut input, &mut esc)
            .unwrap();
        for (i, &expected) in data.iter().enumerate() {
            assert_eq!(
                decoder.decode_symbol(&mut input).unwrap(),
                Some(expected),
                "byte {i}"
            );
        }
        assert_eq!(decoder.decode_symbol(&mut input).unwrap(), Some(ESC));
        assert_eq!(decoder.decode_symbol(&mut input).unwrap(), Some(2));
    }
}
