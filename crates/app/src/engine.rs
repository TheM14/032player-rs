//! `client_core::Engine` on top of `player` (libmpv).

use client_core::{Engine, EngineEnd, EngineEvent};
use player::{EndReason, Player, PlayerEvent};

pub struct MpvEngine(pub Player);

impl Engine for MpvEngine {
    fn load(&self, url: &str) -> Result<(), String> {
        self.0.load(url).map_err(|e| e.to_string())
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
}

/// Used when mpv could not start: every command reports why.
pub struct NullEngine(pub String);

impl Engine for NullEngine {
    fn load(&self, _: &str) -> Result<(), String> {
        Err(self.0.clone())
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
}

pub fn map_event(event: PlayerEvent) -> Option<EngineEvent> {
    Some(match event {
        PlayerEvent::TimePos(s) => EngineEvent::TimePos(s),
        PlayerEvent::Duration(s) => EngineEvent::Duration(s),
        PlayerEvent::Paused(p) => EngineEvent::Paused(p),
        PlayerEvent::Buffering(b) => EngineEvent::Buffering(b),
        PlayerEvent::Volume(v) => EngineEvent::Volume(v),
        PlayerEvent::FileLoaded => EngineEvent::FileLoaded,
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
