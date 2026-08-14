//! A global allocator that counts the allocations made on one thread.
//!
//! The counter is thread-local rather than a global atomic because the test
//! harness and the benchmark runner allocate on their own threads: only the
//! allocations made on the thread under measurement are evidence about the
//! code under measurement.
//!
//! This file is included by both `tests/ranking_alloc.rs` and
//! `benches/ranking.rs` through `#[path]`, so the assertion and the reported
//! figure come from the same counter.

#![allow(dead_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

/// Forwards every request to the system allocator, counting the ones that
/// hand out memory.
pub struct CountingAllocator;

impl CountingAllocator {
    fn record() {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get().saturating_add(1)));
    }
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::record();
        System.alloc(layout)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::record();
        System.alloc_zeroed(layout)
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::record();
        System.realloc(pointer, layout, new_size)
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        System.dealloc(pointer, layout);
    }
}

/// Allocations made on the calling thread since it started.
#[must_use]
pub fn allocations() -> u64 {
    ALLOCATIONS.with(Cell::get)
}

/// Counts the allocations one call makes on the calling thread.
pub fn measure<T>(body: impl FnOnce() -> T) -> (T, u64) {
    let before = allocations();
    let value = body();
    (value, allocations() - before)
}
