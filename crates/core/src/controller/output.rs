//! Audio output device, exclusive / ASIO routing, and backend handover.

use std::collections::{HashMap, HashSet};

use api::{Track, Transcode};

use super::{AsioStateView, Core, Event, Internal};
use crate::engine::{AsioCapabilities, AsioDriver, AsioTarget, AudioDevice, Engine};
use crate::settings::ExclusiveMode;

/// Output routing and ASIO state owned by [`Core`].
#[derive(Default)]
pub(super) struct OutputState {
    /// An output device or exclusive mode change is running.
    pub(super) busy: bool,
    pub(super) asio_drivers: Vec<AsioDriver>,
    /// Cached probe results keyed by normalized uppercase CLSID.
    pub(super) asio_caps: HashMap<String, AsioCapabilities>,
    pub(super) asio_probe_seq: u64,
    pub(super) asio_status: String,
    pub(super) asio_ready: bool,
    pub(super) asio_available: bool,
    /// Active engine stream was started in ASIO mode.
    pub(super) asio_active: bool,
    /// Active ASIO target (driver CLSID + stereo channel pair) currently streaming.
    pub(super) active_asio_target: Option<AsioTarget>,
    /// Current track fell back from ASIO to WASAPI (e.g. Opus).
    pub(super) asio_fallback: bool,
    /// Track IDs confirmed to contain Opus at runtime (fallback on subsequent plays).
    pub(super) opus_fallback_tracks: HashSet<i64>,
}

impl OutputState {
    /// Clears active ASIO stream flags, returning whether any changed.
    pub(super) fn clear_asio_stream(&mut self) -> bool {
        let changed = self.asio_active || self.asio_fallback || self.active_asio_target.is_some();
        self.asio_active = false;
        self.active_asio_target = None;
        self.asio_fallback = false;
        changed
    }

    pub(super) fn cached_asio_caps(&self, clsid: &str) -> Option<&AsioCapabilities> {
        self.asio_caps.get(&clsid.trim().to_ascii_uppercase())
    }

    pub(super) fn cache_asio_caps(&mut self, clsid: &str, caps: AsioCapabilities) {
        self.asio_caps
            .insert(clsid.trim().to_ascii_uppercase(), caps);
    }

    pub(super) fn remove_cached_asio_caps(&mut self, clsid: &str) {
        self.asio_caps.remove(&clsid.trim().to_ascii_uppercase());
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

    pub(super) fn refresh_audio_devices(&self, probe_asio: bool) {
        let engine = self.engine.clone();
        let tx = self.internal.clone();
        tokio::spawn(async move {
            let (devices, drivers) = tokio::task::spawn_blocking(move || {
                (engine.audio_devices(), engine.asio_drivers())
            })
            .await
            .unwrap_or_else(|e| (Err(e.to_string()), Vec::new()));
            let _ = tx.send(Internal::AudioDevicesLoaded {
                devices,
                drivers,
                probe_asio,
            });
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

    pub(super) fn toggle_exclusive(&mut self) {
        let target = if self.effective_exclusive_mode() != ExclusiveMode::Off {
            ExclusiveMode::Off
        } else {
            self.preferred_exclusive_mode()
        };
        self.set_exclusive_mode(target);
    }

    pub(super) fn set_exclusive_mode(&mut self, mode: ExclusiveMode) {
        if self.output.busy {
            self.emit_output_mode();
            return;
        }
        let mut auto_clsid: Option<String> = None;
        let mut probe_clsid: Option<String> = None;
        if mode == ExclusiveMode::Asio {
            if !self.output.asio_available {
                self.emit(Event::Notice("未检测到已安装的 ASIO 驱动".into()));
                self.emit_output_mode();
                return;
            }
            if let Some(target) = self.selected_asio_target() {
                self.sync_engine_asio_target();
                if self.pb.idle {
                    probe_clsid = Some(target.clsid);
                }
            } else if self.output.asio_drivers.len() == 1 {
                let sole = self.output.asio_drivers[0].clsid.clone();
                let (left, right) = self.settings.asio_channel_pair(&sole);
                let _ = self.engine.set_asio_target(Some(AsioTarget {
                    clsid: sole.clone(),
                    left_channel: left,
                    right_channel: right,
                }));
                if self.pb.idle {
                    probe_clsid = Some(sole.clone());
                }
                auto_clsid = Some(sole);
            } else {
                self.emit(Event::Notice("请先在设置中选择要使用的 ASIO 驱动".into()));
                self.emit_output_mode();
                self.emit_asio_state();
                return;
            }
        }
        if auto_clsid.is_none()
            && mode == self.effective_exclusive_mode()
            && mode == self.settings.exclusive_mode()
        {
            self.emit_output_mode();
            return;
        }

        self.output.busy = true;
        let engine = self.engine.clone();
        let tx = self.internal.clone();
        tokio::spawn(async move {
            let (probed, result) = tokio::task::spawn_blocking(move || {
                let probed = if let Some(clsid) = probe_clsid {
                    match engine.probe_asio_driver(&clsid) {
                        Ok(caps) => Some((clsid, caps)),
                        Err(e) => return (None, Err(e)),
                    }
                } else {
                    None
                };
                let res = engine.set_exclusive_mode(mode);
                (probed, res)
            })
            .await
            .unwrap_or_else(|e| (None, Err(e.to_string())));
            let _ = tx.send(Internal::ExclusiveModeSet {
                mode,
                auto_clsid,
                probed,
                result,
            });
        });
    }

    pub(super) fn set_asio_driver(&mut self, clsid: Option<String>) {
        if self.output.busy {
            self.emit_asio_state();
            return;
        }
        let Some(clsid) = clsid.filter(|s| !s.trim().is_empty()) else {
            if self.settings.asio_driver.is_none() && !self.settings.asio {
                self.emit_output_mode();
                self.emit_asio_state();
                return;
            }
            self.output.asio_probe_seq = self.output.asio_probe_seq.wrapping_add(1);
            self.update_settings(|s| {
                s.asio_driver = None;
                s.asio = false;
            });
            self.output.asio_ready = false;
            self.refresh_asio_status_without_probe();
            self.maybe_handover_backend();
            self.sync_engine_asio_target();
            self.emit_output_mode();
            self.emit_asio_state();
            return;
        };
        let canonical = self
            .output
            .asio_drivers
            .iter()
            .find(|d| d.clsid.eq_ignore_ascii_case(&clsid))
            .map(|d| d.clsid.clone())
            .unwrap_or(clsid);
        if self
            .settings
            .asio_driver
            .as_deref()
            .is_some_and(|cur| cur.eq_ignore_ascii_case(&canonical))
        {
            self.emit_asio_state();
            return;
        }
        let rollback_clsid = if self.output.asio_active {
            self.output
                .active_asio_target
                .as_ref()
                .map(|t| t.clsid.clone())
        } else {
            None
        };
        self.update_settings(|s| s.asio_driver = Some(canonical));
        self.output.asio_ready = false;
        // Keep the engine bound to the actively streaming ASIO driver until the
        // new driver probe succeeds and hands over.
        if !self.output.asio_active {
            self.sync_engine_asio_target();
        }
        self.emit_output_mode();
        let handover = self.effective_exclusive_mode() == ExclusiveMode::Asio
            && !self.pb.idle
            && !self.casting();
        self.probe_selected_asio_driver(true, handover, rollback_clsid);
    }

    pub(super) fn set_asio_channel_pair(&mut self, left: usize, right: usize) {
        if self.output.busy || left == right {
            self.emit_asio_state();
            return;
        }
        let Some(clsid) = self.settings.asio_driver.clone() else {
            self.emit_asio_state();
            return;
        };
        if let Some(caps) = self.output.cached_asio_caps(&clsid)
            && (left >= caps.output_channels || right >= caps.output_channels)
        {
            self.emit_asio_state();
            return;
        }
        if self.settings.asio_channel_pair(&clsid) == (left, right) {
            self.emit_asio_state();
            return;
        }
        self.update_settings(|s| s.set_asio_channel_pair(&clsid, left, right));
        self.sync_engine_asio_target();
        self.emit_asio_state();
        self.maybe_handover_backend();
    }

    pub(super) fn refresh_asio_driver(&self) {
        let engine = self.engine.clone();
        let tx = self.internal.clone();
        tokio::spawn(async move {
            let drivers = tokio::task::spawn_blocking(move || engine.asio_drivers())
                .await
                .unwrap_or_default();
            let _ = tx.send(Internal::AsioDriversRefreshed(drivers));
        });
    }

    pub(super) fn open_asio_control_panel(&self) {
        if !self.output.asio_active {
            self.sync_engine_asio_target();
        }
        let engine = self.engine.clone();
        let tx = self.internal.clone();
        tokio::spawn(async move {
            let res = tokio::task::spawn_blocking(move || engine.open_asio_control_panel())
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            let _ = tx.send(Internal::AsioControlPanelOpened(res));
        });
    }

    pub(super) fn selected_asio_target(&self) -> Option<AsioTarget> {
        let saved = self.settings.asio_driver.as_deref()?;
        let driver = self
            .output
            .asio_drivers
            .iter()
            .find(|d| d.clsid.eq_ignore_ascii_case(saved))?;
        let (mut left, mut right) = self.settings.asio_channel_pair(&driver.clsid);
        if let Some(caps) = self.output.cached_asio_caps(&driver.clsid)
            && (left >= caps.output_channels || right >= caps.output_channels || left == right)
        {
            if let Some(first) = caps.channel_pairs.first() {
                left = first.left;
                right = first.right;
            } else {
                left = 0;
                right = 1;
            }
        }
        Some(AsioTarget {
            clsid: driver.clsid.clone(),
            left_channel: left,
            right_channel: right,
        })
    }

    pub(super) fn sync_engine_asio_target(&self) {
        let _ = self.engine.set_asio_target(self.selected_asio_target());
    }

    pub(super) fn asio_configured(&self) -> bool {
        self.output.asio_available && self.selected_asio_target().is_some()
    }

    pub(super) fn preferred_exclusive_mode(&self) -> ExclusiveMode {
        if self.settings.asio && self.asio_configured() {
            ExclusiveMode::Asio
        } else {
            ExclusiveMode::Wasapi
        }
    }

    pub(super) fn effective_exclusive_mode(&self) -> ExclusiveMode {
        if !self.settings.exclusive {
            ExclusiveMode::Off
        } else if self.settings.asio && self.asio_configured() {
            ExclusiveMode::Asio
        } else {
            ExclusiveMode::Wasapi
        }
    }

    pub(super) fn emit_output_mode(&self) {
        self.emit(Event::OutputMode {
            mode: self.effective_exclusive_mode(),
            preferred_mode: self.preferred_exclusive_mode(),
            asio_available: self.output.asio_available,
            asio_configured: self.asio_configured(),
            asio_fallback: self.output.asio_fallback,
        });
    }

    pub(super) fn emit_asio_state(&self) {
        let selected_clsid = self.settings.asio_driver.clone();
        let selected_channels = selected_clsid
            .as_deref()
            .map_or((0, 1), |c| self.settings.asio_channel_pair(c));
        let (channel_pairs, supported_sample_rates) = selected_clsid
            .as_deref()
            .and_then(|sel| self.output.cached_asio_caps(sel))
            .map_or_else(
                || (Vec::new(), Vec::new()),
                |caps| {
                    (
                        caps.channel_pairs.clone(),
                        caps.supported_sample_rates.clone(),
                    )
                },
            );
        self.emit(Event::AsioState(AsioStateView {
            drivers: self.output.asio_drivers.clone(),
            selected_clsid,
            channel_pairs,
            selected_channels,
            supported_sample_rates,
            status: self.output.asio_status.clone(),
            ready: self.output.asio_ready,
        }));
    }

    fn apply_probed_asio_caps(&mut self, clsid: String, caps: AsioCapabilities) {
        let (left, right) = self.settings.asio_channel_pair(&clsid);
        if (left >= caps.output_channels || right >= caps.output_channels || left == right)
            && let Some(first) = caps.channel_pairs.first()
        {
            self.update_settings(|s| {
                s.set_asio_channel_pair(&clsid, first.left, first.right);
            });
        }
        self.output.asio_ready = true;
        self.output.asio_status = format_asio_capabilities(&caps);
        self.output.cache_asio_caps(&clsid, caps);
    }

    /// Recomputes `asio_status` / `asio_ready` from cached capabilities and
    /// active playback state without opening any COM hardware instance.
    fn refresh_asio_status_without_probe(&mut self) {
        let Some(saved) = self.settings.asio_driver.as_deref() else {
            self.output.asio_ready = false;
            self.output.asio_status = if self.output.asio_drivers.is_empty() {
                "未检测到已安装的 ASIO 驱动".into()
            } else {
                "当前仅使用 WASAPI；选定驱动后将在播放条启用 ASIO 快捷切换".into()
            };
            return;
        };
        let Some(driver) = self
            .output
            .asio_drivers
            .iter()
            .find(|d| d.clsid.eq_ignore_ascii_case(saved))
        else {
            self.output.asio_ready = false;
            self.output.asio_status = "已保存的 ASIO 驱动未安装，请重新选择".into();
            return;
        };
        if let Some(status) = self
            .output
            .cached_asio_caps(&driver.clsid)
            .map(format_asio_capabilities)
        {
            self.output.asio_ready = true;
            self.output.asio_status = status;
        } else if self.output.asio_active
            && self
                .output
                .active_asio_target
                .as_ref()
                .is_some_and(|t| t.clsid.eq_ignore_ascii_case(&driver.clsid))
        {
            self.output.asio_ready = true;
            self.output.asio_status = "正在通过该 ASIO 驱动播放".into();
        } else {
            self.output.asio_ready = false;
            self.output.asio_status.clear();
        }
    }

    pub(super) fn sync_asio_status_after_stop(&mut self) {
        if self.output.asio_status == "正在通过该 ASIO 驱动播放" {
            self.refresh_asio_status_without_probe();
            self.emit_asio_state();
        }
    }

    pub(super) fn probe_selected_asio_driver(
        &mut self,
        force: bool,
        handover: bool,
        rollback_clsid: Option<String>,
    ) {
        let Some(saved) = self.settings.asio_driver.clone() else {
            self.output.asio_ready = false;
            self.output.asio_status = if self.output.asio_drivers.is_empty() {
                "未检测到已安装的 ASIO 驱动".into()
            } else {
                "当前仅使用 WASAPI；选定驱动后将在播放条启用 ASIO 快捷切换".into()
            };
            self.emit_asio_state();
            return;
        };
        let Some(driver) = self
            .output
            .asio_drivers
            .iter()
            .find(|d| d.clsid.eq_ignore_ascii_case(&saved))
            .cloned()
        else {
            self.output.asio_ready = false;
            self.output.asio_status = "已保存的 ASIO 驱动未安装，请重新选择".into();
            self.emit_asio_state();
            return;
        };

        // Do not open a second COM instance of a driver that is actively streaming.
        if self.output.asio_active
            && self
                .output
                .active_asio_target
                .as_ref()
                .is_some_and(|t| t.clsid.eq_ignore_ascii_case(&driver.clsid))
        {
            self.output.asio_probe_seq = self.output.asio_probe_seq.wrapping_add(1);
            self.output.asio_ready = true;
            self.output.asio_status = self.output.cached_asio_caps(&driver.clsid).map_or_else(
                || "正在通过该 ASIO 驱动播放".into(),
                format_asio_capabilities,
            );
            self.sync_engine_asio_target();
            self.emit_asio_state();
            if handover {
                self.maybe_handover_backend();
            }
            return;
        }

        if !force
            && let Some(status) = self
                .output
                .cached_asio_caps(&driver.clsid)
                .map(format_asio_capabilities)
        {
            self.output.asio_ready = true;
            self.output.asio_status = status;
            self.emit_asio_state();
            if handover {
                self.maybe_handover_backend();
            }
            return;
        }

        self.output.asio_probe_seq = self.output.asio_probe_seq.wrapping_add(1);
        let seq = self.output.asio_probe_seq;
        self.output.asio_ready = false;
        self.output.asio_status = "正在检测 ASIO 设备…".into();
        self.emit_asio_state();

        let clsid = driver.clsid;
        let engine = self.engine.clone();
        let tx = self.internal.clone();
        tokio::spawn(async move {
            let probe_clsid = clsid.clone();
            let result =
                tokio::task::spawn_blocking(move || engine.probe_asio_driver(&probe_clsid))
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
            let _ = tx.send(Internal::AsioDriverProbed {
                clsid,
                seq,
                result,
                handover,
                rollback_clsid,
            });
        });
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
        let desired_target = if need_asio {
            self.selected_asio_target()
        } else {
            None
        };
        let target_changed = need_asio && desired_target != self.output.active_asio_target;
        if need_asio != self.output.asio_active || target_changed {
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

    pub(super) fn handle_audio_devices_loaded(
        &mut self,
        result: Result<Vec<AudioDevice>, String>,
        drivers: Vec<AsioDriver>,
        probe_asio: bool,
    ) {
        let prev_mode = self.effective_exclusive_mode();
        self.output.asio_drivers = drivers;
        self.output.asio_available = !self.output.asio_drivers.is_empty();
        // Migrate legacy v0.2.27 configs (`exclusive: true, asio: true` without
        // `asioDriver`) seamlessly when a single ASIO driver is installed.
        if self.settings.exclusive
            && self.settings.asio
            && self.settings.asio_driver.is_none()
            && self.output.asio_drivers.len() == 1
        {
            let sole = self.output.asio_drivers[0].clsid.clone();
            self.update_settings(|s| s.asio_driver = Some(sole));
        }
        if !self.output.asio_active {
            self.sync_engine_asio_target();
        }
        if self.effective_exclusive_mode() != prev_mode {
            self.maybe_handover_backend();
        }
        match result {
            Ok(devices) => self.emit(Event::AudioDevices(devices)),
            Err(e) => tracing::warn!("audio devices: {e}"),
        }
        self.emit_output_mode();
        if probe_asio {
            self.probe_selected_asio_driver(false, false, None);
        } else {
            self.refresh_asio_status_without_probe();
            self.emit_asio_state();
        }
    }

    pub(super) fn handle_asio_drivers_refreshed(&mut self, drivers: Vec<AsioDriver>) {
        let prev_mode = self.effective_exclusive_mode();
        self.output.asio_drivers = drivers;
        self.output.asio_available = !self.output.asio_drivers.is_empty();
        if !self.output.asio_active {
            self.sync_engine_asio_target();
        }
        if self.effective_exclusive_mode() != prev_mode {
            self.maybe_handover_backend();
        }
        self.emit_output_mode();
        let handover = self.effective_exclusive_mode() == ExclusiveMode::Asio
            && !self.pb.idle
            && !self.casting();
        self.probe_selected_asio_driver(true, handover, None);
    }

    pub(super) fn handle_asio_driver_probed(
        &mut self,
        clsid: String,
        seq: u64,
        result: Result<AsioCapabilities, String>,
        handover: bool,
        rollback_clsid: Option<String>,
    ) {
        if seq != self.output.asio_probe_seq {
            return;
        }
        match result {
            Ok(caps) => {
                self.apply_probed_asio_caps(clsid, caps);
                self.sync_engine_asio_target();
                self.emit_asio_state();
                if handover {
                    self.maybe_handover_backend();
                }
            }
            Err(e) => {
                self.output.remove_cached_asio_caps(&clsid);
                if handover
                    && self.output.asio_active
                    && let Some(prev) = rollback_clsid
                {
                    self.update_settings(|s| s.asio_driver = Some(prev));
                    self.sync_engine_asio_target();
                    self.refresh_asio_status_without_probe();
                    self.emit_output_mode();
                    self.emit_asio_state();
                    self.sync_preload();
                    self.emit(Event::Notice(format!("切换 ASIO 驱动失败：{e}")));
                } else {
                    self.output.asio_ready = false;
                    self.output.asio_status = format!("设备未就绪：{e}");
                    self.emit_asio_state();
                    if handover {
                        self.emit(Event::Notice(format!("切换 ASIO 驱动失败：{e}")));
                    }
                }
            }
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
        auto_clsid: Option<String>,
        probed: Option<(String, AsioCapabilities)>,
        result: Result<(), String>,
    ) {
        self.output.busy = false;
        let ok = match result {
            Ok(()) => {
                if let Some(clsid) = auto_clsid {
                    self.update_settings(|s| s.asio_driver = Some(clsid));
                }
                if let Some((clsid, caps)) = probed {
                    self.apply_probed_asio_caps(clsid, caps);
                }
                self.update_settings(|s| s.set_exclusive_mode(mode));
                self.sync_engine_asio_target();
                self.maybe_handover_backend();
                true
            }
            Err(e) => {
                self.sync_engine_asio_target();
                if mode == ExclusiveMode::Asio && auto_clsid.is_none() {
                    self.output.asio_ready = false;
                    self.output.asio_status = format!("设备未就绪：{e}");
                }
                self.emit(Event::Notice(format!("切换输出模式失败：{e}")));
                false
            }
        };
        self.emit(Event::Exclusive(self.settings.exclusive));
        self.emit_output_mode();
        if ok && mode == ExclusiveMode::Asio {
            self.probe_selected_asio_driver(false, false, None);
        } else {
            self.emit_asio_state();
        }
    }
}

fn format_asio_capabilities(caps: &AsioCapabilities) -> String {
    let mut parts = vec![
        "已就绪".to_owned(),
        format!("{} 输出通道", caps.output_channels),
        caps.sample_format.clone(),
    ];
    if !caps.supported_sample_rates.is_empty() {
        let rates: Vec<String> = caps
            .supported_sample_rates
            .iter()
            .map(|&hz| format_khz(hz))
            .collect();
        parts.push(format!("支持 {} kHz", rates.join(" / ")));
    }
    parts.join(" · ")
}

fn format_khz(hz: u32) -> String {
    if hz.is_multiple_of(1_000) {
        format!("{}", hz / 1_000)
    } else if hz.is_multiple_of(100) {
        format!("{}.{}", hz / 1_000, (hz % 1_000) / 100)
    } else {
        format!("{:.2}", f64::from(hz) / 1_000.0)
    }
}
