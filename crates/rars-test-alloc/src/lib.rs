//! A global allocator for tests that bound memory use. Install it in a test
//! binary with `#[global_allocator] static A: rars_test_alloc::Counting =
//! rars_test_alloc::Counting;` and wrap the code under test in [`measure`].
//!
//! Counts are process-wide: run the measured code with nothing else
//! allocating (one test at a time).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

pub struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static TOTAL: AtomicUsize = AtomicUsize::new(0);

fn grew(n: usize) {
    let live = LIVE.fetch_add(n, Relaxed) + n;
    PEAK.fetch_max(live, Relaxed);
    TOTAL.fetch_add(n, Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc(layout);
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc_zeroed(layout);
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        System.dealloc(p, layout);
        LIVE.fetch_sub(layout.size(), Relaxed);
    }

    unsafe fn realloc(&self, p: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let q = System.realloc(p, layout, new_size);
        if !q.is_null() {
            LIVE.fetch_sub(layout.size(), Relaxed);
            grew(new_size);
        }
        q
    }
}

/// What the measured closure did to the heap.
#[derive(Debug, Clone, Copy)]
pub struct Usage {
    /// Highest live heap above what was live when `measure` started.
    pub peak: usize,
    /// Bytes allocated (a `realloc` counts its new size).
    pub total: usize,
}

pub fn measure<T>(f: impl FnOnce() -> T) -> (T, Usage) {
    let base = LIVE.load(Relaxed);
    PEAK.store(base, Relaxed);
    let total = TOTAL.load(Relaxed);
    let out = f();
    let usage = Usage {
        peak: PEAK.load(Relaxed).saturating_sub(base),
        total: TOTAL.load(Relaxed) - total,
    };
    (out, usage)
}
