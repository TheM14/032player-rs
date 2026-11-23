//! Wait-free single-producer single-consumer (SPSC) ring buffer for
//! interleaved stereo `f32` audio samples.
//!
//! Ownership is split into `!Sync` [`SpscProducer`] and [`SpscConsumer`]
//! halves so the single-producer / single-consumer invariant is enforced by
//! the type system rather than caller discipline.

use std::cell::{Cell, UnsafeCell};
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Cache-line padded wrapper to prevent false sharing between `head` and `tail`.
#[repr(align(64))]
struct CachePadded<T>(T);

struct RingInner {
    buf: Box<[UnsafeCell<f32>]>,
    mask: usize,
    /// Read cursor, mutated exclusively by [`SpscConsumer`].
    head: CachePadded<AtomicUsize>,
    /// Write cursor, mutated exclusively by [`SpscProducer`].
    tail: CachePadded<AtomicUsize>,
}

// SAFETY: `head` is only written by the unique `SpscConsumer`, and `tail` is
// only written by the unique `SpscProducer`. Disjoint buffer slots are
// synchronized via Acquire/Release operations on `head` and `tail`.
unsafe impl Send for RingInner {}
// SAFETY: Shared across exactly one `SpscProducer` and one `SpscConsumer`
// (each `!Sync`), so no two threads ever concurrently access the same slot or
// write the same cursor.
unsafe impl Sync for RingInner {}

/// Producer half of a wait-free SPSC ring buffer (`Send`, `!Sync`).
pub struct SpscProducer {
    inner: Arc<RingInner>,
    _not_sync: PhantomData<Cell<()>>,
}

/// Consumer half of a wait-free SPSC ring buffer (`Send`, `!Sync`).
pub struct SpscConsumer {
    inner: Arc<RingInner>,
    _not_sync: PhantomData<Cell<()>>,
}

/// Creates a fixed-capacity wait-free SPSC ring buffer with at least
/// `min_samples` capacity (rounded up to a power of two, minimum 1024),
/// returning its producer and consumer halves.
#[must_use]
pub fn spsc_ring(min_samples: usize) -> (SpscProducer, SpscConsumer) {
    let cap = min_samples.max(1024).next_power_of_two();
    let buf: Vec<UnsafeCell<f32>> = (0..cap).map(|_| UnsafeCell::new(0.0)).collect();
    let inner = Arc::new(RingInner {
        buf: buf.into_boxed_slice(),
        mask: cap - 1,
        head: CachePadded(AtomicUsize::new(0)),
        tail: CachePadded(AtomicUsize::new(0)),
    });
    (
        SpscProducer {
            inner: inner.clone(),
            _not_sync: PhantomData,
        },
        SpscConsumer {
            inner,
            _not_sync: PhantomData,
        },
    )
}

impl SpscProducer {
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.inner.buf.len()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        let tail = self.inner.tail.0.load(Ordering::Relaxed);
        let head = self.inner.head.0.load(Ordering::Acquire);
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

    /// Pushes as many samples as fit from `src`, returning the count written.
    pub fn push(&mut self, src: &[f32]) -> usize {
        let cap = self.capacity();
        let tail = self.inner.tail.0.load(Ordering::Relaxed);
        let head = self.inner.head.0.load(Ordering::Acquire);
        let used = tail.wrapping_sub(head);
        let free = cap.saturating_sub(used);
        let n = src.len().min(free);
        if n == 0 {
            return 0;
        }
        let start = tail & self.inner.mask;
        let first = n.min(cap - start);
        for (i, &sample) in src[..first].iter().enumerate() {
            // SAFETY: slot `(start + i)` is strictly in the free region `[tail, head + cap)`
            // and this `&mut self` guarantees exclusive producer access.
            unsafe { *self.inner.buf[start + i].get() = sample };
        }
        let second = n - first;
        for (i, &sample) in src[first..first + second].iter().enumerate() {
            // SAFETY: wrapped portion at `[0..second)` is also in the free region.
            unsafe { *self.inner.buf[i].get() = sample };
        }
        self.inner
            .tail
            .0
            .store(tail.wrapping_add(n), Ordering::Release);
        n
    }
}

impl SpscConsumer {
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.inner.buf.len()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        let head = self.inner.head.0.load(Ordering::Relaxed);
        let tail = self.inner.tail.0.load(Ordering::Acquire);
        tail.wrapping_sub(head)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Discards all currently buffered samples by advancing `head` to `tail`.
    /// Safe because only the consumer mutates `head`.
    pub fn clear(&mut self) {
        let tail = self.inner.tail.0.load(Ordering::Acquire);
        self.inner.head.0.store(tail, Ordering::Release);
    }

    /// Pops up to `dst.len()` samples into `dst`, returning the count read.
    pub fn pop(&mut self, dst: &mut [f32]) -> usize {
        let cap = self.capacity();
        let head = self.inner.head.0.load(Ordering::Relaxed);
        let tail = self.inner.tail.0.load(Ordering::Acquire);
        let avail = tail.wrapping_sub(head);
        let n = dst.len().min(avail);
        if n == 0 {
            return 0;
        }
        let start = head & self.inner.mask;
        let first = n.min(cap - start);
        for (i, out) in dst[..first].iter_mut().enumerate() {
            // SAFETY: slot `(start + i)` is strictly in `[head, tail)` written prior to Release
            // and this `&mut self` guarantees exclusive consumer access.
            *out = unsafe { *self.inner.buf[start + i].get() };
        }
        let second = n - first;
        for (i, out) in dst[first..first + second].iter_mut().enumerate() {
            // SAFETY: wrapped portion at `[0..second)` is in `[head, tail)`.
            *out = unsafe { *self.inner.buf[i].get() };
        }
        self.inner
            .head
            .0
            .store(head.wrapping_add(n), Ordering::Release);
        n
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    fn assert_send<T: Send>() {}

    #[test]
    fn halves_are_send() {
        assert_send::<SpscProducer>();
        assert_send::<SpscConsumer>();
    }

    #[test]
    fn push_pop_wraparound() {
        let (mut prod, mut cons) = spsc_ring(1024);
        assert_eq!(prod.capacity(), 1024);
        assert_eq!(cons.capacity(), 1024);

        let a: Vec<f32> = (0..700).map(|i| i as f32).collect();
        assert_eq!(prod.push(&a), 700);
        let mut out = vec![0.0; 500];
        assert_eq!(cons.pop(&mut out), 500);
        assert_eq!(out, &a[..500]);

        let b: Vec<f32> = (1000..1800).map(|i| i as f32).collect();
        assert_eq!(prod.push(&b), 800);
        assert_eq!(prod.len(), 1000);
        assert_eq!(cons.len(), 1000);

        let mut rest = vec![0.0; 1000];
        assert_eq!(cons.pop(&mut rest), 1000);
        assert_eq!(&rest[..200], &a[500..]);
        assert_eq!(&rest[200..], &b[..]);
        assert_eq!(prod.len(), 0);
        assert!(cons.is_empty());
    }

    #[test]
    fn consumer_clear_empties_ring_safely() {
        let (mut prod, mut cons) = spsc_ring(1024);
        let a: Vec<f32> = (0..600).map(|i| i as f32).collect();
        assert_eq!(prod.push(&a), 600);
        cons.clear();
        assert!(cons.is_empty());
        assert_eq!(prod.available_write(), 1024);

        let b: Vec<f32> = (0..1024).map(|i| (i + 10_000) as f32).collect();
        assert_eq!(prod.push(&b), 1024);
        let mut out = vec![0.0; 1024];
        assert_eq!(cons.pop(&mut out), 1024);
        assert_eq!(out, b);
    }

    #[test]
    fn concurrent_spsc_preserves_order() {
        const TOTAL: usize = 200_000;
        let (mut prod, mut cons) = spsc_ring(2048);
        let producer = thread::spawn(move || {
            let mut sent = 0;
            let mut chunk = [0.0f32; 137];
            while sent < TOTAL {
                let n = chunk.len().min(TOTAL - sent);
                for (i, slot) in chunk[..n].iter_mut().enumerate() {
                    *slot = (sent + i) as f32;
                }
                let mut off = 0;
                while off < n {
                    let pushed = prod.push(&chunk[off..n]);
                    if pushed == 0 {
                        thread::yield_now();
                    } else {
                        off += pushed;
                    }
                }
                sent += n;
            }
        });

        let mut received = 0;
        let mut buf = [0.0f32; 99];
        while received < TOTAL {
            let got = cons.pop(&mut buf);
            if got == 0 {
                thread::yield_now();
                continue;
            }
            for &sample in &buf[..got] {
                assert_eq!(sample, received as f32, "mismatch at sample {received}");
                received += 1;
            }
        }
        producer.join().unwrap();
        assert!(cons.is_empty());
    }
}
