//! `client_core::Engine` on top of `player` (libmpv).

use client_core::{AudioDevice, Engine, EngineEnd, EngineEvent};
use player::{EndReason, Player, PlayerEvent};

pub struct MpvEngine(pub Player);

impl MpvEngine {
    /// mpv ignores the system proxy; route each stream the way reqwest
    /// routes API requests. Also applies to appended (prefetched) entries,
    /// which all come from the same server.
    fn apply_proxy(&self, url: &str) -> Result<(), String> {
        let proxy = os::proxy::http_proxy_for(url);
        tracing::debug!(proxy = proxy.as_deref().unwrap_or("direct"), "stream proxy");
        self.0
            .set_http_proxy(proxy.as_deref())
            .map_err(|e| e.to_string())
    }
}

impl Engine for MpvEngine {
    fn load(&self, url: &str) -> Result<(), String> {
        self.apply_proxy(url)?;
        self.0.load(url).map_err(|e| e.to_string())
    }

    fn append(&self, url: &str) -> Result<(), String> {
        self.apply_proxy(url)?;
        self.0.append(url).map_err(|e| e.to_string())
    }

    fn clear_upcoming(&self) -> Result<(), String> {
        self.0.clear_upcoming().map_err(|e| e.to_string())
    }

    fn stop(&self) -> Result<(), String> {
        self.0.stop().map_err(|e| e.to_string())
    }

    fn set_paused(&self, paused: bool) -> Result<(), String> {
        self.0.set_paused(paused).map_err(|e| e.to_string())
    }

    fn seek(&self, seconds: f64) -> Result<(), String> {
        self.0.seek(seconds).map_err(|e| e.to_string())
    }

    fn set_volume(&self, volume: f64) -> Result<(), String> {
        self.0.set_volume(volume).map_err(|e| e.to_string())
    }

    fn set_fade_gain(&self, gain: f64) -> Result<(), String> {
        self.0.set_fade_gain(gain).map_err(|e| e.to_string())
    }

    fn audio_devices(&self) -> Result<Vec<AudioDevice>, String> {
        let devices = self.0.audio_devices().map_err(|e| e.to_string())?;
        Ok(devices
            .into_iter()
            .map(|d| AudioDevice {
                name: d.name,
                description: d.description,
            })
            .collect())
    }

    fn set_audio_device(&self, name: &str) -> Result<(), String> {
        self.0.set_audio_device(name).map_err(|e| e.to_string())
    }

    fn set_exclusive(&self, enabled: bool) -> Result<(), String> {
        self.0.set_exclusive(enabled).map_err(|e| e.to_string())
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
