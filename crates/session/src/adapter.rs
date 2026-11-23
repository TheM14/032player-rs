//! Translates player signals into playback session calls.
//!
//! Port of `032musicplayer/src/renderer/api/music032/music032-playback-adapter.ts`.
//!
//! Batching: one player action emits several signals in an order that does
//! not match their meaning (a "next" resets the position to 0 and changes the
//! track, and only afterwards is marked as a user action). Signals are
//! therefore only recorded; the owner calls [`PlaybackAdapter::flush`] once
//! per action, which interprets the whole batch at once.
//!
//! Invariants:
//!  - an end event always carries the ending track's own last observed
//!    position and duration, never the next track's (or the reset 0);
//!  - a track gets a session only once it actually plays (no sessions for
//!    tracks skipped through while paused);
//!  - repeat-one replays end the current play (completed) and start a fresh
//!    session without resume, so the server counts each pass;
//!  - seeking (including "previous" restarting the track) stays in the same
//!    session and never counts a new play.

use std::sync::Arc;

use crate::env::LiveProbe;
use crate::session::{PlaybackSession, SessionOptions};
use crate::types::{EndReason, EventPosition, PlayOptions, PlaybackContext, PlaybackState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterStatus {
    Paused,
    Playing,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterTrack {
    /// Daily mix attribution of this queue item (only set when the server supports it).
    pub context: Option<PlaybackContext>,
    pub duration_millis: u64,
    /// 032 track id.
    pub track_id: i64,
    /// Queue item id; distinguishes the same song queued twice.
    pub unique_id: String,
}

/// Positions this close to the end count as a natural finish (covers crossfade pre-roll).
pub const NEAR_END_MS: u64 = 20_000;
/// A jump from near the end to before this position without a seek is a loop restart.
const LOOP_RESTART_MAX_MS: u64 = 3_000;
/// Within this window after a seek, a position jump is the seek, not a loop.
const SEEK_GRACE_MS: i64 = 2_000;

#[derive(Default)]
struct Pending {
    intent: bool,
    position: Option<u64>,
    repeated: bool,
    seek: Option<u64>,
    status: Option<AdapterStatus>,
    /// `Some(None)` = changed to "nothing / not a 032 song".
    track: Option<Option<AdapterTrack>>,
}

pub type NowFn = Box<dyn Fn() -> i64 + Send>;

pub struct PlaybackAdapter {
    session: PlaybackSession,
    live: LiveProbe,
    now: NowFn,
    current: Option<AdapterTrack>,
    status: AdapterStatus,
    last_position_ms: u64,
    last_seek_at: Option<i64>,
    buffering: bool,
    /// The next start for `current` resumes the persisted chain (restart, error retry).
    resume_pending: bool,
    pending: Pending,
}

fn to_ms(sec: f64) -> u64 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let ms = (sec * 1000.0).round().max(0.0) as u64;
    ms
}

fn is_near_end(track: &AdapterTrack, position_ms: u64) -> bool {
    track.duration_millis > 0 && position_ms + NEAR_END_MS >= track.duration_millis
}

impl PlaybackAdapter {
    /// Builds the session with this adapter as its position / duration probe.
    pub fn new(mut options: SessionOptions, now: NowFn) -> Self {
        let live = LiveProbe::default();
        options.probe = Some(Arc::new(live.clone()));
        Self {
            session: PlaybackSession::new(options),
            live,
            now,
            current: None,
            status: AdapterStatus::Stopped,
            last_position_ms: 0,
            last_seek_at: None,
            buffering: false,
            resume_pending: false,
            pending: Pending::default(),
        }
    }

    pub fn session(&self) -> &PlaybackSession {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut PlaybackSession {
        &mut self.session
    }

    pub fn duration_millis(&self) -> u64 {
        self.current.as_ref().map_or(0, |t| t.duration_millis)
    }

    /// Live position of the current track, used for heartbeats.
    pub fn position_millis(&self) -> u64 {
        self.last_position_ms
    }

    fn set_position(&mut self, ms: u64) {
        self.last_position_ms = ms;
        self.live.set_position_millis(ms);
    }

    fn set_current(&mut self, track: Option<AdapterTrack>) {
        self.live
            .set_duration_millis(track.as_ref().map_or(0, |t| t.duration_millis));
        self.current = track;
    }

    fn session_is_for_current(&self) -> bool {
        self.current.as_ref().is_some_and(|t| {
            self.session.has_active_session() && self.session.current_track_id() == Some(t.track_id)
        })
    }

    fn end_current(&mut self, reason: EndReason) {
        if !self.session_is_for_current() {
            return;
        }
        let duration = self.duration_millis();
        self.session.end(
            reason,
            EventPosition {
                duration_millis: Some(duration),
                position_millis: Some(self.last_position_ms),
            },
        );
    }

    fn start_current(&mut self) {
        let Some(current) = &self.current else { return };
        let track_id = current.track_id;
        let opts = PlayOptions {
            context: current.context.clone(),
            duration_millis: Some(current.duration_millis),
            initial_state: Some(if self.buffering {
                PlaybackState::Buffering
            } else {
                PlaybackState::Playing
            }),
            position_millis: Some(self.last_position_ms),
            reason: None,
        };
        if self.resume_pending {
            self.resume_pending = false;
            self.session.restore(track_id, opts);
        } else {
            self.session.play(track_id, opts);
        }
    }

    fn current_near_end(&self) -> bool {
        self.current
            .as_ref()
            .is_some_and(|t| is_near_end(t, self.last_position_ms))
    }

    fn apply_status(&mut self, next: AdapterStatus) {
        let previous = self.status;
        self.status = next;
        if next == AdapterStatus::Stopped {
            self.buffering = false;
        }
        if self.current.is_none() || next == previous {
            return;
        }
        let pos = Some(self.last_position_ms);
        match next {
            AdapterStatus::Playing => {
                if self.session_is_for_current() {
                    self.session.notify_playing(pos);
                } else {
                    self.start_current();
                }
            }
            AdapterStatus::Paused => {
                if self.session_is_for_current() {
                    self.session.notify_paused(pos);
                }
            }
            AdapterStatus::Stopped => {
                // End of queue (near the end) or an explicit stop.
                let reason = if self.current_near_end() {
                    EndReason::Completed
                } else {
                    EndReason::Stopped
                };
                self.end_current(reason);
            }
        }
    }

    /// Interprets every signal recorded since the last flush.
    pub fn flush(&mut self) {
        let p = std::mem::take(&mut self.pending);

        let track_changed = match &p.track {
            None => false,
            Some(next) => match (&self.current, next) {
                (Some(cur), Some(next)) => cur.unique_id != next.unique_id,
                (None, None) => false,
                _ => true,
            },
        };

        if track_changed {
            if self.current.is_some() {
                // A user action in the same batch (next/prev/play from list) is a
                // skip; otherwise the player advanced on its own.
                let reason = if p.intent {
                    EndReason::Skipped
                } else if self.current_near_end() {
                    EndReason::Completed
                } else {
                    EndReason::Stopped
                };
                self.end_current(reason);
                // A pending resume belongs to the track it was armed for; only the
                // initial assignment after mount (current == None) keeps it.
                self.resume_pending = false;
            }
            self.set_current(p.track.flatten());
            // The batch's position is the reset, not progress of the new track.
            self.set_position(0);
            self.buffering = false;
            if let Some(status) = p.status {
                self.status = status;
            }
            if self.current.is_some() && self.status == AdapterStatus::Playing {
                self.start_current();
            }
            return;
        }

        let Some(current) = &self.current else {
            if let Some(status) = p.status {
                if status == AdapterStatus::Stopped {
                    self.buffering = false;
                }
                self.status = status;
            }
            return;
        };

        // Same track. A stop is judged on the position before the reset.
        if p.status == Some(AdapterStatus::Stopped) {
            self.apply_status(AdapterStatus::Stopped);
            let pos = p.seek.or(p.position).unwrap_or(self.last_position_ms);
            self.set_position(pos);
            return;
        }

        let t = (self.now)();
        let wrapped = match p.position {
            Some(position) if p.seek.is_none() => {
                self.last_seek_at.is_none_or(|at| t - at > SEEK_GRACE_MS)
                    && is_near_end(current, self.last_position_ms)
                    && position < LOOP_RESTART_MAX_MS
                    && self.last_position_ms.saturating_sub(position) > NEAR_END_MS / 2
            }
            _ => false,
        };

        if p.repeated || wrapped {
            // Repeat-one: mpv signals it; a looping engine only wraps the playhead.
            self.end_current(EndReason::Completed);
            self.set_position(0);
            self.buffering = false;
            if let Some(status) = p.status {
                self.status = status;
            }
            if self.status == AdapterStatus::Playing {
                self.start_current();
            }
            return;
        }

        if let Some(seek) = p.seek {
            self.last_seek_at = Some(t);
            self.set_position(seek);
            if self.session_is_for_current() {
                self.session.notify_seek(Some(seek));
            }
        } else if let Some(position) = p.position {
            self.set_position(position);
        }

        if let Some(status) = p.status {
            self.apply_status(status);
        }
    }

    /// Buffering state from the engine (stall / recovered).
    pub fn on_buffering(&mut self, is_buffering: bool) {
        self.flush();
        self.buffering = is_buffering;
        if !self.session_is_for_current() || self.status != AdapterStatus::Playing {
            return;
        }
        let pos = Some(self.last_position_ms);
        if is_buffering {
            self.session.notify_buffering(pos);
        } else {
            self.session.notify_playing(pos);
        }
    }

    /// App / window is closing.
    pub fn on_close(&mut self) {
        self.flush();
        if !self.session_is_for_current() {
            return;
        }
        let duration = self.duration_millis();
        self.session.suspend(EventPosition {
            duration_millis: Some(duration),
            position_millis: Some(self.last_position_ms),
        });
        self.resume_pending = true;
    }

    /// Engine reported a playback failure for the current track.
    pub fn on_error(&mut self) {
        self.flush();
        self.buffering = false;
        if self.current.is_none() {
            return;
        }
        self.end_current(EndReason::Error);
        // A retry of the same track continues the interrupted chain.
        self.resume_pending = true;
    }

    pub fn on_progress(&mut self, position_sec: f64) {
        self.pending.position = Some(to_ms(position_sec));
    }

    /// Queue restored on startup: the first play resumes the persisted chain.
    pub fn on_queue_restored(&mut self) {
        self.resume_pending = true;
    }

    /// Repeat-one replay signalled by the player.
    pub fn on_repeated(&mut self) {
        self.pending.repeated = true;
    }

    pub fn on_seek(&mut self, position_sec: f64) {
        self.pending.seek = Some(to_ms(position_sec));
    }

    pub fn on_status(&mut self, next: AdapterStatus) {
        self.pending.status = Some(next);
    }

    /// Current queue item changed; pass `None` for "nothing / not a 032 song".
    pub fn on_track_change(&mut self, next: Option<AdapterTrack>) {
        self.pending.track = Some(next);
    }

    /// Explicit user navigation (next / previous / play from a list).
    pub fn on_user_navigate(&mut self) {
        self.pending.intent = true;
    }
}
