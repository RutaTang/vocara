//! A bounded, wait-free single-producer / single-consumer ring buffer (Lamport style).
//!
//! It carries data between the engine's threads: coalesced parameter commands (control →
//! audio), decoded PCM, one ring per source (feeder → audio), and captured input (device →
//! engine). Push/pop are
//! wait-free and allocate nothing after construction, so they are safe on the audio thread.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct Inner<T> {
    buf: Box<[UnsafeCell<T>]>,
    mask: usize,
    // Monotonic indices; live count = tail - head. Each on its own cache line: side by
    // side, every per-frame push and pop moved the one line between the two threads.
    head: CacheLine<AtomicUsize>,
    tail: CacheLine<AtomicUsize>,
}

/// A value alone on its cache line (128 bytes: Apple silicon's, twice x86's).
#[repr(align(128))]
struct CacheLine<T>(T);

impl<T> std::ops::Deref for CacheLine<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

// SAFETY: access is disciplined SPSC — only the producer writes `tail` and the slot at
// `tail`, only the consumer writes `head` and reads the slot at `head`, with release/acquire
// fences ordering the data write before the index publish. The one exception, the producer
// taking back and rewriting unconsumed slots, is `Producer::retract`'s, under its own
// contract (the consumer kept clear of them by a protocol on top).
unsafe impl<T: Send> Sync for Inner<T> {}

pub struct Producer<T> {
    inner: Arc<Inner<T>>,
}
pub struct Consumer<T> {
    inner: Arc<Inner<T>>,
}

unsafe impl<T: Send> Send for Producer<T> {}
unsafe impl<T: Send> Send for Consumer<T> {}

/// Create an SPSC ring with capacity rounded up to a power of two (min 2).
pub fn spsc<T: Copy + Default>(capacity: usize) -> (Producer<T>, Consumer<T>) {
    let cap = capacity.max(2).next_power_of_two();
    let buf = (0..cap)
        .map(|_| UnsafeCell::new(T::default()))
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let inner = Arc::new(Inner {
        buf,
        mask: cap - 1,
        head: CacheLine(AtomicUsize::new(0)),
        tail: CacheLine(AtomicUsize::new(0)),
    });
    (
        Producer {
            inner: inner.clone(),
        },
        Consumer { inner },
    )
}

impl<T: Copy> Producer<T> {
    /// Push one element. Returns false if the ring is full (caller applies back-pressure;
    /// never blocks).
    #[inline]
    pub fn push(&mut self, v: T) -> bool {
        let inner = &*self.inner;
        let tail = inner.tail.load(Ordering::Relaxed);
        let head = inner.head.load(Ordering::Acquire);
        if tail.wrapping_sub(head) >= inner.buf.len() {
            return false;
        }
        // SAFETY: SPSC — we exclusively own this slot until we publish the new tail.
        unsafe {
            *inner.buf[tail & inner.mask].get() = v;
        }
        inner.tail.store(tail.wrapping_add(1), Ordering::Release);
        true
    }

    /// Free slots currently available to push.
    #[inline]
    pub fn free(&self) -> usize {
        let inner = &*self.inner;
        let tail = inner.tail.load(Ordering::Relaxed);
        let head = inner.head.load(Ordering::Acquire);
        inner.buf.len() - tail.wrapping_sub(head)
    }

    pub fn capacity(&self) -> usize {
        self.inner.buf.len()
    }

    /// Index (monotonic, wrapping) of the next element the consumer will pop.
    #[inline]
    pub fn consumer_pos(&self) -> usize {
        self.inner.head.load(Ordering::Acquire)
    }

    /// Index (monotonic, wrapping) the next push lands at.
    #[inline]
    pub fn producer_pos(&self) -> usize {
        self.inner.tail.load(Ordering::Relaxed)
    }

    /// Take back every element pushed at or after index `to` that the consumer has not popped:
    /// the next push lands at `to` again. Returns false, changing nothing, unless `to` lies in
    /// the unconsumed span (between [`consumer_pos`](Self::consumer_pos) and
    /// [`producer_pos`](Self::producer_pos)).
    ///
    /// # Safety
    /// The consumer must not read the slots at `to` and beyond while they are taken back and
    /// rewritten: from before this call until the elements pushed in their place are
    /// published, it must neither pop nor peek at any index at or past `to`. The ring cannot
    /// see that; a protocol on top of it must guarantee it (the streaming feeder's pop limit,
    /// see `feed::StreamCtrl::pop_limit`).
    pub unsafe fn retract(&mut self, to: usize) -> bool {
        let inner = &*self.inner;
        let tail = inner.tail.load(Ordering::Relaxed);
        let head = inner.head.load(Ordering::Acquire);
        if to.wrapping_sub(head) > tail.wrapping_sub(head) {
            return false;
        }
        inner.tail.store(to, Ordering::Release);
        true
    }
}

impl<T: Copy> Consumer<T> {
    /// Read the next element WITHOUT consuming it, or None if empty. SPSC:
    /// the slot stays exclusively readable until this side advances `head`.
    #[inline]
    pub fn peek(&self) -> Option<T> {
        let inner = &*self.inner;
        let head = inner.head.load(Ordering::Relaxed);
        let tail = inner.tail.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        // SAFETY: SPSC — the producer published this slot via the Release on `tail`.
        Some(unsafe { *inner.buf[head & inner.mask].get() })
    }

    /// Pop one element, or None if empty.
    #[inline]
    pub fn pop(&mut self) -> Option<T> {
        let inner = &*self.inner;
        let head = inner.head.load(Ordering::Relaxed);
        let tail = inner.tail.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        // SAFETY: SPSC — the producer published this slot via the Release on `tail`.
        let v = unsafe { *inner.buf[head & inner.mask].get() };
        inner.head.store(head.wrapping_add(1), Ordering::Release);
        Some(v)
    }

    /// Elements currently available to pop.
    #[inline]
    pub fn len(&self) -> usize {
        let inner = &*self.inner;
        let tail = inner.tail.load(Ordering::Acquire);
        let head = inner.head.load(Ordering::Relaxed);
        tail.wrapping_sub(head)
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Index (monotonic, wrapping) of the next element [`pop`](Self::pop) returns.
    #[inline]
    pub fn position(&self) -> usize {
        self.inner.head.load(Ordering::Relaxed)
    }
}
