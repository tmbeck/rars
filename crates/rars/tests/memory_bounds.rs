//! Peak-heap bounds for extraction. Archives are generated once by rars'
//! own writers into CARGO_TARGET_TMPDIR (release-mode generation takes
//! seconds; debug takes minutes, so every test here is release-only).
//! Archives are parsed before `measure` and read back file-backed, so
//! neither their bytes nor their parsed headers count against a bound: only
//! what extraction itself allocates does.
//!
//!     cargo test -p rars --release --test memory_bounds -- --test-threads=1

use rars_test_alloc::{measure, Counting, Usage};
use std::cell::Cell;
use std::io::Write;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Mutex;

#[global_allocator]
static ALLOC: Counting = Counting;

const MIB: usize = 1024 * 1024;

/// Allocation counts are process-wide: one test at a time, generation included.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[test]
#[cfg_attr(debug_assertions, ignore = "release only")]
fn the_allocator_counts_a_live_buffer() {
    let _g = serial();
    let (_, u) = measure(|| {
        let v = vec![1u8; 8 * MIB];
        std::hint::black_box(&v);
    });
    assert!(u.peak >= 8 * MIB && u.peak < 9 * MIB, "{u:?}");
    assert!(u.total >= 8 * MIB, "{u:?}");
}

/// 4 bits of entropy per byte: packs to ~57 %, so packed sizes are large
/// enough to measure, and no writer falls back to storing it.
fn nibble_text(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            b"ACGTacgtNnRrYyKk"[(x >> 60) as usize]
        })
        .collect()
}

/// The archive (or volumes) `name`, built by `build` on first use. Bump the
/// `vN` in a name whenever its builder changes.
fn cached(name: &str, build: impl FnOnce() -> Vec<Vec<u8>>) -> Vec<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("memory_bounds");
    std::fs::create_dir_all(&dir).unwrap();
    let first = dir.join(format!("{name}.part1.rar"));
    if !first.exists() {
        let volumes = build();
        for (i, v) in volumes.iter().enumerate() {
            let tmp = dir.join(format!("{name}.part{}.rar.tmp", i + 1));
            std::fs::write(&tmp, v).unwrap();
            std::fs::rename(&tmp, dir.join(format!("{name}.part{}.rar", i + 1))).unwrap();
        }
    }
    (1..)
        .map(|i| dir.join(format!("{name}.part{i}.rar")))
        .take_while(|p| p.exists())
        .collect()
}

/// Parse every part, outside `measure`: opening reads up to 8 MiB to look
/// for an SFX stub (until the SFX task) and keeps the headers.
fn open_all(paths: &[PathBuf]) -> Vec<rars::Archive> {
    paths
        .iter()
        .map(|p| rars::ArchiveReader::read_path(p).unwrap())
        .collect()
}

/// Counts what extraction writes and keeps none of it.
struct Count(Rc<Cell<u64>>);

impl Write for Count {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.set(self.0.get() + b.len() as u64);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Extract every member of `archives` (one archive, or a volume set in
/// order), returning the bytes written.
fn extract_all(
    archives: &[rars::Archive],
    options: rars::ArchiveReadOptions<'_>,
) -> rars::Result<u64> {
    let n = Rc::new(Cell::new(0));
    let open = |_: &rars::ExtractedEntryMeta| -> rars::Result<Box<dyn Write>> {
        Ok(Box::new(Count(Rc::clone(&n))))
    };
    match archives {
        [one] => one.extract_to_with_options(options, open)?,
        _ => rars::extract_volumes_to_with_options(archives, options, open)?,
    }
    Ok(n.get())
}

fn check(label: &str, usage: Usage, bound: usize) {
    eprintln!(
        "{label}: peak {} KiB, total {} KiB",
        usage.peak / 1024,
        usage.total / 1024
    );
    assert!(
        usage.peak <= bound,
        "{label}: peak {} bytes over the {bound}-byte bound",
        usage.peak
    );
}

fn solid_rar5(count: usize, dictionary: u64) -> Vec<Vec<u8>> {
    let mut features = rars::FeatureSet::default();
    features.solid = true;
    let opts = rars::rar50::WriterOptions::new(rars::ArchiveVersion::Rar50, features)
        .with_compression_level(1)
        .with_dictionary_size(dictionary);
    let entries: Vec<_> = (0..count)
        .map(|i| {
            rars::rar50::ArchiveEntry::new(
                format!("f{i}").into_bytes(),
                rars::EntrySource::from_bytes(nibble_text(5000 + i % 97, i as u64)),
            )
        })
        .collect();
    vec![rars::rar50::Rar50Writer::new(opts)
        .entries(entries)
        .finish()
        .unwrap()]
}

/// F3: each member used to clone the decoder (its whole history), scan the
/// history for zeros and move it between Vec and VecDeque, so time and
/// allocation grew with members × history.
#[test]
#[cfg_attr(debug_assertions, ignore = "release only")]
fn solid_rar5_scales_linearly() {
    let _g = serial();
    let small = open_all(&cached("solid-rar5-1000-v1", || solid_rar5(1000, 8 << 20)));
    let large = open_all(&cached("solid-rar5-4000-v1", || solid_rar5(4000, 8 << 20)));
    // Force the streaming path, which Task 6 makes the only one.
    let opts = || rars::ArchiveReadOptions::new().with_rar50_buffered_decode_limit(0);
    let run = |a: &[rars::Archive]| {
        let t = std::time::Instant::now();
        let (n, u) = measure(|| extract_all(a, opts()).unwrap());
        (n, u, t.elapsed())
    };
    let (_, us, _) = run(&small);
    let (_, ul, _) = run(&large);
    // The 8 MiB window, one compressed block (the writer's are at most
    // 1 MiB), 64 KiB pending and per-member transients. The parsed headers
    // (2.5 MiB for 4000 members) are live before `measure` and not counted.
    check("solid 1000", us, usize::MAX);
    check("solid 4000", ul, 8 * MIB + 2 * MIB);
    let ratio = ul.total as f64 / us.total as f64;
    assert!(ratio <= 4.5, "allocation volume ×{ratio:.1} for ×4 members");
    // Wide margin: ×4 work; the quadratic code measured ×18. Local gate
    // only, never a shared CI runner.
    let best = |a: &[rars::Archive]| (0..3).map(|_| run(a).2).min().unwrap();
    let (ts, tl) = (best(&small), best(&large));
    eprintln!("solid 1000: {ts:?}, solid 4000: {tl:?}");
    let time = tl.as_secs_f64() / ts.as_secs_f64();
    assert!(time <= 6.0, "time ×{time:.1} for ×4 members");
}

/// Memory the writer may use to build a test archive. The default (256 MiB)
/// refuses an explicitly filtered 64 MiB member and a 32 MiB dictionary.
fn writer_resources() -> rars::WriterResources {
    rars::WriterResources::new(2 << 30)
}

fn rar5_member(size: usize, dictionary: u64, policy: rars::rar50::FilterPolicy) -> Vec<Vec<u8>> {
    let opts =
        rars::rar50::WriterOptions::new(rars::ArchiveVersion::Rar50, rars::FeatureSet::default())
            .with_compression_level(1)
            .with_dictionary_size(dictionary);
    let entry = rars::rar50::ArchiveEntry::new(
        b"m".to_vec(),
        rars::EntrySource::from_bytes(nibble_text(size, 7)),
    );
    let mut out = Vec::new();
    rars::rar50::Rar50Writer::new(opts)
        .filter_policy(policy)
        .entry(entry)
        .write_to(&mut out, &writer_resources())
        .unwrap();
    vec![out]
}

/// F5: a filtered member above the buffered limit failed; below it, it was
/// buffered whole. Streaming holds at most one filter block.
#[test]
#[cfg_attr(debug_assertions, ignore = "release only")]
fn rar5_filtered_member_streams_in_bounded_memory() {
    let _g = serial();
    let a = open_all(&cached("rar5-64m-e8-v1", || {
        rar5_member(
            64 * MIB,
            1 << 20,
            rars::rar50::FilterPolicy::explicit(rars::rar50::FilterKind::E8),
        )
    }));
    let opts = rars::ArchiveReadOptions::new().with_rar50_buffered_decode_limit(0);
    let (n, u) = measure(|| extract_all(&a, opts).unwrap());
    assert_eq!(n, 64 * MIB as u64);
    check("rar5 e8 64 MiB, 1 MiB dict", u, 8 * MIB);
}

fn default_opts() -> rars::ArchiveReadOptions<'static> {
    rars::ArchiveReadOptions::new()
}

/// F4: members up to 512 MiB were decoded into a Vec (plus packed data, a
/// filtered copy and a history copy): ~2.8x the member.
#[test]
#[cfg_attr(debug_assertions, ignore = "release only")]
fn rar5_member_streams_by_default() {
    let _g = serial();
    let a = open_all(&cached("rar5-64m-v1", || {
        rar5_member(64 * MIB, 1 << 20, Default::default())
    }));
    let (n, u) = measure(|| extract_all(&a, default_opts()).unwrap());
    assert_eq!(n, 64 * MIB as u64);
    check("rar5 64 MiB, 1 MiB dict", u, 8 * MIB);
}

/// F1: a member split across volumes was decoded into one Vec, no limit.
#[test]
#[cfg_attr(debug_assertions, ignore = "release only")]
fn rar5_split_member_streams() {
    let _g = serial();
    let p = cached("rar5-64m-4vol-v1", || {
        let opts = rars::rar50::WriterOptions::new(
            rars::ArchiveVersion::Rar50,
            rars::FeatureSet::default(),
        )
        .with_compression_level(1)
        .with_dictionary_size(1 << 20);
        let entry = rars::rar50::ArchiveEntry::new(
            b"m".to_vec(),
            rars::EntrySource::from_bytes(nibble_text(64 * MIB, 9)),
        );
        let mut sink = rars::rar50::CollectedVolumes::new();
        rars::rar50::write_streaming_volumes_to(
            &[entry],
            opts,
            rars::rar50::ArchiveExtras::default(),
            12 * MIB as u64,
            &mut sink,
            &writer_resources(),
        )
        .unwrap();
        sink.take()
    });
    assert!(p.len() >= 3, "a set of several volumes");
    let a = open_all(&p);
    let (n, u) = measure(|| extract_all(&a, default_opts()).unwrap());
    assert_eq!(n, 64 * MIB as u64);
    check("rar5 64 MiB over volumes", u, 8 * MIB);
}

/// F3: a large dictionary was cloned before every member, doubling the peak.
#[test]
#[cfg_attr(debug_assertions, ignore = "release only")]
fn rar5_large_dictionary_is_held_once() {
    let _g = serial();
    let a = open_all(&cached("rar5-solid-32m-dict-v1", || {
        let mut features = rars::FeatureSet::default();
        features.solid = true;
        let opts = rars::rar50::WriterOptions::new(rars::ArchiveVersion::Rar50, features)
            .with_compression_level(1)
            .with_dictionary_size(32 << 20);
        let entries = vec![
            rars::rar50::ArchiveEntry::new(
                b"a".to_vec(),
                rars::EntrySource::from_bytes(nibble_text(48 * MIB, 3)),
            ),
            rars::rar50::ArchiveEntry::new(
                b"b".to_vec(),
                rars::EntrySource::from_bytes(nibble_text(MIB, 4)),
            ),
        ];
        let mut out = Vec::new();
        rars::rar50::Rar50Writer::new(opts)
            .entries(entries)
            .write_to(&mut out, &writer_resources())
            .unwrap();
        vec![out]
    }));
    let (_, u) = measure(|| extract_all(&a, default_opts()).unwrap());
    // The 32 MiB window, one block (≤ 1 MiB) and slack.
    check("rar5 solid, 32 MiB dict", u, 32 * MIB + 8 * MIB);
}

fn legacy(version: rars::ArchiveVersion, size: usize, volume: Option<usize>) -> Vec<Vec<u8>> {
    let mut b = rars::Builder::new(version).compression_level(Some(1));
    b.add_bytes(b"m".to_vec(), nibble_text(size, 11), None, None)
        .unwrap();
    match volume {
        None => vec![b.to_bytes().unwrap()],
        Some(v) => b.volume_size(Some(v)).build_volumes(None).unwrap(),
    }
}

/// F2: the whole packed member was read into memory and copied again into
/// the bit reader (~2x packed).
#[test]
#[cfg_attr(debug_assertions, ignore = "release only")]
fn rar29_member_reads_input_incrementally() {
    let _g = serial();
    let a = open_all(&cached("rar29-32m-v1", || {
        legacy(rars::ArchiveVersion::Rar29, 32 * MIB, None)
    }));
    let (n, u) = measure(|| extract_all(&a, default_opts()).unwrap());
    assert_eq!(n, 32 * MIB as u64);
    check("rar29 32 MiB", u, 12 * MIB);
}

/// F2: a split RAR 2.9 member read every volume's packed data at once.
#[test]
#[cfg_attr(debug_assertions, ignore = "release only")]
fn rar29_split_member_reads_input_incrementally() {
    let _g = serial();
    let p = cached("rar29-32m-4vol-v1", || {
        legacy(rars::ArchiveVersion::Rar29, 32 * MIB, Some(6 * MIB))
    });
    assert!(p.len() >= 3);
    let a = open_all(&p);
    let (n, u) = measure(|| extract_all(&a, default_opts()).unwrap());
    assert_eq!(n, 32 * MIB as u64);
    check("rar29 32 MiB over volumes", u, 12 * MIB);
}
