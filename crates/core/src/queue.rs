//! Play queue. M0: replace the queue from a list and walk it; shuffle,
//! repeat and editing come in M1.

use api::Track;

#[derive(Clone, Debug, PartialEq)]
pub struct QueueItem {
    /// Unique per queue entry: the same track queued twice gets two ids.
    pub uid: u64,
    pub track: Track,
}

#[derive(Debug, Default)]
pub struct Queue {
    items: Vec<QueueItem>,
    index: Option<usize>,
    next_uid: u64,
}

impl Queue {
    /// Replaces the queue and makes `start` current.
    pub fn replace(&mut self, tracks: Vec<Track>, start: usize) -> Option<&QueueItem> {
        self.items = tracks
            .into_iter()
            .map(|track| {
                self.next_uid += 1;
                QueueItem {
                    uid: self.next_uid,
                    track,
                }
            })
            .collect();
        self.index = (start < self.items.len()).then_some(start);
        self.current()
    }

    pub fn current(&self) -> Option<&QueueItem> {
        self.index.and_then(|i| self.items.get(i))
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Moves to the next entry; `None` (and no current entry) past the end.
    pub fn advance(&mut self) -> Option<&QueueItem> {
        let next = self.index.map_or(0, |i| i + 1);
        self.index = (next < self.items.len()).then_some(next);
        self.current()
    }

    /// Moves to the previous entry; stays on the first one.
    pub fn back(&mut self) -> Option<&QueueItem> {
        if let Some(i) = self.index {
            self.index = Some(i.saturating_sub(1));
        }
        self.current()
    }

    pub fn has_next(&self) -> bool {
        self.index.is_some_and(|i| i + 1 < self.items.len())
    }

    pub fn has_previous(&self) -> bool {
        self.index.is_some_and(|i| i > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: i64) -> Track {
        Track {
            id,
            ..Track::default()
        }
    }

    #[test]
    fn walks_the_queue() {
        let mut q = Queue::default();
        assert_eq!(q.replace(vec![track(1), track(2)], 1).unwrap().track.id, 2);
        assert!(!q.has_next());
        assert_eq!(q.back().unwrap().track.id, 1);
        assert_eq!(q.back().unwrap().track.id, 1);
        assert_eq!(q.advance().unwrap().track.id, 2);
        assert!(q.advance().is_none());
        assert!(q.current().is_none());
    }

    #[test]
    fn duplicates_get_distinct_uids() {
        let mut q = Queue::default();
        q.replace(vec![track(1), track(1)], 0);
        let first = q.current().unwrap().uid;
        let second = q.advance().unwrap().uid;
        assert_ne!(first, second);
        // A new queue never reuses ids.
        let third = q.replace(vec![track(1)], 0).unwrap().uid;
        assert!(third > second);
    }

    #[test]
    fn out_of_range_start_has_no_current() {
        let mut q = Queue::default();
        assert!(q.replace(vec![track(1)], 3).is_none());
        assert!(q.replace(vec![], 0).is_none());
        assert!(q.is_empty());
    }
}
