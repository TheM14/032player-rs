//! Local playback transport state, queue persistence, gapless preloading, and
//! engine event handling.

use std::path::PathBuf;

use api::{Track, Transcode};
use session::{AdapterStatus, AdapterTrack, KeyValueStorage};

use super::fade;
use super::{
    Core, Event, Internal, POSITION_KEY, POSITION_SAVE_STEP_MS, POSITION_STEP_MS, QUEUE_FILE,
    QUEUE_SAVE_DELAY, QueueEntry, QueuePosition, SavedQueue,
};
use crate::engine::{EngineEnd, EngineEvent};
use crate::format;
use crate::queue::Step;
use crate::settings::ExclusiveMode;

/// Transport state owned by [`Core`].
pub(super) struct PlaybackState {
    /// Nothing loaded in the engine (before the first play, after the end
    /// of the queue or a failure): play reloads the current entry.
    pub(super) idle: bool,
    /// Between `load` and the new file's `FileLoaded`: progress, duration
    /// and end-of-file signals still belong to the previous file.
    pub(super) loading: bool,
    pub(super) paused: bool,
    pub(super) buffering: bool,
    pub(super) position_ms: i64,
    pub(super) reported_step: i64,
    pub(super) engine_duration_ms: i64,
    /// Lossy transcodes cannot seek by range: the stream starts here and
    /// engine positions are relative to it.
    pub(super) stream_offset_ms: i64,
    /// Transcode active on the currently open local stream (independent of live setting changes).
    pub(super) current_transcode: Option<Transcode>,
    /// Transcode active on the preloaded upcoming stream.
    pub(super) preloaded_transcode: Option<Transcode>,
    /// Queue entry and URL appended to the engine for gapless playback.
    pub(super) preloaded: Option<(u64, String)>,
    /// URL of the file being loaded; a different `FileLoaded` is a stale preload.
    pub(super) expected_path: Option<String>,
    /// Entry of the last started play (repeat-one passes start it again).
    pub(super) started_uid: Option<u64>,
    /// Restored queue entry and the position to continue it at.
    pub(super) resume_at: Option<(u64, i64)>,
    /// Seek to this once the loading file is loaded (resumed position).
    pub(super) pending_seek_ms: Option<i64>,
    /// 接着听: the play starts at 0 for the server and then seeks here
    /// (reported once the file is loaded), like the Electron client.
    pub(super) report_seek_ms: Option<i64>,
    pub(super) saved_position_step: i64,
    /// Monotonic counter for play intents; supersedes older in-flight async play fetches.
    pub(super) play_intent_seq: u64,
    pub(super) fade: fade::Fader,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            idle: true,
            loading: false,
            paused: false,
            buffering: false,
            position_ms: 0,
            reported_step: -1,
            engine_duration_ms: 0,
            stream_offset_ms: 0,
            current_transcode: None,
            preloaded_transcode: None,
            preloaded: None,
            expected_path: None,
            started_uid: None,
            resume_at: None,
            pending_seek_ms: None,
            report_seek_ms: None,
            saved_position_step: -1,
            play_intent_seq: 0,
            fade: fade::Fader::default(),
        }
    }
}

/// Options controlling which fields [`Core::reset_transport`] clears beyond the
/// baseline stopped-transport fields (fade, buffering, idle/loading/paused,
/// preload, transcode, expected path, pending seeks, stream offset).
#[derive(Clone, Copy, Default)]
#[allow(clippy::struct_excessive_bools)]
pub(super) struct TransportReset {
    pub(super) reset_position: bool,
    pub(super) reset_duration: bool,
    pub(super) clear_started_uid: bool,
    pub(super) clear_resume: bool,
    pub(super) clear_asio: bool,
}

impl TransportReset {
    pub(super) const fn stop() -> Self {
        Self {
            reset_position: true,
            reset_duration: false,
            clear_started_uid: false,
            clear_resume: false,
            clear_asio: true,
        }
    }

    pub(super) const fn failure() -> Self {
        Self {
            reset_position: false,
            reset_duration: false,
            clear_started_uid: false,
            clear_resume: false,
            clear_asio: true,
        }
    }

    pub(super) const fn handover() -> Self {
        Self {
            reset_position: false,
            reset_duration: false,
            clear_started_uid: true,
            clear_resume: false,
            clear_asio: false,
        }
    }

    pub(super) const fn full() -> Self {
        Self {
            reset_position: true,
            reset_duration: true,
            clear_started_uid: true,
            clear_resume: true,
            clear_asio: true,
        }
    }
}

impl Core {
    /// Unified transport reset shared by stop, failure, handover and sign-out so
    /// fade, buffering, preload, transcode and ASIO flags cannot drift out of sync.
    pub(super) fn reset_transport(&mut self, opts: TransportReset) {
        self.restore_gain();
        self.clear_buffering();
        self.pb.idle = true;
        self.pb.loading = false;
        self.pb.paused = true;
        self.pb.preloaded = None;
        self.pb.current_transcode = None;
        self.pb.preloaded_transcode = None;
        self.pb.expected_path = None;
        self.pb.pending_seek_ms = None;
        self.pb.report_seek_ms = None;
        self.pb.stream_offset_ms = 0;
        if opts.reset_position {
            self.pb.position_ms = 0;
            self.pb.reported_step = -1;
        }
        if opts.reset_duration {
            self.pb.engine_duration_ms = 0;
        }
        if opts.clear_started_uid {
            self.pb.started_uid = None;
        }
        if opts.clear_resume {
            self.pb.resume_at = None;
        }
        if opts.clear_asio && self.output.clear_asio_stream() {
            self.emit_output_mode();
        }
    }

    pub(super) fn seek(&mut self, position_ms: i64) {
        let position_ms = position_ms.max(0);
        if self.pb.idle
            && let Some((uid, _)) = self.pb.resume_at
        {
            // Restored, not started yet: move where playing will start.
            self.pb.resume_at = Some((uid, position_ms));
            self.pb.position_ms = position_ms;
            self.emit_position();
            self.update_active_line();
            return;
        }
        if self.pb.idle || self.queue.current().is_none() {
            return;
        }
        #[allow(clippy::cast_precision_loss)]
        let seconds = position_ms as f64 / 1000.0;
        if self.casting() {
            if let Err(e) = self.cast_seek(position_ms) {
                self.emit(Event::Notice(e));
                return;
            }
        } else if self.pb.loading {
            // While the file is still loading, avoid sending an early seek to the
            // engine and override any pending initial resume seek instead.
            if self.current_lossy_transcode() {
                if !self.reload_at(position_ms) {
                    return;
                }
            } else {
                self.pb.pending_seek_ms = Some(position_ms);
                self.pb.report_seek_ms = None;
            }
        } else if self.current_lossy_transcode() {
            // No byte ranges: restart the stream at the new position.
            if !self.reload_at(position_ms) {
                return;
            }
        } else if let Err(e) = self.engine.seek(seconds) {
            self.emit(Event::Notice(e));
            return;
        }
        self.pb.position_ms = position_ms;
        if let Some(r) = self.reporter.as_mut() {
            r.act(|a| a.on_seek(seconds));
        }
        self.emit_position();
        self.update_active_line();
    }

    pub(super) fn adapter_track(&self) -> Option<AdapterTrack> {
        self.queue.current().map(|item| AdapterTrack {
            context: item.context.clone(),
            duration_millis: u64::try_from(item.track.duration_millis).unwrap_or(0),
            track_id: item.track.id,
            unique_id: item.uid.to_string(),
        })
    }

    pub(super) fn restart_current(&mut self) {
        self.seek(0);
        self.emit(Event::LyricsReset);
    }

    /// Applies a queue move: play the target, restart it when it is the
    /// entry already playing, or stop at the start of the queue.
    pub(super) fn step(&mut self, step: Option<Step>, user_navigated: bool) {
        self.radio.pending_play_after_end = false;
        match step {
            None => {}
            Some(Step::Play(pos)) if Some(pos) == self.queue.position() && !self.pb.idle => {
                self.restart_current();
            }
            Some(Step::Play(pos)) if self.stops_at_track_change() => {
                // "End of track" stops at the next change of track, skips included.
                self.stop_playback();
                self.queue.go(pos);
                self.show_current_stopped();
                self.emit_queue_position();
                self.set_sleep_timer(None);
            }
            Some(Step::Play(pos)) => {
                self.queue.go(pos);
                self.play_current(user_navigated);
                self.emit_queue_position();
            }
            Some(Step::Stop(pos)) => {
                self.stop_playback();
                self.queue.go(pos);
                self.show_current_stopped();
                self.emit_queue_position();
                self.clear_sleep_on_stop();
            }
        }
    }

    /// Stops the engine and ends the reported play; the queue is untouched.
    pub(super) fn stop_playback(&mut self) {
        if !self.pb.idle {
            if self.casting() {
                self.cast_stop();
            } else {
                let _ = self.engine.stop();
            }
            if let Some(r) = self.reporter.as_mut() {
                r.act(|a| a.on_status(AdapterStatus::Stopped));
            }
        }
        self.reset_transport(TransportReset::stop());
        self.save_position();
        self.emit(Event::Paused(true));
        self.emit_position();
    }

    /// Shows the current entry (if any) without playing it.
    pub(super) fn show_current_stopped(&mut self) {
        let adapter_track = self.adapter_track();
        if let Some(r) = self.reporter.as_mut() {
            r.act(|a| a.on_track_change(adapter_track));
        }
        self.pb.started_uid = None;
        let current = self.queue.current().map(|item| item.track.clone());
        self.emit_now_playing(current.as_ref());
        self.emit_position();
        match self.queue.current().map(|i| i.track.id) {
            Some(id) => self.load_lyrics(id),
            None => self.clear_lyrics(),
        }
        self.update_playing_sources();
    }

    /// The transcode in use, if the server offers it.
    pub(super) fn transcode(&self) -> Option<Transcode> {
        let transcode = self.settings.transcode?;
        let library = self.library.as_ref()?;
        library
            .capabilities
            .can_transcode(transcode.format())
            .then_some(transcode)
    }

    /// MP3 / Opus transcodes are generated live and cannot seek by range.
    fn is_lossy_transcode(transcode: Option<Transcode>) -> bool {
        matches!(transcode, Some(Transcode::Mp3(_) | Transcode::Ogg(_)))
    }

    fn current_lossy_transcode(&self) -> bool {
        Self::is_lossy_transcode(self.pb.current_transcode)
    }

    /// Stream URL of a track using `transcode`, starting at `offset_ms` (lossy transcodes).
    fn track_url_for(
        &self,
        track_id: i64,
        transcode: Option<Transcode>,
        offset_ms: i64,
    ) -> Option<String> {
        let library = self.library.as_ref()?;
        Some(
            match transcode {
                Some(t) => library.client.transcode_url(track_id, t, offset_ms),
                None => library.client.stream_url(track_id),
            }
            .to_string(),
        )
    }

    /// Restarts the current lossy transcode at `position_ms`.
    fn reload_at(&mut self, position_ms: i64) -> bool {
        let Some(id) = self.queue.current().map(|i| i.track.id) else {
            return false;
        };
        let Some(url) = self.track_url_for(id, self.pb.current_transcode, position_ms) else {
            return false;
        };
        if let Err(e) = self.engine.load_for(&url, self.output.asio_active) {
            self.emit(Event::Notice(e));
            return false;
        }
        if self.pb.paused {
            let _ = self.engine.set_paused(true);
        }
        self.pb.loading = true;
        self.pb.preloaded = None;
        self.pb.preloaded_transcode = None;
        self.pb.pending_seek_ms = None;
        self.pb.report_seek_ms = None;
        self.pb.expected_path = Some(url);
        self.pb.stream_offset_ms = position_ms;
        true
    }

    pub(super) fn enqueue(&mut self, tracks: Vec<Track>, at: QueuePosition) {
        if tracks.is_empty() {
            return;
        }
        let n = tracks.len();
        let had_auto = self.radio.manual_queue_count < self.queue.len();
        let idx = crate::radio::manual_insert_index(
            at,
            self.queue.position(),
            self.queue.len(),
            self.radio.manual_queue_count,
        );
        self.queue.insert_at_play_order(idx, tracks);
        self.radio_on_manual_inserted(idx, n);
        self.queue_changed();
        let what = if n == 1 {
            "1 首".to_owned()
        } else {
            format!("{n} 首")
        };
        self.emit(Event::Notice(match at {
            QueuePosition::Next => format!("已添加 {what}到下一首播放"),
            QueuePosition::Last if had_auto => format!("已添加 {what}到手动队列末尾（相似续播前）"),
            QueuePosition::Last => format!("已添加 {what}到队列末尾"),
        }));
    }

    pub(super) fn remove_from_queue(&mut self, uid: u64) {
        let removed_info = self
            .queue
            .position_of_uid(uid)
            .and_then(|pos| self.queue.at(pos).map(|item| (pos, item.track.id)));
        let Some(was_current) = self.queue.remove(uid) else {
            return;
        };
        if let Some((pos, track_id)) = removed_info {
            self.radio_on_track_removed(pos, track_id);
        }
        if was_current {
            let playing = !self.pb.idle && !self.pb.paused;
            self.stop_playback();
            if playing && self.queue.current().is_some() {
                self.play_current(true);
            } else {
                self.show_current_stopped();
            }
        }
        self.queue_changed();
    }

    /// Builds the UI queue snapshot in play order with dual-zone & reason metadata.
    pub(super) fn queue_entries(&self) -> Vec<QueueEntry> {
        let manual_count = self.radio.manual_queue_count.min(self.queue.len());
        self.queue
            .in_play_order()
            .enumerate()
            .map(|(idx, item)| {
                let is_auto = idx >= manual_count;
                let reason = is_auto
                    .then(|| self.radio_reason_for(item.track.id))
                    .flatten();
                let artists = crate::browse::track_artists(&item.track).to_vec();
                let artist_id = artists.first().map_or(0, |a| a.id);
                QueueEntry {
                    uid: item.uid,
                    track_id: item.track.id,
                    album_id: item.track.album_id,
                    artist_id,
                    artists,
                    title: item.track.title.clone(),
                    artist: item.track.artist.clone(),
                    duration: format::duration(item.track.duration_millis),
                    artwork: item.track.artwork_url.clone(),
                    is_auto,
                    reason,
                }
            })
            .collect()
    }

    /// The queue's entries or order changed: tell the UI, refresh the preload.
    pub(super) fn queue_changed(&mut self) {
        self.emit(Event::Queue {
            items: self.queue_entries(),
            current: self.queue.position(),
        });
        self.sync_preload();
        self.cast_queue_changed();
        self.schedule_queue_save();
    }

    pub(super) fn emit_queue_position(&mut self) {
        self.emit(Event::QueuePosition(self.queue.position()));
        self.schedule_queue_save();
    }

    pub(super) fn schedule_queue_save(&mut self) {
        self.queue_revision += 1;
        let revision = self.queue_revision;
        let tx = self.internal.clone();
        tokio::spawn(async move {
            tokio::time::sleep(QUEUE_SAVE_DELAY).await;
            let _ = tx.send(Internal::SaveQueue(revision));
        });
    }

    /// `queue.json` path and contents for the signed-in server.
    pub(super) fn queue_file(&self) -> Option<(PathBuf, Vec<u8>)> {
        let library = self.library.as_ref()?;
        let saved = SavedQueue {
            server: library.client.base_url().to_owned(),
            queue: self.queue.snapshot(),
            radio: self.radio_snapshot(),
        };
        let bytes = serde_json::to_vec(&saved).ok()?;
        Some((self.config.data_dir.join(QUEUE_FILE), bytes))
    }

    pub(super) fn save_position(&mut self) {
        if self.library.is_some() {
            self.session_file
                .set(POSITION_KEY, &self.pb.position_ms.max(0).to_string());
        }
    }

    /// Brings back the queue saved for this server, stopped at the saved
    /// position; playing it resumes the persisted reporting chain.
    pub(super) fn restore_queue(&mut self) {
        let Some(library) = &self.library else { return };
        let path = self.config.data_dir.join(QUEUE_FILE);
        let Some(saved) = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<SavedQueue>(&b).ok())
        else {
            return;
        };
        if saved.server != library.client.base_url() || saved.queue.tracks.is_empty() {
            return;
        }
        self.queue.restore(saved.queue);
        self.radio_restore(saved.radio);
        self.queue_changed();
        let Some(item) = self.queue.current().cloned() else {
            return;
        };
        let position_ms = self
            .session_file
            .get(POSITION_KEY)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
            .clamp(0, item.track.duration_millis.max(0));
        self.pb.resume_at = Some((item.uid, position_ms));
        self.pb.position_ms = position_ms;
        let adapter_track = self.adapter_track();
        if let Some(r) = self.reporter.as_mut() {
            r.act(|a| {
                a.on_queue_restored();
                a.on_track_change(adapter_track);
            });
            #[allow(clippy::cast_precision_loss)]
            r.act(|a| a.on_progress(position_ms as f64 / 1000.0));
        }
        let track = item.track.clone();
        self.emit_now_playing(Some(&track));
        self.emit(Event::Paused(true));
        self.emit_position();
        self.load_lyrics(item.track.id);
        self.update_playing_sources();
    }

    /// Keeps the engine's next file equal to the queue's automatic next
    /// entry, so the engine continues into it without a gap.
    pub(super) fn sync_preload(&mut self) {
        if self.pb.idle || self.pb.loading || self.casting() {
            // Done once the current file has loaded.
            return;
        }
        if self.library.is_none() {
            return;
        }
        let next = match self.queue.auto_next(self.settings.repeat) {
            // The sleep timer stops after this file: the engine must not go on.
            _ if self.pause_at_end() => None,
            Some(Step::Play(pos)) => self.queue.at(pos),
            _ => None,
        };
        let next_transcode = self.transcode();
        let want = next
            .filter(|item| self.want_asio_for(&item.track).0 == self.output.asio_active)
            .map(|item| (item.uid, item.track.id))
            .and_then(|(uid, id)| Some((uid, self.track_url_for(id, next_transcode, 0)?)));
        if want == self.pb.preloaded {
            return;
        }
        self.pb.preloaded = None;
        self.pb.preloaded_transcode = None;
        if let Err(e) = self.engine.clear_upcoming() {
            tracing::warn!("clearing the preloaded file: {e}");
            return;
        }
        if let Some((uid, url)) = want {
            match self.engine.append_for(&url, self.output.asio_active) {
                Ok(()) => {
                    self.pb.preloaded = Some((uid, url));
                    self.pb.preloaded_transcode = next_transcode;
                }
                Err(e) => tracing::warn!("preloading the next file: {e}"),
            }
        }
    }

    /// Loads and starts the current queue entry.
    pub(super) fn play_current(&mut self, user_navigated: bool) {
        self.start_current(user_navigated, true);
    }

    /// Starts the current queue entry: `load` it, or (gapless) it is the
    /// preloaded file the engine has just moved on to.
    pub(super) fn start_current(&mut self, user_navigated: bool, load: bool) {
        let Some(item) = self.queue.current().cloned() else {
            return;
        };
        // 接着听: the entry starts at its saved position (once).
        let start_ms = if load {
            self.queue.take_current_start()
        } else {
            0
        };
        // A restored entry continues where it was left.
        let restored =
            matches!(self.pb.resume_at, Some((uid, ms)) if uid == item.uid && ms > 0 && load);
        let resume_ms = match self.pb.resume_at {
            Some((uid, ms)) if uid == item.uid && ms > 0 && load => Some(ms),
            _ => (start_ms > 0
                && (item.track.duration_millis <= 0 || start_ms < item.track.duration_millis))
                .then_some(start_ms),
        };
        let casting = self.casting();
        let (use_asio, asio_fallback) = if casting {
            (false, false)
        } else {
            self.want_asio_for(&item.track)
        };
        let stream_transcode = if casting {
            None
        } else if load {
            self.transcode()
        } else {
            self.pb.preloaded_transcode
        };
        // Lossy transcodes start the stream there instead of seeking.
        let offset_ms = if !casting && Self::is_lossy_transcode(stream_transcode) {
            resume_ms.unwrap_or(0)
        } else {
            0
        };
        let url = if casting {
            None
        } else {
            let Some(url) = self.track_url_for(item.track.id, stream_transcode, offset_ms) else {
                return;
            };
            Some(url)
        };
        let from_silence = self.pb.idle || self.pb.paused;
        // Sonos: the live stream offset of the started entry.
        let mut cast_offset = 0;
        if load {
            if use_asio {
                self.sync_engine_asio_target();
            }
            let result = match &url {
                Some(url) => self
                    .engine
                    .load_for(url, use_asio)
                    .and_then(|()| self.unpause_engine(from_silence)),
                None => self
                    .cast_start(resume_ms.unwrap_or(0))
                    .map(|offset| cast_offset = offset),
            };
            if let Err(e) = result {
                self.emit(Event::Notice(e));
                return;
            }
            self.output.asio_active = use_asio;
            self.output.active_asio_target = if use_asio {
                self.selected_asio_target()
            } else {
                None
            };
            self.output.asio_fallback = asio_fallback;
        } else if casting {
            // The speaker moved on by itself.
            cast_offset = self.cast_offset();
            self.output.asio_active = false;
            self.output.active_asio_target = None;
            self.output.asio_fallback = false;
        } else {
            // Gapless advance stayed on the engine's existing backend.
            self.output.asio_fallback =
                !self.output.asio_active && self.effective_exclusive_mode() == ExclusiveMode::Asio;
        }
        self.emit_output_mode();
        self.pb.resume_at = None;
        self.pb.idle = false;
        // Sonos: loading until the speaker reports the pushed entry.
        self.pb.loading = !casting || load;
        self.pb.paused = false;
        self.pb.preloaded = None;
        self.pb.current_transcode = stream_transcode;
        self.pb.preloaded_transcode = None;
        self.pb.expected_path = url;
        self.pb.stream_offset_ms = if casting { cast_offset } else { offset_ms };
        self.pb.pending_seek_ms = resume_ms.filter(|_| !casting && offset_ms == 0);
        self.pb.report_seek_ms = resume_ms.filter(|_| !restored);
        self.pb.position_ms = resume_ms.unwrap_or(0);
        self.pb.reported_step = self.pb.position_ms / POSITION_STEP_MS;
        self.pb.engine_duration_ms = 0;
        let position_ms = self.pb.position_ms;

        self.clear_sleep_on_album_change(item.track.album_id);

        // The same entry again without a user action: a repeat-one pass.
        let repeated = !user_navigated && self.pb.started_uid == Some(item.uid);
        self.pb.started_uid = Some(item.uid);
        let adapter_track = self.adapter_track();
        if let Some(r) = self.reporter.as_mut() {
            r.act(|a| {
                if repeated {
                    a.on_repeated();
                }
                #[allow(clippy::cast_precision_loss)]
                a.on_progress(position_ms as f64 / 1000.0);
                a.on_track_change(adapter_track);
                if user_navigated {
                    a.on_user_navigate();
                }
                a.on_status(AdapterStatus::Playing);
            });
        }
        if casting && load {
            // No sound until the speaker has loaded it.
            self.apply_buffering(true);
        }

        let track = item.track.clone();
        self.emit_now_playing(Some(&track));
        self.emit(Event::Paused(false));
        self.emit_position();
        self.save_position();
        self.load_lyrics(track.id);
        self.update_playing_sources();
        self.emit_radio_state();
        self.maybe_trigger_auto_continue(false);
    }

    pub(super) fn duration_ms(&self) -> i64 {
        self.queue
            .current()
            .map(|i| i.track.duration_millis)
            .filter(|d| *d > 0)
            .unwrap_or(self.pb.engine_duration_ms)
    }

    pub(super) fn emit_position(&self) {
        self.emit(Event::Position {
            position_ms: self.pb.position_ms,
            duration_ms: self.duration_ms(),
        });
    }

    /// Playback position (ms, absolute) from the engine or the speaker.
    pub(super) fn apply_position(&mut self, ms: i64) {
        self.pb.position_ms = ms.max(0);
        let save_step = self.pb.position_ms / POSITION_SAVE_STEP_MS;
        if save_step != self.pb.saved_position_step {
            self.pb.saved_position_step = save_step;
            self.save_position();
        }
        let step = self.pb.position_ms / POSITION_STEP_MS;
        if step != self.pb.reported_step {
            self.pb.reported_step = step;
            #[allow(clippy::cast_precision_loss)]
            let seconds = self.pb.position_ms as f64 / 1000.0;
            if let Some(r) = self.reporter.as_mut() {
                r.act(|a| a.on_progress(seconds));
            }
            self.emit_position();
        }
        self.update_active_line();
    }

    /// The renderer paused or plays.
    pub(super) fn apply_paused(&mut self, paused: bool) {
        if self.pb.idle || paused == self.pb.paused {
            return;
        }
        self.pb.paused = paused;
        if let Some(r) = self.reporter.as_mut() {
            let status = if paused {
                AdapterStatus::Paused
            } else {
                AdapterStatus::Playing
            };
            r.act(|a| a.on_status(status));
        }
        if paused {
            self.save_position();
        }
        self.emit(Event::Paused(paused));
    }

    pub(super) fn apply_buffering(&mut self, buffering: bool) {
        self.pb.buffering = buffering;
        if let Some(r) = self.reporter.as_mut() {
            r.act(|a| a.on_buffering(buffering));
        }
        self.emit(Event::Buffering(buffering));
    }

    pub(super) fn clear_buffering(&mut self) {
        if self.pb.buffering {
            self.pb.buffering = false;
            self.emit(Event::Buffering(false));
        }
    }

    /// The current file played to its end.
    pub(super) fn end_of_file(&mut self) {
        if self.pb.idle || self.pb.loading {
            return;
        }
        match self.queue.auto_next(self.settings.repeat) {
            Some(Step::Play(pos)) if self.pause_at_end() => {
                // Sleep timer: on to the next entry, stopped.
                self.stop_playback();
                self.queue.go(pos);
                self.show_current_stopped();
                self.emit_queue_position();
                self.set_sleep_timer(None);
            }
            Some(Step::Play(pos)) => {
                let gapless = self
                    .pb
                    .preloaded
                    .as_ref()
                    .is_some_and(|(uid, _)| self.queue.at(pos).map(|i| i.uid) == Some(*uid));
                self.queue.go(pos);
                // Gapless: the engine already moved on to the preload.
                self.start_current(false, !gapless);
                self.emit_queue_position();
            }
            step @ Some(Step::Stop(_)) => {
                if !self.pause_at_end() && self.maybe_trigger_auto_continue(true) {
                    return;
                }
                self.step(step, false);
            }
            None => {}
        }
    }

    /// The current file cannot play.
    pub(super) fn playback_failed(&mut self, error: String) {
        if self.pb.idle {
            return;
        }
        if self.output.asio_active && error.contains("Opus is not supported") {
            // Only remember tracks whose *original* stream is Opus; never blacklist
            // a lossless track just because an Ogg transcode was active.
            if self.pb.current_transcode.is_none()
                && let Some(item) = self.queue.current()
            {
                self.output.opus_fallback_tracks.insert(item.track.id);
            }
            let position = self.pb.position_ms;
            let _ = self.engine.stop();
            self.output.asio_active = false;
            self.output.active_asio_target = None;
            self.output.asio_fallback = true;
            self.emit_output_mode();
            self.hand_over(false, position);
            self.play_current(false);
            return;
        }
        // Keeps the engine from moving on into the preload.
        let _ = self.engine.stop();
        self.reset_transport(TransportReset::failure());
        if let Some(r) = self.reporter.as_mut() {
            r.act(|a| {
                a.on_error();
                a.on_status(AdapterStatus::Paused);
            });
        }
        self.emit(Event::Paused(true));
        self.emit(Event::Notice(format!("播放失败：{error}")));
    }

    pub(super) fn engine_event(&mut self, event: EngineEvent) {
        if self.casting() {
            // The engine is stopped while a Sonos room plays; only the
            // (local) volume setting follows it.
            if let EngineEvent::Volume(volume) = event {
                self.update_settings(|s| s.volume = volume.clamp(0.0, 100.0));
            }
            return;
        }
        match event {
            EngineEvent::TimePos(seconds) => {
                if self.pb.idle || self.pb.loading {
                    return;
                }
                #[allow(clippy::cast_possible_truncation)]
                let ms = (seconds * 1000.0).round() as i64 + self.pb.stream_offset_ms;
                self.apply_position(ms);
            }
            EngineEvent::Duration(seconds) => {
                if self.pb.loading {
                    return;
                }
                #[allow(clippy::cast_possible_truncation)]
                let ms = (seconds * 1000.0).round() as i64;
                self.pb.engine_duration_ms = ms;
                self.emit_position();
            }
            EngineEvent::Paused(paused) => {
                self.pb.fade.engine_paused(paused);
                self.apply_paused(paused);
            }
            EngineEvent::Buffering(buffering) => self.apply_buffering(buffering),
            EngineEvent::Volume(volume) => {
                self.update_settings(|s| s.volume = volume.clamp(0.0, 100.0));
                self.emit(Event::Volume(volume));
            }
            EngineEvent::FileLoaded { path } => {
                if !self.pb.loading {
                    return;
                }
                if let (Some(path), Some(expected)) = (&path, &self.pb.expected_path)
                    && path != expected
                {
                    // The engine moved on to a stale preload (the queue
                    // changed right at the track boundary): load the right one.
                    tracing::debug!(
                        path = %api::redact_media_token(path),
                        expected = %api::redact_media_token(expected),
                        "unexpected file, reloading"
                    );
                    if let Err(e) = self.engine.load_for(expected, self.output.asio_active) {
                        self.emit(Event::Notice(e));
                    }
                    return;
                }
                self.pb.loading = false;
                if let Some(ms) = self.pb.pending_seek_ms.take() {
                    #[allow(clippy::cast_precision_loss)]
                    if let Err(e) = self.engine.seek(ms as f64 / 1000.0) {
                        tracing::warn!("seeking to the resumed position: {e}");
                    }
                }
                if let Some(ms) = self.pb.report_seek_ms.take()
                    && let Some(r) = self.reporter.as_mut()
                {
                    #[allow(clippy::cast_precision_loss)]
                    r.act(|a| a.on_seek(ms as f64 / 1000.0));
                }
                self.sync_preload();
            }
            EngineEvent::EndFile { reason, error } => match reason {
                EngineEnd::Stop => {}
                EngineEnd::Eof => self.end_of_file(),
                EngineEnd::Error => {
                    self.playback_failed(error.unwrap_or_else(|| "未知错误".into()));
                }
            },
        }
    }
}
