use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Persistent key-value storage (resume snapshot, resume_invalid block, client id).
/// Implementations swallow their own errors: reporting keeps working in memory.
pub trait KeyValueStorage: Send {
    fn get(&self, key: &str) -> Option<String>;
    fn set(&mut self, key: &str, value: &str);
    fn remove(&mut self, key: &str);
}

/// In-memory storage. Clones share the same map.
#[derive(Clone, Debug, Default)]
pub struct MemoryStorage {
    map: Arc<Mutex<HashMap<String, String>>>,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> HashMap<String, String> {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl KeyValueStorage for MemoryStorage {
    fn get(&self, key: &str) -> Option<String> {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned()
    }

    fn set(&mut self, key: &str, value: &str) {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_owned(), value.to_owned());
    }

    fn remove(&mut self, key: &str) {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key);
    }
}

pub type TimerId = u64;

/// One-shot timers. When a timer fires the driver calls
/// [`crate::PlaybackSession::on_timer`] with its id.
pub trait Scheduler: Send {
    fn set_timeout(&mut self, delay_millis: u64) -> TimerId;
    fn clear_timeout(&mut self, id: TimerId);
}

/// Live position / duration of the current track, read for heartbeats and
/// for events that carry no explicit position.
pub trait Probe: Send + Sync {
    fn position_millis(&self) -> u64;
    fn duration_millis(&self) -> u64;
}

/// A [`Probe`] backed by shared atomics. Clones share the same values.
#[derive(Clone, Debug, Default)]
pub struct LiveProbe {
    position: Arc<AtomicU64>,
    duration: Arc<AtomicU64>,
}

impl LiveProbe {
    pub fn set_position_millis(&self, value: u64) {
        self.position.store(value, Ordering::Relaxed);
    }

    pub fn set_duration_millis(&self, value: u64) {
        self.duration.store(value, Ordering::Relaxed);
    }
}

impl Probe for LiveProbe {
    fn position_millis(&self) -> u64 {
        self.position.load(Ordering::Relaxed)
    }

    fn duration_millis(&self) -> u64 {
        self.duration.load(Ordering::Relaxed)
    }
}
