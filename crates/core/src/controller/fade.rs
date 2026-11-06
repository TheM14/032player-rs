//! Volume fade on play / pause (Electron `mpv-player.tsx`
//! `audioFadeOnStatusChange`: 300 ms, on by default). The fade runs on the
//! engine's separate fade gain, so the user volume (and `settings.json`)
//! never changes. Track changes while playing do not fade.

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
    /// The running fade and whether it ends paused.
    running: Option<(JoinHandle<()>, bool)>,
}

impl Default for Fader {
    fn default() -> Self {
        Self {
            level: Arc::new(Mutex::new(1.0)),
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

    /// Stops the running fade where it is.
    fn cancel(&mut self) {
        if let Some((task, _)) = self.running.take() {
            task.abort();
        }
    }

    /// Pause state the last fade heads to. A fade-out counts until the
    /// engine reports the pause (see [`Fader::engine_paused`]).
    pub(super) fn target(&self) -> Option<bool> {
        self.running.as_ref().map(|&(_, paused)| paused)
    }

    /// The engine reported its pause state: a fade that ended there is done.
    pub(super) fn engine_paused(&mut self, paused: bool) {
        if self.target() == Some(paused) {
            self.running = None;
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
                // Snap the last step so rounding never leaves it just short.
                *level = if (*level - to).abs() <= step * 1.5 {
                    to
                } else if *level < to {
                    *level + step
                } else {
                    *level - step
                };
                *level
            };
            if let Err(e) = engine.set_fade_gain(gain(now)) {
                tracing::debug!("fade gain: {e}");
                break;
            }
            #[allow(clippy::float_cmp)]
            if now == to {
                break;
            }
        }
        if pause && let Err(e) = engine.set_paused(true) {
            tracing::warn!("pause after fade: {e}");
        }
    })
}

impl Core {
    /// The pause state playback is in or heading to (a running fade-out
    /// still plays but counts as paused).
    pub(super) fn effective_paused(&self) -> bool {
        self.fade.target().unwrap_or(self.paused)
    }

    /// Pauses the engine, fading out first when enabled.
    pub(super) fn pause_engine(&mut self) -> Result<(), String> {
        self.fade.cancel();
        if !self.settings.fade_on_pause
            || self.engine.set_fade_gain(gain(self.fade.level())).is_err()
        {
            return self.engine.set_paused(true);
        }
        let task = spawn_ramp(self.engine.clone(), self.fade.level.clone(), 0.0, true);
        self.fade.running = Some((task, true));
        // The icon flips now; the engine's `Paused` follows after the fade.
        self.emit(Event::Paused(true));
        Ok(())
    }

    /// Unpauses the engine, fading in when enabled. `from_silence`: the
    /// engine was paused or stopped, so the fade starts at silence.
    pub(super) fn unpause_engine(&mut self, from_silence: bool) -> Result<(), String> {
        self.fade.cancel();
        if !self.settings.fade_on_pause {
            self.restore_gain();
            return self.engine.set_paused(false);
        }
        if from_silence {
            self.fade.set_level(0.0);
        }
        let level = self.fade.level();
        if self.engine.set_fade_gain(gain(level)).is_err() {
            return self.engine.set_paused(false);
        }
        self.engine.set_paused(false)?;
        if level < 1.0 {
            let task = spawn_ramp(self.engine.clone(), self.fade.level.clone(), 1.0, false);
            self.fade.running = Some((task, false));
        }
        if !self.paused {
            // A fade-out was cancelled: the engine never paused.
            self.emit(Event::Paused(false));
        }
        Ok(())
    }

    /// Back to full level (the fade was switched off mid-way).
    pub(super) fn restore_gain(&mut self) {
        self.fade.cancel();
        if self.fade.level() < 1.0 {
            self.fade.set_level(1.0);
            let _ = self.engine.set_fade_gain(1.0);
        }
    }
}
