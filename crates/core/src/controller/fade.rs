//! Volume fade on play / pause (Electron `mpv-player.tsx`
//! `audioFadeOnStatusChange`: 300 ms, on by default). The fade runs on the
//! engine's separate fade gain, so the user volume (and `settings.json`)
//! never changes. Track changes while playing do not fade.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::task::JoinHandle;

use super::{Core, Event};
use crate::engine::Engine;

/// A full fade (silence to full level or back).
const FADE_MS: u64 = 300;
const FADE_STEP_MS: u64 = 10;

pub(super) struct Fader {
    /// Fade progress, 0 (silent) to 1 (full); shared with the running fade.
    level: Arc<Mutex<f64>>,
    /// Invalidates a cancelled ramp. Checked while holding `level`, so no
    /// stale task can issue another engine command after `cancel` returns.
    generation: Arc<AtomicU64>,
    /// The running fade and whether it ends paused.
    running: Option<(JoinHandle<()>, bool)>,
}

impl Default for Fader {
    fn default() -> Self {
        Self {
            level: Arc::new(Mutex::new(1.0)),
            generation: Arc::new(AtomicU64::new(0)),
            running: None,
        }
    }
}

impl Fader {
    fn level(&self) -> f64 {
        *self.level.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_level(&self, level: f64) {
        *self.level.lock().unwrap_or_else(|e| e.into_inner()) = level;
    }

    /// Stops the running fade where it is and returns the new generation.
    fn cancel(&mut self) -> u64 {
        // A ramp holds the same lock around its generation check and engine
        // call. Taking it here bounds the wait to an already-running call.
        let _level = self.level.lock().unwrap_or_else(|e| e.into_inner());
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        drop(_level);
        if let Some((task, _)) = self.running.take() {
            task.abort();
        }
        generation
    }

    /// Pause state the last fade heads to. A fade-out counts until the
    /// engine reports the pause (see [`Fader::engine_paused`]).
    pub(super) fn target(&self) -> Option<bool> {
        self.running.as_ref().map(|&(_, paused)| paused)
    }

    /// The engine reported its pause state: a fade that ended there is done.
    pub(super) fn engine_paused(&mut self, paused: bool) {
        let state = self
            .running
            .as_ref()
            .map(|(task, target)| (task.is_finished(), *target));
        match (paused, state) {
            // Unpausing starts, rather than finishes, a fade-in. Keep its
            // handle so an immediate pause can still cancel that ramp.
            (false, Some((false, false))) => {}
            // An external pause during fade-in must invalidate that ramp.
            (true, Some((false, false))) => {
                self.cancel();
            }
            // A fade-out is complete once its pause arrives. Completed tasks
            // never need to remain as the effective transport target.
            (true, Some((_, true))) | (_, Some((true, _))) => self.running = None,
            _ => {}
        }
    }
}

impl Drop for Fader {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Engine gain for a fade level. Cubic like mpv's volume curve, so the fade
/// sounds like Electron's linear ramp of the mpv `volume`.
fn gain(level: f64) -> f64 {
    level.clamp(0.0, 1.0).powi(3)
}

/// Ramps from the current level to `to`, then pauses when `pause`.
fn spawn_ramp(
    engine: Arc<dyn Engine>,
    level: Arc<Mutex<f64>>,
    generation: Arc<AtomicU64>,
    expected_generation: u64,
    to: f64,
    pause: bool,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(FADE_STEP_MS));
        #[allow(clippy::cast_precision_loss)]
        let step = FADE_STEP_MS as f64 / FADE_MS as f64;
        loop {
            tick.tick().await;
            let now = {
                let mut level = level.lock().unwrap_or_else(|e| e.into_inner());
                if generation.load(Ordering::Relaxed) != expected_generation {
                    return;
                }
                // Snap the last step so rounding never leaves it just short.
                *level = if (*level - to).abs() <= step * 1.5 {
                    to
                } else if *level < to {
                    *level + step
                } else {
                    *level - step
                };
                let now = *level;
                // Keep the lock through the engine call: `cancel` takes the
                // same lock before invalidating this generation.
                if let Err(e) = engine.set_fade_gain(gain(now)) {
                    tracing::debug!("fade gain: {e}");
                    if pause {
                        break;
                    }
                    return;
                }
                now
            };
            #[allow(clippy::float_cmp)]
            if now == to {
                break;
            }
        }
        if pause {
            let _level = level.lock().unwrap_or_else(|e| e.into_inner());
            if generation.load(Ordering::Relaxed) != expected_generation {
                return;
            }
            if let Err(e) = engine.set_paused(true) {
                tracing::warn!("pause after fade: {e}");
            }
        }
    })
}

impl Core {
    /// The pause state playback is in or heading to (a running fade-out
    /// still plays but counts as paused).
    pub(super) fn effective_paused(&self) -> bool {
        self.pb.fade.target().unwrap_or(self.pb.paused)
    }

    /// Pauses the engine, fading out first when enabled.
    pub(super) fn pause_engine(&mut self) -> Result<(), String> {
        if self.casting() {
            return self.cast_pause(true);
        }
        let generation = self.pb.fade.cancel();
        if !self.settings.fade_on_pause
            || self
                .engine
                .set_fade_gain(gain(self.pb.fade.level()))
                .is_err()
        {
            return self.engine.set_paused(true);
        }
        let task = spawn_ramp(
            self.engine.clone(),
            self.pb.fade.level.clone(),
            self.pb.fade.generation.clone(),
            generation,
            0.0,
            true,
        );
        self.pb.fade.running = Some((task, true));
        // The icon flips now; the engine's `Paused` follows after the fade.
        self.emit(Event::Paused(true));
        Ok(())
    }

    /// Unpauses the engine, fading in when enabled. `from_silence`: the
    /// engine was paused or stopped, so the fade starts at silence.
    pub(super) fn unpause_engine(&mut self, from_silence: bool) -> Result<(), String> {
        if self.casting() {
            return self.cast_pause(false);
        }
        let generation = self.pb.fade.cancel();
        if !self.settings.fade_on_pause {
            self.restore_gain();
            return self.engine.set_paused(false);
        }
        if from_silence {
            self.pb.fade.set_level(0.0);
        }
        let level = self.pb.fade.level();
        if self.engine.set_fade_gain(gain(level)).is_err() {
            return self.engine.set_paused(false);
        }
        self.engine.set_paused(false)?;
        if level < 1.0 {
            let task = spawn_ramp(
                self.engine.clone(),
                self.pb.fade.level.clone(),
                self.pb.fade.generation.clone(),
                generation,
                1.0,
                false,
            );
            self.pb.fade.running = Some((task, false));
        }
        if !self.pb.paused {
            // A fade-out was cancelled: the engine never paused.
            self.emit(Event::Paused(false));
        }
        Ok(())
    }

    /// Back to full level (the fade was switched off mid-way).
    pub(super) fn restore_gain(&mut self) {
        self.pb.fade.cancel();
        if self.pb.fade.level() < 1.0 {
            self.pb.fade.set_level(1.0);
            let _ = self.engine.set_fade_gain(1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn early_unpause_event_keeps_fade_in_cancellable() {
        let mut fader = Fader::default();
        let task = tokio::spawn(std::future::pending::<()>());
        fader.running = Some((task, false));

        // mpv reports the unpause at the start of the 300 ms ramp, not when
        // it reaches full volume. This must not detach the running task.
        fader.engine_paused(false);
        assert_eq!(fader.target(), Some(false));

        // A following pause cancels the old ramp instead of letting two
        // tasks race to set gain in opposite directions.
        fader.engine_paused(true);
        assert_eq!(fader.target(), None);
    }
}
