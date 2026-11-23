//! Audio engine seen from core. `app` implements it on top of `player`
//! (libmpv), so core and its tests never link libmpv.

/// Commands core sends to the engine. Errors are human-readable strings.
pub trait Engine: Send + Sync + 'static {
    /// Replaces whatever plays (and anything queued) with `url` and starts it.
    fn load(&self, url: &str) -> Result<(), String>;
    /// Queues `url` after the current file; the engine prefetches it and
    /// continues into it without a gap.
    fn append(&self, url: &str) -> Result<(), String>;
    /// Drops everything queued after the current file.
    fn clear_upcoming(&self) -> Result<(), String>;
    fn stop(&self) -> Result<(), String>;
    fn set_paused(&self, paused: bool) -> Result<(), String>;
    /// Absolute seek in seconds.
    fn seek(&self, seconds: f64) -> Result<(), String>;
    /// 0-100.
    fn set_volume(&self, volume: f64) -> Result<(), String>;
    /// Extra gain for play / pause fades, 0 (silent) to 1 (unchanged),
    /// applied on top of the volume without changing it. Engines without
    /// it return an error and core pauses / plays without fading.
    fn set_fade_gain(&self, _gain: f64) -> Result<(), String> {
        Err("fade gain not supported".into())
    }
    /// Output devices (may block briefly; called off the core loop).
    fn audio_devices(&self) -> Result<Vec<AudioDevice>, String>;
    /// `auto` = system default. May block while the output reopens.
    fn set_audio_device(&self, name: &str) -> Result<(), String>;
    /// Exclusive mode, also while playing; rolls back on failure.
    /// May block while the output reopens.
    fn set_exclusive(&self, enabled: bool) -> Result<(), String>;

    /// Loads `url`, preferring ASIO when `use_asio` is true.
    fn load_for(&self, url: &str, _use_asio: bool) -> Result<(), String> {
        self.load(url)
    }

    /// Appends `url` for gapless playback, preferring ASIO when `use_asio` is true.
    fn append_for(&self, url: &str, _use_asio: bool) -> Result<(), String> {
        self.append(url)
    }

    /// Enumerates installed ASIO drivers without opening hardware.
    fn asio_drivers(&self) -> Vec<AsioDriver> {
        Vec::new()
    }

    /// Probes a single ASIO driver by CLSID to inspect its output channel pairs
    /// and supported sample rates.
    fn probe_asio_driver(&self, _clsid: &str) -> Result<AsioCapabilities, String> {
        Err("ASIO not supported".into())
    }

    /// Configures the active ASIO target (driver CLSID and output channel pair).
    fn set_asio_target(&self, _target: Option<AsioTarget>) -> Result<(), String> {
        Ok(())
    }

    /// Applies an [`ExclusiveMode`](crate::settings::ExclusiveMode) change.
    fn set_exclusive_mode(&self, mode: crate::settings::ExclusiveMode) -> Result<(), String> {
        match mode {
            crate::settings::ExclusiveMode::Off => self.set_exclusive(false),
            crate::settings::ExclusiveMode::Wasapi => self.set_exclusive(true),
            crate::settings::ExclusiveMode::Asio => Err("ASIO not supported".into()),
        }
    }

    /// Opens the ASIO driver control panel if supported by the current engine.
    fn open_asio_control_panel(&self) -> Result<(), String> {
        Err("ASIO not supported".into())
    }
}

/// An audio output device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDevice {
    /// Engine name (`wasapi/{guid}`, `auto`).
    pub name: String,
    pub description: String,
}

/// An installed ASIO driver discovered from the OS registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AsioDriver {
    pub clsid: String,
    pub name: String,
}

/// A stereo pair of output channels exposed by an ASIO driver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AsioChannelPair {
    pub left: usize,
    pub right: usize,
    pub name: String,
}

/// Selected ASIO driver and stereo output channel pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AsioTarget {
    pub clsid: String,
    pub left_channel: usize,
    pub right_channel: usize,
}

/// Hardware capabilities obtained by probing an ASIO driver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AsioCapabilities {
    pub output_channels: usize,
    pub channel_pairs: Vec<AsioChannelPair>,
    pub supported_sample_rates: Vec<u32>,
    pub sample_format: String,
    pub buffer_size: usize,
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
    /// A file started; `path` is its URL when the engine knows it.
    FileLoaded {
        path: Option<String>,
    },
    EndFile {
        reason: EngineEnd,
        error: Option<String>,
    },
}
