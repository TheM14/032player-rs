//! Audio engine seen from core. `app` implements it on top of `player`
//! (libmpv), so core and its tests never link libmpv.

/// Commands core sends to the engine. Errors are human-readable strings.
pub trait Engine: Send + Sync + 'static {
    /// Replaces whatever plays with `url` and starts it.
    fn load(&self, url: &str) -> Result<(), String>;
    fn stop(&self) -> Result<(), String>;
    fn set_paused(&self, paused: bool) -> Result<(), String>;
    /// Absolute seek in seconds.
    fn seek(&self, seconds: f64) -> Result<(), String>;
    /// 0-100.
    fn set_volume(&self, volume: f64) -> Result<(), String>;
    /// Output devices (may block briefly; called off the core loop).
    fn audio_devices(&self) -> Result<Vec<AudioDevice>, String>;
    /// `auto` = system default. May block while the output reopens.
    fn set_audio_device(&self, name: &str) -> Result<(), String>;
    /// Exclusive mode, also while playing; rolls back on failure.
    /// May block while the output reopens.
    fn set_exclusive(&self, enabled: bool) -> Result<(), String>;
}

/// An audio output device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDevice {
    /// Engine name (`wasapi/{guid}`, `auto`).
    pub name: String,
    pub description: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineEnd {
    /// Reached the end of the file.
    Eof,
    /// Replaced or stopped by a command.
    Stop,
    /// Load or decode failure.
    Error,
}

/// Signals from the engine, fed back with [`crate::CoreHandle::engine_event`].
#[derive(Clone, Debug, PartialEq)]
pub enum EngineEvent {
    TimePos(f64),
    Duration(f64),
    Paused(bool),
    Buffering(bool),
    Volume(f64),
    FileLoaded,
    EndFile {
        reason: EngineEnd,
        error: Option<String>,
    },
}
