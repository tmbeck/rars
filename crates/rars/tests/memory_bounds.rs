//! Peak-heap bounds for extraction. Archives are generated once by rars'
//! own writers into CARGO_TARGET_TMPDIR (release-mode generation takes
//! seconds; debug takes minutes, so every test here is release-only).
//! Archives are parsed before `measure` and read back file-backed, so
//! neither their bytes nor their parsed headers count against a bound: only
//! what extraction itself allocates does.
//!
//!     cargo test -p rars --release --test memory_bounds -- --test-threads=1

use rars_test_alloc::{measure, Counting};
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
