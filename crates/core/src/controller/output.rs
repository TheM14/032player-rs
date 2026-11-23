//! Audio output device, exclusive / ASIO routing, and backend handover.

use std::collections::HashSet;

use api::{Track, Transcode};

use super::{Core, Event, Internal};
use crate::engine::{AudioDevice, Engine};
use crate::settings::ExclusiveMode;

/// Output routing and ASIO state owned by [`Core`].
#[derive(Default)]
pub(super) struct OutputState {
    /// An output device or exclusive mode change is running.
    pub(super) busy: bool,
    pub(super) audio_devices: Vec<AudioDevice>,
    pub(super) asio_available: bool,
    /// Active engine stream was started in ASIO mode.
    pub(super) asio_active: bool,
    /// Current track fell back from ASIO to WASAPI (e.g. Opus).
    pub(super) asio_fallback: bool,
    /// Track IDs confirmed to contain Opus at runtime (fallback on subsequent plays).
    pub(super) opus_fallback_tracks: HashSet<i64>,
}

impl OutputState {
    /// Clears active ASIO stream flags, returning whether either changed.
    pub(super) fn clear_asio_stream(&mut self) -> bool {
        let changed = self.asio_active || self.asio_fallback;
        self.asio_active = false;
        self.asio_fallback = false;
        changed
    }
}

/// Returns true when the stream for `track` uses Opus and must fall back to WASAPI.
pub(super) fn is_opus_track(track: &Track, transcode: Option<Transcode>) -> bool {
    match transcode {
        Some(Transcode::Ogg(_)) => true,
        Some(_) => false,
        None => {
            track
                .codec
                .as_deref()
                .is_some_and(|c| c.trim().eq_ignore_ascii_case("opus"))
                || track.container.trim().eq_ignore_ascii_case("opus")
                || track.mime_type.to_ascii_lowercase().contains("opus")
                || track
                    .relative_path
                    .trim()
                    .to_ascii_lowercase()
                    .ends_with(".opus")
        }
    }
}

impl Core {
    /// Runs an output change (device, exclusive) on a blocking thread.
    pub(super) fn change_output(
        &mut self,
        change: impl FnOnce(&dyn Engine) -> Result<(), String> + Send + 'static,
        done: impl FnOnce(Result<(), String>) -> Internal + Send + 'static,
    ) {
        self.output.busy = true;
        let engine = self.engine.clone();
        let tx = self.internal.clone();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || change(engine.as_ref()))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            let _ = tx.send(done(result));
        });
    }

    pub(super) fn refresh_audio_devices(&self) {
        let engine = self.engine.clone();
        let tx = self.internal.clone();
        tokio::spawn(async move {
            let res = tokio::task::spawn_blocking(move || engine.audio_devices())
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            let _ = tx.send(Internal::AudioDevicesLoaded(res));
        });
    }

    pub(super) fn set_audio_device(&mut self, device: Option<String>) {
        let device = device.filter(|d| !d.is_empty() && d != "auto");
        if self.output.busy || device == self.settings.audio_device {
            self.emit(Event::AudioDevice(self.settings.audio_device.clone()));
            return;
        }
        let name = device.clone().unwrap_or_else(|| "auto".into());
        self.change_output(
            move |e| e.set_audio_device(&name),
            move |r| Internal::AudioDeviceSet(device, r),
        );
    }

    pub(super) fn set_exclusive(&mut self, enabled: bool) {
        if self.output.busy || enabled == self.settings.exclusive {
            self.emit(Event::Exclusive(self.settings.exclusive));
            self.emit_output_mode();
            return;
        }
        self.change_output(
            move |e| e.set_exclusive(enabled),
            move |r| Internal::ExclusiveSet(enabled, r),
        );
    }

    pub(super) fn set_exclusive_mode(&mut self, mode: ExclusiveMode) {
        if self.output.busy {
            self.emit_output_mode();
            return;
        }
        if mode == ExclusiveMode::Asio && !self.output.asio_available {
            self.emit(Event::Notice("当前输出设备不支持 ASIO 模式".into()));
            self.emit_output_mode();
            return;
        }
        if mode == self.settings.exclusive_mode() {
            self.emit_output_mode();
            return;
        }
        self.change_output(
            move |e| e.set_exclusive_mode(mode),
            move |r| Internal::ExclusiveModeSet(mode, r),
        );
    }

    pub(super) fn open_asio_control_panel(&self) {
        let engine = self.engine.clone();
        let tx = self.internal.clone();
        tokio::spawn(async move {
            let res = tokio::task::spawn_blocking(move || engine.open_asio_control_panel())
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            let _ = tx.send(Internal::AsioControlPanelOpened(res));
        });
    }

    pub(super) fn effective_exclusive_mode(&self) -> ExclusiveMode {
        if !self.settings.exclusive {
            ExclusiveMode::Off
        } else if self.settings.asio && self.output.asio_available {
            ExclusiveMode::Asio
        } else {
            ExclusiveMode::Wasapi
        }
    }

    pub(super) fn emit_output_mode(&self) {
        self.emit(Event::OutputMode {
            mode: self.effective_exclusive_mode(),
            asio_available: self.output.asio_available,
            asio_fallback: self.output.asio_fallback,
        });
    }

    pub(super) fn refresh_asio_availability(&mut self) {
        self.output.asio_available = self.engine.supports_asio(
            &self.output.audio_devices,
            self.settings.audio_device.as_deref(),
        );
    }

    pub(super) fn want_asio_for(&self, track: &Track) -> (bool, bool) {
        if self.effective_exclusive_mode() != ExclusiveMode::Asio || self.casting() {
            return (false, false);
        }
        if is_opus_track(track, self.transcode())
            || self.output.opus_fallback_tracks.contains(&track.id)
        {
            (false, true)
        } else {
            (true, false)
        }
    }

    pub(super) fn maybe_handover_backend(&mut self) {
        if self.pb.idle || self.casting() {
            return;
        }
        let (need_asio, _) = self
            .queue
            .current()
            .map_or((false, false), |i| self.want_asio_for(&i.track));
        if need_asio != self.output.asio_active {
            let was_playing = !self.pb.paused;
            let position = self.pb.position_ms;
            let _ = self.engine.stop();
            self.output.clear_asio_stream();
            self.hand_over(false, position);
            if was_playing {
                self.play_current(false);
            }
        } else {
            self.sync_preload();
        }
    }

    pub(super) fn handle_audio_devices_loaded(&mut self, result: Result<Vec<AudioDevice>, String>) {
        match result {
            Ok(devices) => {
                let was_asio = self.output.asio_available;
                self.output.audio_devices = devices.clone();
                self.refresh_asio_availability();
                if self.output.asio_available != was_asio {
                    self.maybe_handover_backend();
                }
                self.emit(Event::AudioDevices(devices));
                self.emit_output_mode();
            }
            Err(e) => tracing::warn!("audio devices: {e}"),
        }
    }

    pub(super) fn handle_audio_device_set(
        &mut self,
        device: Option<String>,
        result: Result<(), String>,
    ) {
        self.output.busy = false;
        match result {
            Ok(()) => {
                self.update_settings(|s| s.audio_device.clone_from(&device));
                self.refresh_asio_availability();
                self.maybe_handover_backend();
            }
            Err(e) => self.emit(Event::Notice(format!("切换输出设备失败：{e}"))),
        }
        self.emit(Event::AudioDevice(self.settings.audio_device.clone()));
        self.emit_output_mode();
    }

    pub(super) fn handle_exclusive_set(&mut self, enabled: bool, result: Result<(), String>) {
        self.output.busy = false;
        match result {
            Ok(()) => {
                self.update_settings(|s| {
                    s.exclusive = enabled;
                    if !enabled {
                        s.asio = false;
                    }
                });
                self.maybe_handover_backend();
            }
            Err(e) => self.emit(Event::Notice(format!("切换独占模式失败：{e}"))),
        }
        self.emit(Event::Exclusive(self.settings.exclusive));
        self.emit_output_mode();
    }

    pub(super) fn handle_exclusive_mode_set(
        &mut self,
        mode: ExclusiveMode,
        result: Result<(), String>,
    ) {
        self.output.busy = false;
        match result {
            Ok(()) => {
                self.update_settings(|s| s.set_exclusive_mode(mode));
                self.maybe_handover_backend();
            }
            Err(e) => self.emit(Event::Notice(format!("切换输出模式失败：{e}"))),
        }
        self.emit(Event::Exclusive(self.settings.exclusive));
        self.emit_output_mode();
    }
}
