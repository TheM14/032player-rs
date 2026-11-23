//! `client_core::Engine` combining `player::Player` (libmpv/WASAPI) and
//! `player::AsioPlayer` (clean-room ASIO) with Opus automatic fallback.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use client_core::{
    AsioCapabilities, AsioChannelPair, AsioDriver, AsioTarget, AudioDevice, Engine, EngineEnd,
    EngineEvent, ExclusiveMode,
};
use player::{AsioPlayer, EndReason, Player, PlayerEvent};

pub type Forward = Box<dyn Fn(EngineEvent) + Send + Sync>;

pub const ACTIVE_NONE: u8 = 0;
pub const ACTIVE_MPV: u8 = 1;
pub const ACTIVE_ASIO: u8 = 2;

pub struct HybridEngine {
    mpv: Player,
    asio: Mutex<Option<Arc<AsioPlayer>>>,
    asio_target: Mutex<Option<AsioTarget>>,
    asio_active_target: Mutex<Option<AsioTarget>>,
    active: Arc<AtomicU8>,
    exclusive: AtomicBool,
    volume: Mutex<f64>,
    fade_gain: Mutex<f64>,
    #[cfg_attr(not(windows), allow(dead_code))]
    forward: Arc<OnceLock<Forward>>,
}

impl HybridEngine {
    pub fn new(
        mpv: Player,
        initial_volume: f64,
        initial_exclusive: bool,
        forward: Arc<OnceLock<Forward>>,
        active: Arc<AtomicU8>,
    ) -> Self {
        Self {
            mpv,
            asio: Mutex::new(None),
            asio_target: Mutex::new(None),
            asio_active_target: Mutex::new(None),
            active,
            exclusive: AtomicBool::new(initial_exclusive),
            volume: Mutex::new(initial_volume.clamp(0.0, 100.0)),
            fade_gain: Mutex::new(1.0),
            forward,
        }
    }

    /// mpv ignores the system proxy; route each stream the way reqwest
    /// routes API requests. Also applies to appended (prefetched) entries,
    /// which all come from the same server.
    fn apply_mpv_proxy(&self, url: &str) -> Result<(), String> {
        let proxy = os::proxy::http_proxy_for(url);
        tracing::debug!(proxy = proxy.as_deref().unwrap_or("direct"), "stream proxy");
        self.mpv
            .set_http_proxy(proxy.as_deref())
            .map_err(|e| e.to_string())
    }

    fn release_asio(&self) {
        let old = self.asio.lock().expect("asio lock").take();
        *self.asio_active_target.lock().expect("asio active target") = None;
        drop(old);
    }

    fn ensure_asio(&self, url: &str) -> Result<Arc<AsioPlayer>, String> {
        let target = self
            .asio_target
            .lock()
            .expect("asio target")
            .clone()
            .ok_or_else(|| "请先在设置中选择要使用的 ASIO 驱动".to_owned())?;
        let proxy = os::proxy::http_proxy_for(url);
        let mut guard = self.asio.lock().expect("asio lock");
        let target_changed = self
            .asio_active_target
            .lock()
            .expect("asio active target")
            .as_ref()
            != Some(&target);
        if target_changed {
            let old = guard.take();
            *self.asio_active_target.lock().expect("asio active target") = None;
            drop(old);
        }
        if guard.is_none() {
            let player = Arc::new(self.create_asio_player(&target)?);
            *guard = Some(player);
            *self.asio_active_target.lock().expect("asio active target") = Some(target);
        }
        let asio = guard.as_ref().expect("asio initialized").clone();
        drop(guard);
        asio.set_http_proxy(proxy.as_deref());
        asio.set_volume(*self.volume.lock().expect("volume lock"));
        asio.set_fade_gain(*self.fade_gain.lock().expect("fade lock"));
        Ok(asio)
    }

    #[cfg(windows)]
    fn create_asio_player(&self, target: &AsioTarget) -> Result<AsioPlayer, String> {
        let sink_target = player::AsioTarget::new(
            target.clsid.clone(),
            target.left_channel,
            target.right_channel,
        );
        let forward = self.forward.clone();
        let active = self.active.clone();
        let vol = *self.volume.lock().expect("volume lock");
        AsioPlayer::new(
            player::WindowsAsioSink::new(sink_target),
            vol,
            move |event| {
                if active.load(Ordering::Acquire) != ACTIVE_ASIO {
                    return;
                }
                if let (Some(f), Some(e)) = (forward.get(), map_event(event)) {
                    f(e);
                }
            },
        )
        .map_err(|e| e.to_string())
    }

    #[cfg(not(windows))]
    fn create_asio_player(&self, _target: &AsioTarget) -> Result<AsioPlayer, String> {
        Err("ASIO is only supported on Windows".into())
    }
}

impl Engine for HybridEngine {
    fn load(&self, url: &str) -> Result<(), String> {
        self.load_for(url, false)
    }

    fn load_for(&self, url: &str, use_asio: bool) -> Result<(), String> {
        if use_asio {
            if self.active.load(Ordering::Acquire) == ACTIVE_MPV {
                self.active.store(ACTIVE_NONE, Ordering::Release);
                let _ = self.mpv.stop();
            }
            let asio = self.ensure_asio(url)?;
            self.active.store(ACTIVE_ASIO, Ordering::Release);
            asio.load(url).map_err(|e| e.to_string())
        } else {
            if self.active.load(Ordering::Acquire) == ACTIVE_ASIO
                || self.asio.lock().expect("asio lock").is_some()
            {
                self.active.store(ACTIVE_NONE, Ordering::Release);
                self.release_asio();
            }
            let _ = self
                .mpv
                .set_exclusive(self.exclusive.load(Ordering::Acquire));
            self.apply_mpv_proxy(url)?;
            self.active.store(ACTIVE_MPV, Ordering::Release);
            self.mpv.load(url).map_err(|e| e.to_string())
        }
    }

    fn append(&self, url: &str) -> Result<(), String> {
        self.append_for(url, false)
    }

    fn append_for(&self, url: &str, use_asio: bool) -> Result<(), String> {
        if use_asio {
            let proxy = os::proxy::http_proxy_for(url);
            let guard = self.asio.lock().expect("asio lock");
            let Some(asio) = guard.as_ref() else {
                return Err("ASIO output is not active".into());
            };
            asio.set_http_proxy(proxy.as_deref());
            asio.append(url).map_err(|e| e.to_string())
        } else {
            self.apply_mpv_proxy(url)?;
            self.mpv.append(url).map_err(|e| e.to_string())
        }
    }

    fn clear_upcoming(&self) -> Result<(), String> {
        match self.active.load(Ordering::Acquire) {
            ACTIVE_ASIO => {
                if let Some(asio) = self.asio.lock().expect("asio lock").as_ref() {
                    asio.clear_upcoming().map_err(|e| e.to_string())?;
                }
                Ok(())
            }
            _ => self.mpv.clear_upcoming().map_err(|e| e.to_string()),
        }
    }

    fn stop(&self) -> Result<(), String> {
        self.active.store(ACTIVE_NONE, Ordering::Release);
        self.release_asio();
        self.mpv.stop().map_err(|e| e.to_string())
    }

    fn set_paused(&self, paused: bool) -> Result<(), String> {
        match self.active.load(Ordering::Acquire) {
            ACTIVE_ASIO => {
                if let Some(asio) = self.asio.lock().expect("asio lock").as_ref() {
                    asio.set_paused(paused).map_err(|e| e.to_string())
                } else {
                    Ok(())
                }
            }
            _ => self.mpv.set_paused(paused).map_err(|e| e.to_string()),
        }
    }

    fn seek(&self, seconds: f64) -> Result<(), String> {
        match self.active.load(Ordering::Acquire) {
            ACTIVE_ASIO => {
                if let Some(asio) = self.asio.lock().expect("asio lock").as_ref() {
                    asio.seek(seconds).map_err(|e| e.to_string())
                } else {
                    Err("ASIO output is not active".into())
                }
            }
            _ => self.mpv.seek(seconds).map_err(|e| e.to_string()),
        }
    }

    fn set_volume(&self, volume: f64) -> Result<(), String> {
        *self.volume.lock().expect("volume lock") = volume.clamp(0.0, 100.0);
        if let Some(asio) = self.asio.lock().expect("asio lock").as_ref() {
            asio.set_volume(volume);
        }
        self.mpv.set_volume(volume).map_err(|e| e.to_string())
    }

    fn set_fade_gain(&self, gain: f64) -> Result<(), String> {
        *self.fade_gain.lock().expect("fade lock") = gain.clamp(0.0, 1.0);
        if let Some(asio) = self.asio.lock().expect("asio lock").as_ref() {
            asio.set_fade_gain(gain);
        }
        self.mpv.set_fade_gain(gain).map_err(|e| e.to_string())
    }

    fn audio_devices(&self) -> Result<Vec<AudioDevice>, String> {
        let devices = self.mpv.audio_devices().map_err(|e| e.to_string())?;
        Ok(devices
            .into_iter()
            .map(|d| AudioDevice {
                name: d.name,
                description: d.description,
            })
            .collect())
    }

    fn set_audio_device(&self, name: &str) -> Result<(), String> {
        self.mpv.set_audio_device(name).map_err(|e| e.to_string())
    }

    fn set_exclusive(&self, enabled: bool) -> Result<(), String> {
        self.exclusive.store(enabled, Ordering::Release);
        self.mpv.set_exclusive(enabled).map_err(|e| e.to_string())
    }

    fn asio_drivers(&self) -> Vec<AsioDriver> {
        player::list_asio_drivers()
            .into_iter()
            .map(|d| AsioDriver {
                clsid: d.clsid,
                name: d.name,
            })
            .collect()
    }

    fn probe_asio_driver(&self, clsid: &str) -> Result<AsioCapabilities, String> {
        let caps = player::probe_asio_driver(clsid).map_err(|e| e.to_string())?;
        Ok(AsioCapabilities {
            output_channels: caps.output_channels,
            channel_pairs: caps
                .channel_pairs
                .into_iter()
                .map(|p| AsioChannelPair {
                    left: p.left,
                    right: p.right,
                    name: p.name,
                })
                .collect(),
            supported_sample_rates: caps.supported_sample_rates,
            sample_format: caps.sample_type.to_string(),
            buffer_size: caps.buffer_size,
        })
    }

    fn set_asio_target(&self, target: Option<AsioTarget>) -> Result<(), String> {
        *self.asio_target.lock().expect("asio target") = target;
        Ok(())
    }

    fn set_exclusive_mode(&self, mode: ExclusiveMode) -> Result<(), String> {
        match mode {
            ExclusiveMode::Off => {
                self.exclusive.store(false, Ordering::Release);
                if self.active.load(Ordering::Acquire) == ACTIVE_ASIO {
                    // ASIO will be stopped and handed over to mpv by core.
                    Ok(())
                } else {
                    self.mpv.set_exclusive(false).map_err(|e| e.to_string())
                }
            }
            ExclusiveMode::Wasapi => {
                self.exclusive.store(true, Ordering::Release);
                if self.active.load(Ordering::Acquire) == ACTIVE_ASIO {
                    // Handover happens after core stops ASIO, so defer mpv exclusive
                    // until `load_for(..., false)` avoids device-busy errors.
                    Ok(())
                } else {
                    self.mpv.set_exclusive(true).map_err(|e| e.to_string())
                }
            }
            ExclusiveMode::Asio => {
                let target = self
                    .asio_target
                    .lock()
                    .expect("asio target")
                    .clone()
                    .ok_or_else(|| "请先选择要使用的 ASIO 驱动".to_owned())?;
                if !player::list_asio_drivers()
                    .iter()
                    .any(|d| d.clsid.eq_ignore_ascii_case(&target.clsid))
                {
                    return Err("所选 ASIO 驱动未安装".into());
                }
                if self.active.load(Ordering::Acquire) == ACTIVE_NONE {
                    player::probe_asio_driver(&target.clsid).map_err(|e| e.to_string())?;
                }
                self.exclusive.store(true, Ordering::Release);
                Ok(())
            }
        }
    }

    fn open_asio_control_panel(&self) -> Result<(), String> {
        let target = self.asio_target.lock().expect("asio target").clone();
        let active_matches = self
            .asio_active_target
            .lock()
            .expect("asio active target")
            .as_ref()
            .is_some_and(|a| {
                target
                    .as_ref()
                    .is_some_and(|t| a.clsid.eq_ignore_ascii_case(&t.clsid))
            });
        // Clone the Arc and drop the mutex before opening the control panel so
        // playback controls (pause/seek/stop) never block behind it.
        let asio = if active_matches {
            self.asio.lock().expect("asio lock").clone()
        } else {
            None
        };
        if let Some(asio) = asio {
            return asio.open_control_panel().map_err(|e| e.to_string());
        }
        let target = target.ok_or_else(|| "请先选择要使用的 ASIO 驱动".to_owned())?;
        player::open_asio_control_panel_standalone(&target.clsid).map_err(|e| e.to_string())
    }
}

/// Used when mpv could not start: every command reports why.
pub struct NullEngine(pub String);

impl Engine for NullEngine {
    fn load(&self, _: &str) -> Result<(), String> {
        Err(self.0.clone())
    }

    fn append(&self, _: &str) -> Result<(), String> {
        Err(self.0.clone())
    }

    fn clear_upcoming(&self) -> Result<(), String> {
        Ok(())
    }

    fn stop(&self) -> Result<(), String> {
        Ok(())
    }

    fn set_paused(&self, _: bool) -> Result<(), String> {
        Err(self.0.clone())
    }

    fn seek(&self, _: f64) -> Result<(), String> {
        Err(self.0.clone())
    }

    fn set_volume(&self, _: f64) -> Result<(), String> {
        Err(self.0.clone())
    }

    fn audio_devices(&self) -> Result<Vec<AudioDevice>, String> {
        Err(self.0.clone())
    }

    fn set_audio_device(&self, _: &str) -> Result<(), String> {
        Err(self.0.clone())
    }

    fn set_exclusive(&self, _: bool) -> Result<(), String> {
        Err(self.0.clone())
    }
}

pub fn map_event(event: PlayerEvent) -> Option<EngineEvent> {
    Some(match event {
        PlayerEvent::TimePos(s) => EngineEvent::TimePos(s),
        PlayerEvent::Duration(s) => EngineEvent::Duration(s),
        PlayerEvent::Paused(p) => EngineEvent::Paused(p),
        PlayerEvent::Buffering(b) => EngineEvent::Buffering(b),
        PlayerEvent::Volume(v) => EngineEvent::Volume(v),
        PlayerEvent::FileLoaded { path } => EngineEvent::FileLoaded { path },
        PlayerEvent::EndFile { reason, error } => EngineEvent::EndFile {
            reason: match reason {
                EndReason::Eof => EngineEnd::Eof,
                EndReason::Error => EngineEnd::Error,
                EndReason::Stop | EndReason::Quit | EndReason::Redirect => EngineEnd::Stop,
            },
            error,
        },
        PlayerEvent::PlaylistPos(_)
        | PlayerEvent::Idle(_)
        | PlayerEvent::Muted(_)
        | PlayerEvent::PlaybackRestart
        | PlayerEvent::Shutdown => return None,
    })
}
