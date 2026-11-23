#![cfg_attr(not(windows), allow(dead_code))]

//! Synchronized global callback slot and single-stream guard for ASIO.
//!
//! ASIO's C callback table has no `userdata` parameter, so at most one ASIO
//! output stream can be active per process and its state is reached through a
//! process-global slot. Using an `RwLock<Option<Arc<T>>>` ensures:
//!
//! - Real-time callbacks use non-blocking `try_read()` and never block on
//!   teardown, nor dereference freed memory.
//! - Teardown acquires `write()` to detach the slot, mathematically waiting
//!   for every in-flight callback to exit before DMA buffers are disposed.
//! - Every callback invocation is wrapped in `catch_unwind` so panics never
//!   unwind across `extern "C"` boundaries.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use crate::{AsioError, Result};

/// Ensures only one `AsioOutput` stream owns the process-global ASIO callbacks.
pub(crate) struct StreamSlot {
    occupied: AtomicBool,
}

impl StreamSlot {
    pub(crate) const fn new() -> Self {
        Self {
            occupied: AtomicBool::new(false),
        }
    }

    pub(crate) fn try_acquire(&'static self) -> Result<StreamSlotGuard> {
        self.occupied
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| StreamSlotGuard { slot: self })
            .map_err(|_| AsioError::AlreadyActive)
    }
}

pub(crate) struct StreamSlotGuard {
    slot: &'static StreamSlot,
}

impl Drop for StreamSlotGuard {
    fn drop(&mut self) {
        self.slot.occupied.store(false, Ordering::Release);
    }
}

/// Process-global holder for the active ASIO callback state.
pub(crate) struct CallbackSlot<T> {
    inner: RwLock<Option<Arc<T>>>,
}

impl<T> CallbackSlot<T> {
    pub(crate) const fn new() -> Self {
        Self {
            inner: RwLock::new(None),
        }
    }

    pub(crate) fn install(&self, state: Arc<T>) {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        *guard = Some(state);
    }

    /// Detaches the active state and blocks until all in-flight callbacks that
    /// entered `with_active` have exited.
    pub(crate) fn clear(&self) {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }

    /// Runs `f` on the currently active state if no teardown is in progress.
    /// Non-blocking (`try_read`) and panic-safe (`catch_unwind`).
    pub(crate) fn with_active<R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
        catch_unwind(AssertUnwindSafe(|| {
            let guard = self.inner.try_read().ok()?;
            let state = guard.as_ref()?;
            Some(f(state))
        }))
        .ok()
        .flatten()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;
    use std::sync::atomic::AtomicUsize;
    use std::thread;
    use std::time::Duration;

    use super::*;

    #[test]
    fn stream_slot_rejects_concurrent_acquisition() {
        static SLOT: StreamSlot = StreamSlot::new();
        let first = SLOT.try_acquire().expect("first acquire");
        assert!(matches!(SLOT.try_acquire(), Err(AsioError::AlreadyActive)));
        drop(first);
        let _second = SLOT.try_acquire().expect("acquire after drop");
    }

    #[test]
    fn clear_waits_for_in_flight_callback_and_leaves_unique_owner() {
        static SLOT: CallbackSlot<AtomicUsize> = CallbackSlot::new();
        let state = Arc::new(AtomicUsize::new(0));
        SLOT.install(Arc::clone(&state));

        let entered = Arc::new(Barrier::new(2));
        let entered_cb = Arc::clone(&entered);
        let cb_thread = thread::spawn(move || {
            let ran = SLOT.with_active(|counter| {
                counter.fetch_add(1, Ordering::AcqRel);
                entered_cb.wait();
                thread::sleep(Duration::from_millis(40));
                counter.fetch_add(1, Ordering::AcqRel);
                42
            });
            assert_eq!(ran, Some(42));
        });

        entered.wait();
        // `clear()` must wait for the in-flight callback to complete before returning.
        SLOT.clear();
        assert_eq!(state.load(Ordering::Acquire), 2);
        assert_eq!(Arc::strong_count(&state), 1);

        cb_thread.join().unwrap();
        // Late callbacks after `clear()` observe `None` and do not touch `state`.
        assert_eq!(SLOT.with_active(|c| c.load(Ordering::Relaxed)), None);
    }

    #[test]
    fn callback_panic_is_caught_and_does_not_poison_slot() {
        static SLOT: CallbackSlot<AtomicUsize> = CallbackSlot::new();
        let state = Arc::new(AtomicUsize::new(7));
        SLOT.install(Arc::clone(&state));

        let res: Option<usize> = SLOT.with_active(|_| -> usize {
            panic!("simulated callback panic");
        });
        assert_eq!(res, None);

        // Slot remains usable and cleanly detachable after a caught panic.
        assert_eq!(SLOT.with_active(|c| c.load(Ordering::Relaxed)), Some(7));
        SLOT.clear();
        assert_eq!(Arc::strong_count(&state), 1);
    }
}
