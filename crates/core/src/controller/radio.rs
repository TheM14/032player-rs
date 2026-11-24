//! Similar Track Radio and Queue Auto-Continue coordinator on [`Core`].
//!
//! Coordinates proactive prefetching at queue end, explicit Track Radio from
//! any track or the currently playing song, batch refresh ("换一批"), and manual
//! vs. auto-continued queue boundary preservation across local & Sonos playback.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use api::{ApiError, Feature, SimilarTrack, SimilarTracksOptions, Track};

use super::{Core, Event, Internal};
use crate::SleepTimer;
use crate::browse::TrackList;
use crate::queue::{Repeat, Step};
use crate::radio::{
    AUTO_CONTINUE_BATCH_SIZE, AUTO_CONTINUE_THRESHOLD, MAX_CACHED_REASONS, MAX_DISMISSED_HISTORY,
    MAX_RECENT_RECOMMENDED, RADIO_INITIAL_BATCH_SIZE, RadioErrorKind, RadioPauseReason, RadioSeed,
    RadioSnapshot, RadioStatus, SimilarityMethod, filter_and_diversify_seeded,
    primary_reason_label, remap_manual_count_after_move, remap_manual_count_after_remove,
    select_rolling_seed, should_trigger_auto_continue,
};

fn next_radio_seed() -> i64 {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    #[allow(clippy::cast_possible_truncation)]
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut x = nanos ^ seq.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    let positive = (x & (i64::MAX as u64)) as i64;
    if positive == 0 { 1 } else { positive }
}

/// What kind of radio fetch is in flight.
#[derive(Clone, Debug)]
pub(super) enum RadioFetchKind {
    StartFromCurrent { seed: RadioSeed },
    StartFromTrack { seed: RadioSeed },
    Refresh { seed_title: String },
    AutoContinue { seed_title: String, announce: bool },
}

/// Live coordinator state owned by [`Core`].
#[derive(Default)]
pub(super) struct RadioState {
    generation: u64,
    task: Option<tokio::task::AbortHandle>,
    pub(super) manual_queue_count: usize,
    pub(super) radio_seed: Option<RadioSeed>,
    pub(super) last_auto_seed_title: Option<String>,
    similar_reasons: HashMap<i64, Vec<String>>,
    reasons_order: VecDeque<i64>,
    dismissed_ids: HashSet<i64>,
    dismissed_order: VecDeque<i64>,
    recent_recommended_ids: HashSet<i64>,
    recent_recommended_order: VecDeque<i64>,
    pub(super) is_loading: bool,
    pub(super) error_kind: Option<RadioErrorKind>,
    /// Queue naturally reached end while an auto-continue fetch was running.
    pub(super) pending_play_after_end: bool,
}

impl Drop for RadioState {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl RadioState {
    fn bump(&mut self) -> u64 {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.generation += 1;
        self.pending_play_after_end = false;
        self.generation
    }

    fn clear_dismissed(&mut self) {
        self.dismissed_ids.clear();
        self.dismissed_order.clear();
    }

    fn remember_dismissed(&mut self, ids: impl IntoIterator<Item = i64>) {
        for id in ids {
            if id <= 0 {
                continue;
            }
            if self.dismissed_ids.insert(id) {
                self.dismissed_order.push_back(id);
            }
        }
        while self.dismissed_order.len() > MAX_DISMISSED_HISTORY {
            if let Some(old) = self.dismissed_order.pop_front() {
                self.dismissed_ids.remove(&old);
            }
        }
    }

    fn clear_recent_recommended(&mut self) {
        self.recent_recommended_ids.clear();
        self.recent_recommended_order.clear();
    }

    fn remember_recent_recommended(&mut self, ids: impl IntoIterator<Item = i64>) {
        for id in ids {
            if id <= 0 {
                continue;
            }
            if !self.recent_recommended_ids.insert(id)
                && let Some(pos) = self
                    .recent_recommended_order
                    .iter()
                    .position(|&existing| existing == id)
            {
                self.recent_recommended_order.remove(pos);
            }
            self.recent_recommended_order.push_back(id);
        }
        while self.recent_recommended_order.len() > MAX_RECENT_RECOMMENDED {
            if let Some(old) = self.recent_recommended_order.pop_front() {
                self.recent_recommended_ids.remove(&old);
            }
        }
    }

    fn merge_reasons(&mut self, incoming: &[SimilarTrack]) {
        for item in incoming {
            if item.track.id <= 0 || item.reasons.is_empty() {
                continue;
            }
            if let Some(pos) = self
                .reasons_order
                .iter()
                .position(|&id| id == item.track.id)
            {
                self.reasons_order.remove(pos);
            }
            self.reasons_order.push_back(item.track.id);
            self.similar_reasons
                .insert(item.track.id, item.reasons.clone());
        }
        while self.reasons_order.len() > MAX_CACHED_REASONS {
            if let Some(old) = self.reasons_order.pop_front() {
                self.similar_reasons.remove(&old);
            }
        }
    }
}

impl Core {
    pub(super) fn radio_supported(&self) -> bool {
        self.library
            .as_ref()
            .is_some_and(|l| l.capabilities.has(Feature::SimilarTracks))
    }

    pub(super) fn radio_similarity_method(&self) -> SimilarityMethod {
        let cap = self
            .library
            .as_ref()
            .and_then(|l| l.capabilities.similarity_method());
        SimilarityMethod::from_capability(cap)
    }

    pub(super) fn radio_pause_reason(&self) -> RadioPauseReason {
        match self.settings.repeat {
            Repeat::One => return RadioPauseReason::RepeatOne,
            Repeat::All => return RadioPauseReason::RepeatAll,
            Repeat::Off => {}
        }
        if let Some(sleep) = &self.sleep.active {
            match sleep.timer {
                SleepTimer::EndOfTrack => return RadioPauseReason::SleepTrack,
                SleepTimer::EndOfAlbum if self.pause_at_end() => {
                    return RadioPauseReason::SleepAlbum;
                }
                SleepTimer::EndOfAlbum | SleepTimer::Minutes(_) => {}
            }
        }
        RadioPauseReason::None
    }

    pub(super) fn radio_status(&self) -> RadioStatus {
        RadioStatus {
            supported: self.radio_supported(),
            auto_play_enabled: self.settings.auto_play_similar,
            manual_queue_count: self.radio.manual_queue_count.min(self.queue.len()),
            radio_seed: self.radio.radio_seed.clone(),
            last_auto_seed_title: self.radio.last_auto_seed_title.clone(),
            is_loading: self.radio.is_loading,
            error_kind: self.radio.error_kind,
            similarity_method: self.radio_similarity_method(),
            pause_reason: self.radio_pause_reason(),
        }
    }

    pub(super) fn emit_radio_state(&self) {
        self.emit(Event::RadioState(self.radio_status()));
    }

    pub(super) fn radio_reason_for(&self, track_id: i64) -> Option<String> {
        self.radio
            .similar_reasons
            .get(&track_id)
            .and_then(|reasons| primary_reason_label(reasons))
            .map(str::to_owned)
    }

    pub(super) fn radio_snapshot(&self) -> RadioSnapshot {
        RadioSnapshot {
            manual_queue_count: Some(self.radio.manual_queue_count.min(self.queue.len())),
            radio_seed: self.radio.radio_seed.clone(),
            last_auto_seed_title: self.radio.last_auto_seed_title.clone(),
            similar_reasons: self.radio.similar_reasons.clone(),
        }
    }

    pub(super) fn radio_restore(&mut self, snapshot: RadioSnapshot) {
        self.radio.bump();
        self.radio.clear_dismissed();
        let len = self.queue.len();
        self.radio.manual_queue_count = snapshot.manual_queue_count.map_or(len, |c| c.min(len));
        self.radio.radio_seed = snapshot.radio_seed;
        self.radio.last_auto_seed_title = snapshot.last_auto_seed_title;
        self.radio.similar_reasons.clear();
        self.radio.reasons_order.clear();
        for (id, reasons) in snapshot.similar_reasons {
            if id > 0 && !reasons.is_empty() && self.radio.reasons_order.len() < MAX_CACHED_REASONS
            {
                self.radio.reasons_order.push_back(id);
                self.radio.similar_reasons.insert(id, reasons);
            }
        }
        self.radio.is_loading = false;
        self.radio.error_kind = None;
        self.emit_radio_state();
    }

    /// Resets radio state when a normal (non-radio) manual queue replaces the
    /// queue. Callers that do not immediately follow with `start_current` can
    /// call [`Self::maybe_trigger_auto_continue`] afterwards.
    pub(super) fn radio_reset_for_manual_queue(&mut self) {
        self.radio.bump();
        self.radio.clear_dismissed();
        self.radio.manual_queue_count = self.queue.len();
        self.radio.radio_seed = None;
        self.radio.last_auto_seed_title = None;
        self.radio.similar_reasons.clear();
        self.radio.reasons_order.clear();
        self.radio.is_loading = false;
        self.radio.error_kind = None;
        self.emit_radio_state();
    }

    /// Called when the queue is completely cleared or the user signs out.
    pub(super) fn radio_reset(&mut self) {
        self.radio.bump();
        self.radio.clear_dismissed();
        self.radio.manual_queue_count = 0;
        self.radio.radio_seed = None;
        self.radio.last_auto_seed_title = None;
        self.radio.similar_reasons.clear();
        self.radio.reasons_order.clear();
        self.radio.is_loading = false;
        self.radio.error_kind = None;
        self.emit_radio_state();
    }

    pub(super) fn radio_clear_recent_recommended(&mut self) {
        self.radio.clear_recent_recommended();
    }

    pub(super) fn radio_on_manual_inserted(&mut self, insert_index: usize, count: usize) {
        let len = self.queue.len();
        let prev_len = len.saturating_sub(count);
        let boundary = self.radio.manual_queue_count.min(prev_len);
        self.radio.manual_queue_count = if insert_index <= boundary {
            (boundary + count).min(len)
        } else {
            boundary.min(len)
        };
        self.emit_radio_state();
    }

    pub(super) fn radio_on_track_moved(&mut self, from: usize, to: usize) {
        self.radio.manual_queue_count = remap_manual_count_after_move(
            self.radio.manual_queue_count,
            from,
            to,
            self.queue.len(),
        );
        self.emit_radio_state();
    }

    pub(super) fn radio_on_track_removed(&mut self, remove_index: usize, removed_track_id: i64) {
        if remove_index >= self.radio.manual_queue_count {
            self.radio.remember_dismissed([removed_track_id]);
        }
        self.radio.manual_queue_count = remap_manual_count_after_remove(
            self.radio.manual_queue_count,
            remove_index,
            self.queue.len(),
        );
        self.emit_radio_state();
        self.maybe_trigger_auto_continue(false);
    }

    pub(super) fn set_auto_play_similar(&mut self, enabled: bool) {
        self.update_settings(|s| s.auto_play_similar = enabled);
        if enabled {
            self.radio.error_kind = None;
            self.emit_radio_state();
            self.emit(Event::Notice("已开启队列播完自动续播".into()));
            self.maybe_trigger_auto_continue(false);
            return;
        }
        self.radio.bump();
        let trimmed = if !self.queue.is_empty() {
            let cur = self.queue.position().unwrap_or(0);
            let keep_through = self
                .radio
                .manual_queue_count
                .saturating_sub(1)
                .max(cur)
                .min(self.queue.len() - 1);
            if keep_through + 1 < self.queue.len() {
                self.queue.replace_after(keep_through, Vec::new());
                true
            } else {
                false
            }
        } else {
            false
        };
        self.radio.manual_queue_count = self.queue.len();
        self.radio.radio_seed = None;
        self.radio.last_auto_seed_title = None;
        self.radio.is_loading = false;
        self.radio.error_kind = None;
        if trimmed {
            self.queue_changed();
        }
        self.emit_radio_state();
        self.emit(Event::Notice("已关闭队列播完自动续播".into()));
    }

    /// Starts Track Radio from the currently playing song without interrupting
    /// playback: keeps `[0..=current]` intact and replaces everything after.
    pub(super) fn start_radio_from_current(&mut self) {
        let Some(current) = self.queue.current().cloned() else {
            return;
        };
        if !self.radio_supported() || current.track.id <= 0 {
            self.emit(Event::Notice("当前服务器不支持相似电台".into()));
            return;
        }
        self.update_settings(|s| s.auto_play_similar = true);
        let radio_gen = self.radio.bump();
        self.radio.clear_dismissed();
        self.radio.is_loading = true;
        self.radio.error_kind = None;
        self.emit_radio_state();

        let keep_end = self.queue.position().map_or(0, |p| p + 1);
        let existing: Vec<Track> = self
            .queue
            .in_play_order()
            .take(keep_end)
            .map(|i| i.track.clone())
            .collect();
        let seed = RadioSeed {
            track_id: current.track.id,
            title: current.track.title.clone(),
        };
        self.spawn_radio_fetch(
            radio_gen,
            current.track.id,
            &existing,
            RADIO_INITIAL_BATCH_SIZE,
            RadioFetchKind::StartFromCurrent { seed },
        );
    }

    /// Starts Track Radio from any track: plays it immediately and appends a
    /// similar batch seamlessly.
    pub(super) fn start_radio_from_track(&mut self, track: Track) {
        if !self.radio_supported() || track.id <= 0 {
            self.emit(Event::Notice("当前服务器不支持相似电台".into()));
            return;
        }
        self.clear_repeat_and_shuffle_for_radio();
        self.update_settings(|s| s.auto_play_similar = true);
        let radio_gen = self.radio.bump();
        self.radio.clear_dismissed();
        self.begin_play_intent();

        let seed = RadioSeed {
            track_id: track.id,
            title: track.title.clone(),
        };
        let seed_track_id = track.id;
        let existing = vec![track.clone()];
        self.queue.replace(vec![track], 0);
        self.radio.manual_queue_count = 1;
        self.radio.radio_seed = Some(seed.clone());
        self.radio.last_auto_seed_title = Some(seed.title.clone());
        self.radio.similar_reasons.clear();
        self.radio.reasons_order.clear();
        self.radio.is_loading = true;
        self.radio.error_kind = None;
        self.play_current(true);
        self.queue_changed();
        self.emit_radio_state();

        self.spawn_radio_fetch(
            radio_gen,
            seed_track_id,
            &existing,
            RADIO_INITIAL_BATCH_SIZE,
            RadioFetchKind::StartFromTrack { seed },
        );
    }

    pub(super) fn start_radio_from_list(&mut self, list: TrackList, index: usize) {
        let track = self
            .library
            .as_ref()
            .and_then(|l| l.list(list).get(index))
            .cloned();
        if let Some(track) = track {
            self.start_radio_from_track(track);
        }
    }

    pub(super) fn start_radio_from_queue_item(&mut self, uid: u64) {
        if self.queue.current().is_some_and(|i| i.uid == uid) {
            self.start_radio_from_current();
            return;
        }
        if let Some(item) = self.queue.item_by_uid(uid).cloned() {
            self.start_radio_from_track(item.track);
        }
    }

    /// Replaces unplayed auto-similar tracks with a fresh batch ("换一批").
    pub(super) fn refresh_auto_similar(&mut self) {
        if !self.settings.auto_play_similar || !self.radio_supported() || self.queue.is_empty() {
            return;
        }
        let tracks: Vec<Track> = self
            .queue
            .in_play_order()
            .map(|i| i.track.clone())
            .collect();
        let cur = self.queue.position().unwrap_or(0);
        let keep_through = self
            .radio
            .manual_queue_count
            .saturating_sub(1)
            .max(cur)
            .min(tracks.len() - 1);
        let Some(seed) = self.resolve_seed_for_refresh(&tracks, cur, keep_through) else {
            return;
        };
        if keep_through + 1 < tracks.len() {
            self.radio
                .remember_dismissed(tracks[keep_through + 1..].iter().map(|t| t.id));
        }
        let radio_gen = self.radio.bump();
        self.radio.is_loading = true;
        self.radio.error_kind = None;
        self.emit_radio_state();

        self.spawn_radio_fetch(
            radio_gen,
            seed.id,
            &tracks,
            AUTO_CONTINUE_BATCH_SIZE,
            RadioFetchKind::Refresh {
                seed_title: seed.title,
            },
        );
    }

    /// Manually triggers appending one more batch ("再加一批" / retry button).
    pub(super) fn load_more_auto_similar(&mut self) {
        if !self.settings.auto_play_similar
            || !self.radio_supported()
            || self.radio.is_loading
            || self.queue.is_empty()
        {
            return;
        }
        let tracks: Vec<Track> = self
            .queue
            .in_play_order()
            .map(|i| i.track.clone())
            .collect();
        let Some(seed) = select_rolling_seed(&tracks, self.queue.position()).cloned() else {
            return;
        };
        let radio_gen = self.radio.bump();
        self.radio.is_loading = true;
        self.radio.error_kind = None;
        self.emit_radio_state();

        self.spawn_radio_fetch(
            radio_gen,
            seed.id,
            &tracks,
            AUTO_CONTINUE_BATCH_SIZE,
            RadioFetchKind::AutoContinue {
                seed_title: seed.title,
                announce: true,
            },
        );
    }

    /// Proactively prefetches and appends similar tracks when approaching the
    /// end of the queue. Returns `true` when an async fetch is started (or was
    /// already running and adopted for `play_first_if_ended`).
    pub(super) fn maybe_trigger_auto_continue(&mut self, play_first_if_ended: bool) -> bool {
        let sleep_blocking = matches!(
            self.radio_pause_reason(),
            RadioPauseReason::SleepTrack | RadioPauseReason::SleepAlbum
        );
        if play_first_if_ended
            && self.radio.is_loading
            && self.radio_supported()
            && self.settings.auto_play_similar
            && self.settings.repeat == Repeat::Off
            && !sleep_blocking
        {
            self.radio.pending_play_after_end = true;
            return true;
        }
        let eligible = should_trigger_auto_continue(
            self.radio_supported(),
            self.settings.auto_play_similar,
            self.settings.repeat,
            sleep_blocking,
            self.queue.len(),
            self.queue.position(),
            self.radio.is_loading,
            AUTO_CONTINUE_THRESHOLD,
        );
        if !eligible {
            return false;
        }
        let tracks: Vec<Track> = self
            .queue
            .in_play_order()
            .map(|i| i.track.clone())
            .collect();
        let Some(seed) = select_rolling_seed(&tracks, self.queue.position()).cloned() else {
            return false;
        };
        let radio_gen = self.radio.bump();
        self.radio.is_loading = true;
        self.radio.error_kind = None;
        self.radio.pending_play_after_end = play_first_if_ended;
        self.emit_radio_state();

        self.spawn_radio_fetch(
            radio_gen,
            seed.id,
            &tracks,
            AUTO_CONTINUE_BATCH_SIZE,
            RadioFetchKind::AutoContinue {
                seed_title: seed.title,
                announce: false,
            },
        );
        true
    }

    fn resolve_seed_for_refresh(
        &self,
        tracks: &[Track],
        cur: usize,
        keep_through: usize,
    ) -> Option<Track> {
        if let Some(seed) = &self.radio.radio_seed
            && let Some(found) = tracks.iter().find(|t| t.id == seed.track_id && t.id > 0)
        {
            return Some(found.clone());
        }
        let slice = &tracks[..=keep_through.min(tracks.len() - 1)];
        select_rolling_seed(slice, Some(cur.min(slice.len() - 1))).cloned()
    }

    fn clear_repeat_and_shuffle_for_radio(&mut self) {
        if self.settings.repeat != Repeat::Off {
            self.update_settings(|s| s.repeat = Repeat::Off);
            self.emit(Event::Repeat(Repeat::Off));
            self.cast_play_mode();
        }
        if self.settings.shuffle {
            self.queue.set_shuffle(false);
            self.update_settings(|s| s.shuffle = false);
            self.emit(Event::Shuffle(false));
        }
    }

    fn spawn_radio_fetch(
        &mut self,
        radio_gen: u64,
        seed_track_id: i64,
        existing: &[Track],
        limit: usize,
        kind: RadioFetchKind,
    ) {
        let Some((client, tx)) = self.request() else {
            self.radio.is_loading = false;
            self.emit_radio_state();
            return;
        };
        let list_gen = self.list_generation;
        let api_token = client.api_token().to_owned();
        let mut excluded_ids: HashSet<i64> =
            existing.iter().map(|t| t.id).filter(|&id| id > 0).collect();
        excluded_ids.extend(self.radio.dismissed_ids.iter().copied());
        let exclude_vec: Vec<i64> = excluded_ids.iter().copied().collect();
        #[allow(clippy::cast_possible_truncation)]
        let request_limit = ((limit * 3).clamp(18, 48)) as u32;
        let request_seed = next_radio_seed();

        let handle = tokio::spawn(async move {
            let result = client
                .similar_tracks_detailed(
                    seed_track_id,
                    SimilarTracksOptions {
                        limit: request_limit,
                        exclude_ids: &exclude_vec,
                        diverse: true,
                        seed: Some(request_seed),
                    },
                )
                .await;
            let _ = tx.send(Internal::RadioFetched {
                list_gen,
                radio_gen,
                api_token,
                kind,
                excluded_ids,
                limit,
                seed: request_seed,
                result,
            });
        });
        self.radio.task = Some(handle.abort_handle());
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_radio_fetched(
        &mut self,
        list_gen: u64,
        radio_gen: u64,
        api_token: String,
        kind: RadioFetchKind,
        mut excluded_ids: HashSet<i64>,
        limit: usize,
        seed: i64,
        result: Result<Vec<SimilarTrack>, ApiError>,
    ) {
        if list_gen != self.list_generation || radio_gen != self.radio.generation {
            return;
        }
        let play_after_end = std::mem::take(&mut self.radio.pending_play_after_end);
        self.radio.is_loading = false;
        self.radio.task = None;

        let raw = match result {
            Ok(items) => items,
            Err(e) if e.is_unauthorized() => {
                self.internal(Internal::Unauthorized { api_token });
                return;
            }
            Err(e) if e.status() == Some(404) => Vec::new(),
            Err(e) => {
                let err_kind = if e.status() == Some(503) {
                    RadioErrorKind::Busy
                } else {
                    RadioErrorKind::OfflineOrFailed
                };
                self.radio.error_kind = Some(err_kind);
                self.emit_radio_state();
                let announce = !matches!(
                    kind,
                    RadioFetchKind::AutoContinue {
                        announce: false,
                        ..
                    }
                );
                if announce {
                    let msg = match err_kind {
                        RadioErrorKind::Busy => "相似度服务正忙，请稍后重试",
                        _ => "无法获取相似曲目，请检查网络连接",
                    };
                    self.emit(Event::Notice(msg.into()));
                }
                if play_after_end {
                    self.step(Some(Step::Stop(0)), false);
                }
                return;
            }
        };

        // Include live queue tracks & dismissed IDs in case queue mutated while in flight.
        for item in self.queue.in_play_order() {
            if item.track.id > 0 {
                excluded_ids.insert(item.track.id);
            }
        }
        excluded_ids.extend(self.radio.dismissed_ids.iter().copied());
        let picked = filter_and_diversify_seeded(
            raw,
            &excluded_ids,
            &self.radio.recent_recommended_ids,
            2,
            limit,
            Some(seed),
        );

        if picked.is_empty() {
            self.radio.error_kind = Some(RadioErrorKind::Empty);
            self.emit_radio_state();
            let announce = !matches!(
                kind,
                RadioFetchKind::AutoContinue {
                    announce: false,
                    ..
                }
            );
            if announce {
                self.emit(Event::Notice("未找到更多相似曲目".into()));
            }
            if play_after_end {
                self.step(Some(Step::Stop(0)), false);
            }
            return;
        }

        self.radio.merge_reasons(&picked);
        self.radio.error_kind = None;
        let new_tracks: Vec<Track> = picked.into_iter().map(|s| s.track).collect();
        let added_count = new_tracks.len();

        match kind {
            RadioFetchKind::StartFromCurrent { seed } => {
                let Some(cur_pos) = self.queue.position() else {
                    self.emit_radio_state();
                    return;
                };
                if self
                    .queue
                    .current()
                    .is_none_or(|i| i.track.id != seed.track_id)
                {
                    self.emit_radio_state();
                    return;
                }
                self.clear_repeat_and_shuffle_for_radio();
                let cur_pos = self.queue.position().unwrap_or(cur_pos);
                self.radio
                    .remember_recent_recommended(new_tracks.iter().map(|t| t.id));
                self.queue.replace_after(cur_pos, new_tracks);
                self.radio.manual_queue_count = cur_pos + 1;
                self.radio.radio_seed = Some(seed.clone());
                self.radio.last_auto_seed_title = Some(seed.title.clone());
                self.queue_changed();
                self.emit_radio_state();
                self.emit(Event::Notice(format!(
                    "已基于《{}》开启相似电台（续排 {added_count} 首）",
                    seed.title
                )));
            }
            RadioFetchKind::StartFromTrack { seed } => {
                if self.queue.at(0).is_none_or(|i| i.track.id != seed.track_id) {
                    self.emit_radio_state();
                    return;
                }
                self.radio
                    .remember_recent_recommended(new_tracks.iter().map(|t| t.id));
                self.queue.append_ordered(new_tracks);
                self.queue_changed();
                self.emit_radio_state();
                self.emit(Event::Notice(format!(
                    "正在播放《{}》的相似电台（已追加 {added_count} 首）",
                    seed.title
                )));
            }
            RadioFetchKind::Refresh { seed_title } => {
                if self.queue.is_empty() || !self.settings.auto_play_similar {
                    self.emit_radio_state();
                    return;
                }
                let cur = self.queue.position().unwrap_or(0);
                let keep_through = self
                    .radio
                    .manual_queue_count
                    .saturating_sub(1)
                    .max(cur)
                    .min(self.queue.len() - 1);
                self.radio
                    .remember_recent_recommended(new_tracks.iter().map(|t| t.id));
                self.queue.replace_after(keep_through, new_tracks);
                self.radio.last_auto_seed_title = Some(seed_title);
                self.queue_changed();
                self.emit_radio_state();
                self.emit(Event::Notice(format!("已更换 {added_count} 首相似曲目")));
            }
            RadioFetchKind::AutoContinue {
                seed_title,
                announce,
            } => {
                if self.queue.is_empty() || !self.settings.auto_play_similar {
                    self.emit_radio_state();
                    return;
                }
                let first_new = self.queue.len();
                self.radio
                    .remember_recent_recommended(new_tracks.iter().map(|t| t.id));
                self.queue.append_ordered(new_tracks);
                self.radio.last_auto_seed_title = Some(seed_title);
                self.queue_changed();
                self.emit_radio_state();
                if play_after_end && first_new < self.queue.len() {
                    self.queue.go(first_new);
                    self.play_current(false);
                    self.emit_queue_position();
                } else if announce {
                    self.emit(Event::Notice(format!("已补充 {added_count} 首相似曲目")));
                }
            }
        }
    }
}
