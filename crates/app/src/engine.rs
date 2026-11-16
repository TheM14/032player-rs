//! `client_core::Engine` combining `player::Player` (libmpv/WASAPI) and
//! `player::AsioPlayer` (clean-room ASIO) with Opus automatic fallback.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use client_core::{AudioDevice, Engine, EngineEnd, EngineEvent, ExclusiveMode};
use player::{AsioPlayer, EndReason, Player, PlayerEvent};

pub type Forward = Box<dyn Fn(EngineEvent) + Send + Sync>;

pub const ACTIVE_NONE: u8 = 0;
pub const ACTIVE_MPV: u8 = 1;
pub const ACTIVE_ASIO: u8 = 2;

pub struct HybridEngine {
    mpv: Player,
    asio: Mutex<Option<AsioPlayer>>,
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
        drop(old);
    }

    fn ensure_asio(&self, url: &str) -> Result<(), String> {
        let proxy = os::proxy::http_proxy_for(url);
        let mut guard = self.asio.lock().expect("asio lock");
        if guard.is_none() {
            let player = self.create_asio_player()?;
            *guard = Some(player);
        }
        let asio = guard.as_ref().expect("asio initialized");
        asio.set_http_proxy(proxy.as_deref());
        asio.set_volume(*self.volume.lock().expect("volume lock"));
        asio.set_fade_gain(*self.fade_gain.lock().expect("fade lock"));
        Ok(())
    }

    #[cfg(windows)]
    fn create_asio_player(&self) -> Result<AsioPlayer, String> {
        let clsid =
            player::fiio_asio_clsid().ok_or_else(|| "未检测到 FiiO ASIO 驱动".to_owned())?;
        let forward = self.forward.clone();
        let active = self.active.clone();
        let vol = *self.volume.lock().expect("volume lock");
        AsioPlayer::new(player::WindowsAsioSink::new(clsid), vol, move |event| {
            if active.load(Ordering::Acquire) != ACTIVE_ASIO {
                return;
            }
            if let (Some(f), Some(e)) = (forward.get(), map_event(event)) {
                f(e);
            }
        })
        .map_err(|e| e.to_string())
    }

    #[cfg(not(windows))]
    fn create_asio_player(&self) -> Result<AsioPlayer, String> {
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
            self.ensure_asio(url)?;
            self.active.store(ACTIVE_ASIO, Ordering::Release);
            let guard = self.asio.lock().expect("asio lock");
            guard
                .as_ref()
                .expect("asio initialized")
                .load(url)
                .map_err(|e| e.to_string())
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

    fn supports_asio(&self, devices: &[AudioDevice], selected: Option<&str>) -> bool {
        let mapped: Vec<player::AudioDevice> = devices
            .iter()
            .map(|d| player::AudioDevice {
                name: d.name.clone(),
                description: d.description.clone(),
            })
            .collect();
        player::fiio_asio_available(&mapped, selected)
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
                if player::fiio_asio_clsid().is_none() {
                    return Err("未检测到 FiiO ASIO 驱动".into());
                }
                if self.active.load(Ordering::Acquire) == ACTIVE_NONE {
                    player::probe_fiio_asio().map_err(|e| e.to_string())?;
                }
                self.exclusive.store(true, Ordering::Release);
                Ok(())
            }
        }
    }

    fn open_asio_control_panel(&self) -> Result<(), String> {
        if let Some(asio) = self.asio.lock().expect("asio lock").as_ref() {
            return asio.open_control_panel().map_err(|e| e.to_string());
        }
        player::open_fiio_control_panel_standalone().map_err(|e| e.to_string())
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
