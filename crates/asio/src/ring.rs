//! Wait-free single-producer single-consumer (SPSC) ring buffer for
//! interleaved stereo `f32` audio samples.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Fixed-capacity wait-free SPSC ring buffer.
///
/// Capacity is in `f32` elements (for stereo frames, `frames * 2`).
pub struct SpscRing {
    buf: Box<[UnsafeCell<f32>]>,
    mask: usize,
    head: AtomicUsize, // read cursor (consumer)
    tail: AtomicUsize, // write cursor (producer)
}

// SAFETY: SPSC contract is enforced by single producer and single consumer callers.
unsafe impl Send for SpscRing {}
// SAFETY: `head` is only updated by consumer, `tail` is only updated by producer;
// disjoint buffer slots are synchronized via Acquire/Release on `head` and `tail`.
unsafe impl Sync for SpscRing {}

impl SpscRing {
    /// Creates a ring buffer with at least `min_samples` capacity (rounded up
    /// to a power of two, minimum 1024).
    #[must_use]
    pub fn new(min_samples: usize) -> Self {
        let cap = min_samples.max(1024).next_power_of_two();
        let buf: Vec<UnsafeCell<f32>> = (0..cap).map(|_| UnsafeCell::new(0.0)).collect();
        Self {
            buf: buf.into_boxed_slice(),
            mask: cap - 1,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    #[must_use]
    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        let tail = self.tail.load(Ordering::Acquire);
        let head = self.head.load(Ordering::Acquire);
        tail.wrapping_sub(head)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[must_use]
    pub fn available_write(&self) -> usize {
        self.capacity().saturating_sub(self.len())
    }

    /// Resets cursors to empty. Only call while consumer is paused/stopped
    /// or synchronized with the consumer.
    pub fn clear(&self) {
        let tail = self.tail.load(Ordering::Acquire);
        self.head.store(tail, Ordering::Release);
    }

    /// Pushes as many samples as fit from `src`, returning the count written.
    /// Must only be called from the single producer thread.
    pub fn push(&self, src: &[f32]) -> usize {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        let free = self.capacity() - tail.wrapping_sub(head);
        let n = src.len().min(free);
        if n == 0 {
            return 0;
        }
        let start = tail & self.mask;
        let first = n.min(self.capacity() - start);
        for (i, &sample) in src[..first].iter().enumerate() {
            // SAFETY: slot `(start + i)` is strictly in the free region `[tail, head + cap)`.
            unsafe { *self.buf[start + i].get() = sample };
        }
        let second = n - first;
        for (i, &sample) in src[first..first + second].iter().enumerate() {
            // SAFETY: wrapped portion at `[0..second)` is also in the free region.
            unsafe { *self.buf[i].get() = sample };
        }
        self.tail.store(tail.wrapping_add(n), Ordering::Release);
        n
    }

    /// Pops up to `dst.len()` samples into `dst`, returning the count read.
    /// Must only be called from the single consumer thread.
    pub fn pop(&self, dst: &mut [f32]) -> usize {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let avail = tail.wrapping_sub(head);
        let n = dst.len().min(avail);
        if n == 0 {
            return 0;
        }
        let start = head & self.mask;
        let first = n.min(self.capacity() - start);
        for (i, out) in dst[..first].iter_mut().enumerate() {
            // SAFETY: slot `(start + i)` is strictly in `[head, tail)` written prior to Release.
            *out = unsafe { *self.buf[start + i].get() };
        }
        let second = n - first;
        for (i, out) in dst[first..first + second].iter_mut().enumerate() {
            // SAFETY: wrapped portion at `[0..second)` is in `[head, tail)`.
            *out = unsafe { *self.buf[i].get() };
        }
        self.head.store(head.wrapping_add(n), Ordering::Release);
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_pop_wraparound() {
        let ring = SpscRing::new(1024);
        assert_eq!(ring.capacity(), 1024);
        let a: Vec<f32> = (0..700).map(|i| i as f32).collect();
        assert_eq!(ring.push(&a), 700);
        let mut out = vec![0.0; 500];
        assert_eq!(ring.pop(&mut out), 500);
        assert_eq!(out, &a[..500]);

        let b: Vec<f32> = (1000..1800).map(|i| i as f32).collect();
        assert_eq!(ring.push(&b), 800);
        assert_eq!(ring.len(), 1000);

        let mut rest = vec![0.0; 1000];
        assert_eq!(ring.pop(&mut rest), 1000);
        assert_eq!(&rest[..200], &a[500..]);
        assert_eq!(&rest[200..], &b[..]);
        assert_eq!(ring.len(), 0);
    }
}
