//! RCU graph cell: lock-free hot-swap of the live graph at a block boundary.
//!
//! The control thread compiles a new graph entirely off the audio thread and `publish`es it
//! with one atomic swap. The audio thread `load`s the raw pointer once per block — no
//! refcount, nothing to drop. The retired graph is handed to a reaper via an SPSC ring and
//! freed on a non-audio thread after a one-block grace epoch, so the audio thread never
//! performs an unbounded `free()`.

use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::graph::Graph;
use crate::ring::{spsc, Consumer, Producer};

pub struct GraphCell {
    ptr: AtomicPtr<Graph>,
}

impl GraphCell {
    pub fn new(initial: Box<Graph>) -> Self {
        GraphCell {
            ptr: AtomicPtr::new(Box::into_raw(initial)),
        }
    }

    /// Audio thread: the current graph pointer. Read once per block; do **not** cache the
    /// reference across blocks (the reaper's grace epoch only guarantees liveness for the
    /// current block).
    #[inline]
    pub fn load(&self) -> *mut Graph {
        self.ptr.load(Ordering::Acquire)
    }

    /// Control thread: install `next`, returning the now-retired raw pointer to hand to the
    /// reaper. Never freed here.
    #[must_use]
    pub fn publish(&self, next: Box<Graph>) -> *mut Graph {
        self.ptr.swap(Box::into_raw(next), Ordering::AcqRel)
    }
}

impl Drop for GraphCell {
    fn drop(&mut self) {
        let p = self.ptr.swap(ptr::null_mut(), Ordering::AcqRel);
        if !p.is_null() {
            // SAFETY: we own the only pointer at drop time.
            unsafe { drop(Box::from_raw(p)) };
        }
    }
}

// SAFETY: the pointer is only published by the control thread and loaded by the audio
// thread; ownership transfer of the retired graph goes through the reaper ring.
unsafe impl Send for GraphCell {}
unsafe impl Sync for GraphCell {}

/// A retired graph pointer in transit to the reaper. POD for the ring.
#[derive(Clone, Copy)]
pub struct RetiredPtr(pub *mut Graph);

impl Default for RetiredPtr {
    fn default() -> Self {
        RetiredPtr(ptr::null_mut())
    }
}

// SAFETY: the audio thread relinquishes all access before pushing; only the reaper drops it.
unsafe impl Send for RetiredPtr {}

pub fn reaper_channel(capacity: usize) -> (Producer<RetiredPtr>, Consumer<RetiredPtr>) {
    spsc(capacity)
}

/// Drop a retired graph pointer (called by the reaper on a non-audio thread, after the
/// grace epoch). No-op on null.
///
/// # Safety
/// `p` must have come from [`GraphCell::publish`] and must no longer be observable by the
/// audio thread (one block has elapsed since the swap).
pub unsafe fn reap(p: RetiredPtr) {
    if !p.0.is_null() {
        drop(Box::from_raw(p.0));
    }
}
