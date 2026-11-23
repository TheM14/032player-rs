//! Sleep timer state: countdown minutes task, end-of-track, and end-of-album.

use std::time::Duration;

use tokio::task::AbortHandle;

use super::{Core, Event, Internal, SLEEP_TICK_MS, Sleep, SleepStatus, SleepTimer};
use crate::queue::{Repeat, Step};

/// Active sleep timer state and background tick task handle.
#[derive(Default)]
pub(super) struct SleepState {
    pub(super) active: Option<Sleep>,
    /// Bumped per sleep timer start / cancel; stale ticks are ignored.
    generation: u64,
    /// Handle for the active minutes sleep-timer task so it is aborted when cancelled.
    task: Option<AbortHandle>,
}

impl SleepState {
    /// Cancels any active sleep timer and aborts its background tick task.
    pub(super) fn cancel(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.generation += 1;
        self.active = None;
    }
}

impl Drop for SleepState {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl Core {
    pub(super) fn set_sleep_timer(&mut self, timer: Option<SleepTimer>) {
        self.sleep.cancel();
        self.sleep.active = timer.map(|timer| Sleep {
            timer,
            remaining_ms: match timer {
                SleepTimer::Minutes(m) => i64::from(m) * 60_000,
                _ => 0,
            },
            album_id: self.queue.current().map(|i| i.track.album_id),
        });
        if let Some(SleepTimer::Minutes(_)) = timer {
            let generation = self.sleep.generation;
            let tx = self.internal.clone();
            let handle = tokio::spawn(async move {
                let mut tick =
                    tokio::time::interval(Duration::from_millis(SLEEP_TICK_MS.unsigned_abs()));
                tick.tick().await;
                loop {
                    tick.tick().await;
                    if tx.send(Internal::SleepTick(generation)).is_err() {
                        break;
                    }
                }
            });
            self.sleep.task = Some(handle.abort_handle());
        }
        self.emit_sleep();
        self.emit_radio_state();
        // Whether the engine or Sonos may continue into the next file changed.
        self.sync_preload();
        self.cast_play_mode();
        if timer.is_none() {
            self.maybe_trigger_auto_continue(false);
        }
    }

    pub(super) fn emit_sleep(&self) {
        let status = self.sleep.active.as_ref().map(|s| SleepStatus {
            timer: s.timer,
            remaining_ms: matches!(s.timer, SleepTimer::Minutes(_)).then_some(s.remaining_ms),
        });
        self.emit(Event::SleepTimer(status));
    }

    pub(super) fn stops_at_track_change(&self) -> bool {
        self.sleep
            .active
            .as_ref()
            .is_some_and(|s| s.timer == SleepTimer::EndOfTrack)
    }

    /// Playback stops when the current file ends (sleep timer).
    pub(super) fn pause_at_end(&self) -> bool {
        let Some(sleep) = &self.sleep.active else {
            return false;
        };
        match sleep.timer {
            SleepTimer::EndOfTrack => true,
            SleepTimer::EndOfAlbum => {
                let next_album = match self.queue.auto_next(self.settings.repeat) {
                    Some(Step::Play(pos)) => self.queue.at(pos).map(|i| i.track.album_id),
                    _ => None,
                };
                // Last track of the album: the next one (if any) is elsewhere.
                // Repeat-one replays the same track and so never ends the album.
                self.settings.repeat != Repeat::One && next_album != sleep.album_id
            }
            SleepTimer::Minutes(_) => false,
        }
    }

    pub(super) fn clear_sleep_on_album_change(&mut self, album_id: i64) {
        if let Some(sleep) = &self.sleep.active
            && sleep.timer == SleepTimer::EndOfAlbum
            && sleep.album_id != Some(album_id)
        {
            // Moved on to another album: the timer no longer applies.
            self.set_sleep_timer(None);
        }
    }

    pub(super) fn clear_sleep_on_stop(&mut self) {
        if matches!(
            self.sleep.active.as_ref().map(|s| s.timer),
            Some(SleepTimer::EndOfTrack | SleepTimer::EndOfAlbum)
        ) {
            self.set_sleep_timer(None);
        }
    }

    pub(super) fn on_sleep_tick(&mut self, generation: u64) {
        if generation != self.sleep.generation {
            return;
        }
        let playing = !self.pb.idle && !self.pb.paused;
        let Some(sleep) = self.sleep.active.as_mut() else {
            return;
        };
        if !playing {
            return;
        }
        sleep.remaining_ms -= SLEEP_TICK_MS;
        if sleep.remaining_ms > 0 {
            self.emit_sleep();
            return;
        }
        self.set_sleep_timer(None);
        if let Err(e) = self.pause_engine() {
            self.emit(Event::Notice(e));
        }
    }
}
