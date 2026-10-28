//! Play queue: entries in queue order, a play order (identity, or a
//! permutation while shuffled) and the current position in the play order.
//! Semantics follow the Electron client's `player.store.ts`.

use api::Track;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq)]
pub struct QueueItem {
    /// Unique per queue entry: the same track queued twice gets two ids.
    pub uid: u64,
    pub track: Track,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

impl Repeat {
    /// Off -> All -> One -> Off (the player bar button).
    pub fn cycle(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }
}

/// The queue as saved between runs (uids are not kept).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct QueueSnapshot {
    /// Entries in queue order.
    pub tracks: Vec<Track>,
    /// Play order (positions into `tracks`).
    pub order: Vec<usize>,
    /// Current position in the play order.
    pub position: Option<usize>,
    pub shuffle: bool,
}

/// Where a move through the queue lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Play the entry at this play-order position.
    Play(usize),
    /// Past the end without repeat: go back to this position, stopped.
    Stop(usize),
}

#[derive(Debug)]
pub struct Queue {
    items: Vec<QueueItem>,
    /// Play order: positions into `items`.
    order: Vec<usize>,
    /// Current position in `order`.
    pos: Option<usize>,
    shuffle: bool,
    next_uid: u64,
    rng: u64,
}

impl Default for Queue {
    fn default() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9e37_79b9_7f4a_7c15, |d| {
                u64::try_from(d.as_nanos() & u128::from(u64::MAX)).unwrap_or(1)
            });
        Self::with_seed(seed)
    }
}

impl Queue {
    pub fn with_seed(seed: u64) -> Self {
        Self {
            items: Vec::new(),
            order: Vec::new(),
            pos: None,
            shuffle: false,
            next_uid: 0,
            rng: seed | 1,
        }
    }

    // ------------------------------------------------------------ reading

    pub fn current(&self) -> Option<&QueueItem> {
        self.pos.map(|p| &self.items[self.order[p]])
    }

    /// Current position in the play order.
    pub fn position(&self) -> Option<usize> {
        self.pos
    }

    /// Entry at a play-order position.
    pub fn at(&self, pos: usize) -> Option<&QueueItem> {
        self.order.get(pos).map(|&i| &self.items[i])
    }

    /// Entries in play order (what plays next comes next).
    pub fn in_play_order(&self) -> impl Iterator<Item = &QueueItem> {
        self.order.iter().map(|&i| &self.items[i])
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    /// Where the queue goes when the current entry ends by itself.
    pub fn auto_next(&self, repeat: Repeat) -> Option<Step> {
        let pos = self.pos?;
        let last = self.order.len() - 1;
        Some(match repeat {
            Repeat::One => Step::Play(pos),
            Repeat::All => Step::Play(if pos == last { 0 } else { pos + 1 }),
            Repeat::Off if pos == last => Step::Stop(0),
            Repeat::Off => Step::Play(pos + 1),
        })
    }

    /// "Next" pressed: repeat-one still advances (and stays on the last).
    pub fn manual_next(&self, repeat: Repeat) -> Option<Step> {
        let pos = self.pos?;
        let last = self.order.len() - 1;
        Some(match repeat {
            Repeat::One => Step::Play((pos + 1).min(last)),
            _ => self.auto_next(repeat)?,
        })
    }

    /// "Previous" pressed (the caller handles restarting after 10 s).
    pub fn manual_previous(&self, repeat: Repeat) -> Option<Step> {
        let pos = self.pos?;
        Some(Step::Play(match (repeat, pos) {
            (Repeat::All, 0) => self.order.len() - 1,
            (_, 0) => 0,
            _ => pos - 1,
        }))
    }

    // ------------------------------------------------------------ moving

    /// Makes the entry at play-order position `pos` current.
    pub fn go(&mut self, pos: usize) -> Option<&QueueItem> {
        if pos < self.order.len() {
            self.pos = Some(pos);
        }
        self.current()
    }

    /// Makes the entry `uid` current.
    pub fn go_to_uid(&mut self, uid: u64) -> Option<&QueueItem> {
        let pos = self.order.iter().position(|&i| self.items[i].uid == uid)?;
        self.go(pos)
    }

    pub fn snapshot(&self) -> QueueSnapshot {
        QueueSnapshot {
            tracks: self.items.iter().map(|i| i.track.clone()).collect(),
            order: self.order.clone(),
            position: self.pos,
            shuffle: self.shuffle,
        }
    }

    /// Replaces the queue with a saved one. A play order that does not fit
    /// the entries, or was saved with the other shuffle mode, is rebuilt
    /// around the saved current entry.
    pub fn restore(&mut self, snapshot: QueueSnapshot) {
        let QueueSnapshot {
            tracks,
            order,
            position,
            shuffle,
        } = snapshot;
        self.items = self.wrap(tracks);
        let n = self.items.len();
        let mut seen = vec![false; n];
        let valid = order.len() == n
            && order
                .iter()
                .all(|&i| i < n && !std::mem::replace(&mut seen[i], true));
        let position = position.filter(|&p| p < n);
        if valid && shuffle == self.shuffle {
            self.order = order;
            self.pos = position;
        } else {
            let current = position.map(|p| if valid { order[p] } else { p });
            self.rebuild_order(current);
        }
    }

    // ------------------------------------------------------------ editing

    fn wrap(&mut self, tracks: Vec<Track>) -> Vec<QueueItem> {
        tracks
            .into_iter()
            .map(|track| {
                self.next_uid += 1;
                QueueItem {
                    uid: self.next_uid,
                    track,
                }
            })
            .collect()
    }

    /// Replaces the queue and makes `start` (queue order) current; while
    /// shuffled it plays first and the rest is shuffled.
    pub fn replace(&mut self, tracks: Vec<Track>, start: usize) -> Option<&QueueItem> {
        self.items = self.wrap(tracks);
        let start = (start < self.items.len()).then_some(start);
        self.rebuild_order(start);
        self.current()
    }

    /// Inserts after the current entry (at the front when nothing is current).
    pub fn play_next(&mut self, tracks: Vec<Track>) {
        let n = tracks.len();
        if n == 0 {
            return;
        }
        let new = self.wrap(tracks);
        let at = self.pos.map_or(0, |p| self.order[p] + 1);
        self.items.splice(at..at, new);
        if self.shuffle {
            for i in &mut self.order {
                if *i >= at {
                    *i += n;
                }
            }
            let mut added: Vec<usize> = (at..at + n).collect();
            self.shuffle_slice(&mut added);
            let insert = self.pos.map_or(0, |p| p + 1);
            self.order.splice(insert..insert, added);
        } else {
            // Inserted after the current entry, so its position is unchanged.
            self.order = (0..self.items.len()).collect();
        }
    }

    /// Appends; while shuffled the new entries go to the end of the play
    /// order in random order.
    pub fn add_last(&mut self, tracks: Vec<Track>) {
        let new = self.wrap(tracks);
        let start = self.items.len();
        self.items.extend(new);
        let mut added: Vec<usize> = (start..self.items.len()).collect();
        if self.shuffle {
            self.shuffle_slice(&mut added);
        }
        self.order.extend(added);
    }

    /// Removes the entry `uid`. Returns whether it was the current one; the
    /// entry that took its place in the play order (if any) is then current.
    pub fn remove(&mut self, uid: u64) -> Option<bool> {
        let index = self.items.iter().position(|i| i.uid == uid)?;
        let order_pos = self.order.iter().position(|&i| i == index)?;
        self.items.remove(index);
        self.order.remove(order_pos);
        for i in &mut self.order {
            if *i > index {
                *i -= 1;
            }
        }
        let was_current = self.pos == Some(order_pos);
        self.pos = match self.pos {
            Some(p) if order_pos < p => Some(p - 1),
            Some(p) if p >= self.order.len() => None,
            other => other,
        };
        Some(was_current)
    }

    /// Moves the entry `uid` to play-order position `to` (drag and drop);
    /// the current entry stays current. Returns whether anything moved.
    /// Shuffled, only the play order changes; otherwise the queue order.
    pub fn move_to(&mut self, uid: u64, to: usize) -> bool {
        let Some(from) = self.order.iter().position(|&i| self.items[i].uid == uid) else {
            return false;
        };
        let to = to.min(self.order.len() - 1);
        if from == to {
            return false;
        }
        let current = self.current().map(|i| i.uid);
        if self.shuffle {
            let entry = self.order.remove(from);
            self.order.insert(to, entry);
        } else {
            let entry = self.items.remove(from);
            self.items.insert(to, entry);
        }
        self.pos = current.and_then(|u| self.order.iter().position(|&i| self.items[i].uid == u));
        true
    }

    /// Updates the favorite flag of every entry of `track_id`.
    pub fn set_favorite(&mut self, track_id: i64, favorite: bool) {
        for item in &mut self.items {
            if item.track.id == track_id {
                item.track.is_favorite = favorite;
            }
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.order.clear();
        self.pos = None;
    }

    /// Turns shuffle on (current entry first, the rest shuffled) or off
    /// (queue order, staying on the current entry).
    pub fn set_shuffle(&mut self, shuffle: bool) {
        if shuffle == self.shuffle {
            return;
        }
        self.shuffle = shuffle;
        let current = self.pos.map(|p| self.order[p]);
        self.rebuild_order(current);
    }

    /// Play order from scratch with `current` (queue order index) current.
    fn rebuild_order(&mut self, current: Option<usize>) {
        if self.shuffle {
            let mut rest: Vec<usize> = (0..self.items.len())
                .filter(|&i| Some(i) != current)
                .collect();
            self.shuffle_slice(&mut rest);
            self.order = current.into_iter().chain(rest).collect();
            self.pos = current.map(|_| 0);
        } else {
            self.order = (0..self.items.len()).collect();
            self.pos = current;
        }
    }

    /// Fisher-Yates with xorshift64*.
    fn shuffle_slice(&mut self, v: &mut [usize]) {
        for i in (1..v.len()).rev() {
            self.rng ^= self.rng >> 12;
            self.rng ^= self.rng << 25;
            self.rng ^= self.rng >> 27;
            let r = self.rng.wrapping_mul(0x2545_f491_4f6c_dd1d);
            let j = usize::try_from(r % (i as u64 + 1)).unwrap_or(0);
            v.swap(i, j);
        }
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

    fn tracks(ids: std::ops::RangeInclusive<i64>) -> Vec<Track> {
        ids.map(track).collect()
    }

    fn ids(q: &Queue) -> Vec<i64> {
        q.in_play_order().map(|i| i.track.id).collect()
    }

    fn current_id(q: &Queue) -> Option<i64> {
        q.current().map(|i| i.track.id)
    }

    /// Follows `step(q)` and returns the new current id.
    fn walk(q: &mut Queue, step: impl Fn(&Queue) -> Option<Step>) -> Option<i64> {
        match step(q)? {
            Step::Play(p) | Step::Stop(p) => q.go(p).map(|i| i.track.id),
        }
    }

    #[test]
    fn move_keeps_the_current_entry() {
        let mut q = Queue::with_seed(3);
        q.replace(tracks(1..=5), 1);
        let uid_of = |q: &Queue, id: i64| q.in_play_order().find(|i| i.track.id == id).unwrap().uid;
        let uid = uid_of(&q, 5);
        assert!(q.move_to(uid, 0));
        assert_eq!(ids(&q), [5, 1, 2, 3, 4]);
        assert_eq!(current_id(&q), Some(2));
        assert_eq!(q.position(), Some(2));
        // Moving the current entry follows it.
        let uid = uid_of(&q, 2);
        assert!(q.move_to(uid, 99));
        assert_eq!(ids(&q), [5, 1, 3, 4, 2]);
        assert_eq!(q.position(), Some(4));
        assert!(!q.move_to(uid, 4));
        assert!(!q.move_to(12345, 0));

        // Shuffled: only the play order changes; turning shuffle off goes
        // back to the queue order.
        q.set_shuffle(true);
        let order = ids(&q);
        let last = *order.last().unwrap();
        assert!(q.move_to(uid_of(&q, last), 1));
        assert_eq!(ids(&q)[1], last);
        assert_eq!(current_id(&q), Some(2));
        q.set_shuffle(false);
        assert_eq!(ids(&q), [5, 1, 3, 4, 2]);
    }

    #[test]
    fn duplicates_get_distinct_uids() {
        let mut q = Queue::default();
        q.replace(vec![track(1), track(1)], 0);
        let first = q.current().unwrap().uid;
        let second = q.go(1).unwrap().uid;
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
        assert_eq!(q.auto_next(Repeat::All), None);
    }

    #[test]
    fn repeat_modes() {
        let mut q = Queue::default();
        q.replace(tracks(1..=3), 2);
        assert_eq!(q.auto_next(Repeat::Off), Some(Step::Stop(0)));
        assert_eq!(q.manual_next(Repeat::Off), Some(Step::Stop(0)));
        assert_eq!(q.auto_next(Repeat::All), Some(Step::Play(0)));
        assert_eq!(q.auto_next(Repeat::One), Some(Step::Play(2)));
        // Manual next with repeat-one advances, but stays on the last entry.
        assert_eq!(q.manual_next(Repeat::One), Some(Step::Play(2)));
        q.go(0);
        assert_eq!(q.manual_next(Repeat::One), Some(Step::Play(1)));
        assert_eq!(q.manual_previous(Repeat::Off), Some(Step::Play(0)));
        assert_eq!(q.manual_previous(Repeat::All), Some(Step::Play(2)));
        q.go(1);
        assert_eq!(q.manual_previous(Repeat::One), Some(Step::Play(0)));
    }

    #[test]
    fn shuffle_keeps_current_first_and_restores_order() {
        let mut q = Queue::with_seed(7);
        q.replace(tracks(1..=20), 4);
        q.set_shuffle(true);
        assert_eq!(q.position(), Some(0));
        assert_eq!(current_id(&q), Some(5));
        let mut sorted = ids(&q);
        assert_ne!(sorted, (1..=20).collect::<Vec<_>>(), "seed 7 shuffles");
        sorted.sort_unstable();
        assert_eq!(sorted, (1..=20).collect::<Vec<_>>());

        // Walk a few entries, then turn shuffle off: same entry, queue order.
        let id = walk(&mut q, |q| q.manual_next(Repeat::Off)).unwrap();
        q.set_shuffle(false);
        assert_eq!(ids(&q), (1..=20).collect::<Vec<_>>());
        assert_eq!(current_id(&q), Some(id));
        assert_eq!(q.position(), Some(usize::try_from(id - 1).unwrap()));
    }

    #[test]
    fn replace_while_shuffled_starts_with_the_chosen_entry() {
        let mut q = Queue::with_seed(3);
        q.set_shuffle(true);
        q.replace(tracks(1..=10), 6);
        assert_eq!((q.position(), current_id(&q)), (Some(0), Some(7)));
        assert_eq!(q.len(), 10);
    }

    #[test]
    fn play_next_and_add_last() {
        let mut q = Queue::default();
        q.replace(tracks(1..=3), 1);
        q.play_next(vec![track(10), track(11)]);
        assert_eq!(ids(&q), [1, 2, 10, 11, 3]);
        assert_eq!(current_id(&q), Some(2));
        q.add_last(vec![track(20)]);
        assert_eq!(ids(&q), [1, 2, 10, 11, 3, 20]);
        assert_eq!(walk(&mut q, |q| q.auto_next(Repeat::Off)), Some(10));

        // Nothing current: play-next goes to the front.
        let mut q = Queue::default();
        q.play_next(vec![track(1)]);
        q.play_next(vec![track(2)]);
        assert_eq!(ids(&q), [2, 1]);
        assert_eq!(q.current(), None);
    }

    #[test]
    fn play_next_while_shuffled_comes_right_after_current() {
        let mut q = Queue::with_seed(11);
        q.replace(tracks(1..=5), 0);
        q.set_shuffle(true);
        walk(&mut q, |q| q.auto_next(Repeat::Off));
        let current = current_id(&q);
        q.play_next(vec![track(10), track(11)]);
        assert_eq!(current_id(&q), current);
        let pos = q.position().unwrap();
        let mut next: Vec<i64> = ids(&q)[pos + 1..pos + 3].to_vec();
        next.sort_unstable();
        assert_eq!(next, [10, 11]);
        q.add_last(vec![track(20)]);
        assert_eq!(*ids(&q).last().unwrap(), 20);
        // Shuffle off: queue order has 10, 11 right after the current entry.
        q.set_shuffle(false);
        let order = ids(&q);
        let at = order.iter().position(|&i| Some(i) == current).unwrap();
        let mut after: Vec<i64> = order[at + 1..at + 3].to_vec();
        after.sort_unstable();
        assert_eq!(after, [10, 11]);
        assert_eq!(*order.last().unwrap(), 20);
    }

    #[test]
    fn remove_keeps_or_replaces_current() {
        let mut q = Queue::default();
        q.replace(tracks(1..=4), 2);
        let uid = |q: &Queue, id: i64| q.in_play_order().find(|i| i.track.id == id).unwrap().uid;

        assert_eq!(q.remove(uid(&q, 1)), Some(false));
        assert_eq!((ids(&q), current_id(&q)), (vec![2, 3, 4], Some(3)));
        // Removing the current entry: the next one takes its place.
        assert_eq!(q.remove(uid(&q, 3)), Some(true));
        assert_eq!((ids(&q), current_id(&q)), (vec![2, 4], Some(4)));
        // Removing the last current entry leaves nothing current.
        assert_eq!(q.remove(uid(&q, 4)), Some(true));
        assert_eq!(current_id(&q), None);
        assert_eq!(q.remove(12345), None);

        let mut q = Queue::with_seed(5);
        q.replace(tracks(1..=6), 0);
        q.set_shuffle(true);
        let gone = q.at(3).unwrap().uid;
        q.remove(gone);
        assert_eq!(ids(&q).len(), 5);
        assert_eq!(current_id(&q), Some(1));
        q.clear();
        assert!(q.is_empty() && q.current().is_none());
    }

    #[test]
    fn snapshot_round_trip() {
        let mut q = Queue::with_seed(9);
        q.replace(tracks(1..=6), 2);
        q.set_shuffle(true);
        walk(&mut q, |q| q.auto_next(Repeat::Off));
        let snap = q.snapshot();

        let mut r = Queue::with_seed(1);
        r.set_shuffle(true);
        r.restore(snap.clone());
        assert_eq!((ids(&r), current_id(&r)), (ids(&q), current_id(&q)));

        // Restored with shuffle off: queue order, same current entry.
        let mut r = Queue::default();
        r.restore(snap.clone());
        assert_eq!(ids(&r), (1..=6).collect::<Vec<_>>());
        assert_eq!(current_id(&r), current_id(&q));

        // A broken order falls back to queue order.
        let mut r = Queue::default();
        r.restore(QueueSnapshot {
            order: vec![0, 0, 1],
            position: Some(1),
            ..snap
        });
        assert_eq!((ids(&r).len(), current_id(&r)), (6, Some(2)));
    }

    #[test]
    fn go_to_uid() {
        let mut q = Queue::default();
        q.replace(tracks(1..=3), 0);
        let uid = q.at(2).unwrap().uid;
        assert_eq!(q.go_to_uid(uid).map(|i| i.track.id), Some(3));
        assert!(q.go_to_uid(999).is_none());
    }
}
