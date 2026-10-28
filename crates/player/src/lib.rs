//! libmpv wrapper. The only crate that links libmpv.
//!
//! One mpv instance, audio only, driven in-process (no IPC socket). Events
//! are read on a dedicated thread and handed to a callback as
//! [`PlayerEvent`]s. Options follow the Electron client
//! (`src/main/features/core/player/index.ts`, `DEFAULT_MPV_PARAMETERS`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use libmpv2::events::{Event, PropertyData};
use libmpv2::{Format, Mpv, mpv_end_file_reason};
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum PlayerError {
    #[error("mpv: {0}")]
    Mpv(String),
    #[error("audio output change already in progress")]
    Busy,
    #[error("mpv did not apply exclusive mode")]
    ExclusiveNotApplied,
    #[error("invalid mpv response: {0}")]
    InvalidResponse(String),
}

impl From<libmpv2::Error> for PlayerError {
    fn from(e: libmpv2::Error) -> Self {
        Self::Mpv(format!("{e:?}"))
    }
}

pub type Result<T> = std::result::Result<T, PlayerError>;

/// Why a file stopped (`end-file` reason).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    /// Reached the end of the file.
    Eof,
    /// Stopped or replaced by a command (`loadfile replace`, `stop`, `playlist-next`).
    Stop,
    Quit,
    /// Could not be loaded or decoded.
    Error,
    Redirect,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlayerEvent {
    /// Playback position in seconds.
    TimePos(f64),
    /// Duration of the current file in seconds.
    Duration(f64),
    Paused(bool),
    /// `paused-for-cache`: stalled waiting for the network.
    Buffering(bool),
    /// Index in mpv's playlist; `-1` when nothing is playing.
    PlaylistPos(i64),
    /// Nothing loaded (`idle-active`).
    Idle(bool),
    Volume(f64),
    Muted(bool),
    FileLoaded,
    EndFile {
        reason: EndReason,
        /// mpv error text for [`EndReason::Error`].
        error: Option<String>,
    },
    /// A seek finished and playback continues.
    PlaybackRestart,
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct AudioDevice {
    /// Value for `audio-device`, e.g. `wasapi/{guid}` or `auto`.
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Clone, Debug)]
pub struct PlayerOptions {
    /// `audio-device`; `None` = `auto`.
    pub audio_device: Option<String>,
    /// `audio-exclusive` (WASAPI exclusive on Windows).
    pub exclusive: bool,
    /// 0-100.
    pub volume: f64,
    /// Extra mpv options (name, value), applied before initialization.
    pub extra: Vec<(String, String)>,
}

impl Default for PlayerOptions {
    fn default() -> Self {
        Self {
            audio_device: None,
            exclusive: false,
            volume: 100.0,
            extra: Vec::new(),
        }
    }
}

const OBSERVED: &[(&str, Format)] = &[
    ("time-pos", Format::Double),
    ("duration", Format::Double),
    ("pause", Format::Flag),
    ("paused-for-cache", Format::Flag),
    ("playlist-pos", Format::Int64),
    ("idle-active", Format::Flag),
    ("volume", Format::Double),
    ("mute", Format::Flag),
];

pub struct Player {
    mpv: Arc<Mpv>,
    thread: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    output_change: Mutex<()>,
}

impl Player {
    pub fn new(
        options: &PlayerOptions,
        mut on_event: impl FnMut(PlayerEvent) + Send + 'static,
    ) -> Result<Self> {
        let mpv = Mpv::with_initializer(|init| {
            init.set_option("idle", "yes")?;
            init.set_option("config", "no")?;
            init.set_option("load-scripts", "no")?;
            init.set_option("video", "no")?;
            init.set_option("audio-display", "no")?;
            init.set_option("terminal", "no")?;
            init.set_option("audio-client-name", "032player")?;
            init.set_option("prefetch-playlist", "yes")?;
            // Without these, ffmpeg blocks forever on a dead TCP connection
            // (common after the system wakes from sleep).
            init.set_option("network-timeout", "10")?;
            init.set_option(
                "stream-lavf-o",
                "reconnect=1,reconnect_streamed=1,reconnect_at_eof=1,reconnect_delay_max=5",
            )?;
            init.set_option(
                "audio-device",
                options.audio_device.as_deref().unwrap_or("auto"),
            )?;
            init.set_option(
                "audio-exclusive",
                if options.exclusive { "yes" } else { "no" },
            )?;
            init.set_option("volume", options.volume.clamp(0.0, 100.0))?;
            for (name, value) in &options.extra {
                init.set_option(name, value.as_str())?;
            }
            Ok(())
        })?;
        mpv.disable_deprecated_events()?;
        for (id, (name, format)) in (0u64..).zip(OBSERVED) {
            mpv.observe_property(name, *format, id)?;
        }

        let mpv = Arc::new(mpv);
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let mpv = mpv.clone();
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("mpv-events".into())
                .spawn(move || event_loop(&mpv, &stop, &mut on_event))
                .map_err(|e| PlayerError::Mpv(e.to_string()))?
        };
        Ok(Self {
            mpv,
            thread: Some(thread),
            stop,
            output_change: Mutex::new(()),
        })
    }

    /// Replaces the playlist with `url` and starts it.
    pub fn load(&self, url: &str) -> Result<()> {
        Ok(self.mpv.command("loadfile", &[url, "replace"])?)
    }

    /// Appends `url` to the playlist (prefetched for gapless playback).
    pub fn append(&self, url: &str) -> Result<()> {
        Ok(self.mpv.command("loadfile", &[url, "append"])?)
    }

    /// Drops every playlist entry after the current one.
    pub fn clear_upcoming(&self) -> Result<()> {
        let pos: i64 = self.mpv.get_property("playlist-pos")?;
        let count: i64 = self.mpv.get_property("playlist-count")?;
        for index in (pos + 1..count).rev() {
            self.mpv.command("playlist-remove", &[&index.to_string()])?;
        }
        Ok(())
    }

    pub fn next(&self) -> Result<()> {
        Ok(self.mpv.command("playlist-next", &["force"])?)
    }

    pub fn stop(&self) -> Result<()> {
        Ok(self.mpv.command("stop", &[])?)
    }

    pub fn set_paused(&self, paused: bool) -> Result<()> {
        Ok(self.mpv.set_property("pause", paused)?)
    }

    pub fn toggle_pause(&self) -> Result<()> {
        Ok(self.mpv.command("cycle", &["pause"])?)
    }

    /// Absolute seek in seconds.
    pub fn seek(&self, seconds: f64) -> Result<()> {
        let target = format!("{:.3}", seconds.max(0.0));
        Ok(self.mpv.command("seek", &[&target, "absolute"])?)
    }

    /// 0-100.
    pub fn set_volume(&self, volume: f64) -> Result<()> {
        Ok(self.mpv.set_property("volume", volume.clamp(0.0, 100.0))?)
    }

    pub fn set_muted(&self, muted: bool) -> Result<()> {
        Ok(self.mpv.set_property("mute", muted)?)
    }

    pub fn audio_devices(&self) -> Result<Vec<AudioDevice>> {
        let json: String = self.mpv.get_property("audio-device-list")?;
        serde_json::from_str(&json).map_err(|e| PlayerError::InvalidResponse(e.to_string()))
    }

    pub fn audio_device(&self) -> Result<String> {
        Ok(self.mpv.get_property("audio-device")?)
    }

    pub fn set_audio_device(&self, name: &str) -> Result<()> {
        let _guard = self
            .output_change
            .try_lock()
            .map_err(|_| PlayerError::Busy)?;
        Ok(self.mpv.set_property("audio-device", name)?)
    }

    pub fn exclusive(&self) -> Result<bool> {
        Ok(self.mpv.get_property("audio-exclusive")?)
    }

    /// Switches exclusive mode, also while playing. Reads the option back
    /// and rolls back on failure (like `audio-exclusive.ts`). Success means
    /// mpv accepted the option, not that the hardware honours it.
    pub fn set_exclusive(&self, enabled: bool) -> Result<()> {
        let _guard = self
            .output_change
            .try_lock()
            .map_err(|_| PlayerError::Busy)?;
        let previous = self.exclusive()?;
        if previous == enabled {
            return Ok(());
        }
        let result = self
            .mpv
            .set_property("audio-exclusive", enabled)
            .map_err(PlayerError::from)
            .and_then(|()| match self.exclusive()? {
                applied if applied == enabled => Ok(()),
                _ => Err(PlayerError::ExclusiveNotApplied),
            });
        if result.is_err() {
            let _ = self.mpv.set_property("audio-exclusive", previous);
        }
        result
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.mpv.command("quit", &[]);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn end_reason(raw: libmpv2::EndFileReason) -> EndReason {
    match raw {
        mpv_end_file_reason::Eof => EndReason::Eof,
        mpv_end_file_reason::Quit => EndReason::Quit,
        mpv_end_file_reason::Error => EndReason::Error,
        mpv_end_file_reason::Redirect => EndReason::Redirect,
        _ => EndReason::Stop,
    }
}

fn property_event(name: &str, change: &PropertyData<'_>) -> Option<PlayerEvent> {
    Some(match (name, change) {
        ("time-pos", PropertyData::Double(v)) => PlayerEvent::TimePos(*v),
        ("duration", PropertyData::Double(v)) => PlayerEvent::Duration(*v),
        ("pause", PropertyData::Flag(v)) => PlayerEvent::Paused(*v),
        ("paused-for-cache", PropertyData::Flag(v)) => PlayerEvent::Buffering(*v),
        ("playlist-pos", PropertyData::Int64(v)) => PlayerEvent::PlaylistPos(*v),
        ("idle-active", PropertyData::Flag(v)) => PlayerEvent::Idle(*v),
        ("volume", PropertyData::Double(v)) => PlayerEvent::Volume(*v),
        ("mute", PropertyData::Flag(v)) => PlayerEvent::Muted(*v),
        _ => return None,
    })
}

fn event_loop(mpv: &Mpv, stop: &AtomicBool, on_event: &mut impl FnMut(PlayerEvent)) {
    loop {
        // None is either a timeout or a property that became unavailable.
        let Some(event) = mpv.wait_event(0.5) else {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            continue;
        };
        let out = match event {
            Ok(Event::Shutdown) => {
                on_event(PlayerEvent::Shutdown);
                return;
            }
            Ok(Event::PropertyChange { name, change, .. }) => property_event(name, &change),
            Ok(Event::FileLoaded) => Some(PlayerEvent::FileLoaded),
            Ok(Event::PlaybackRestart) => Some(PlayerEvent::PlaybackRestart),
            Ok(Event::EndFile(reason)) => Some(PlayerEvent::EndFile {
                reason: end_reason(reason),
                error: None,
            }),
            // Only end-file (and async replies, which are unused) carry an error.
            Err(e) => Some(PlayerEvent::EndFile {
                reason: EndReason::Error,
                error: Some(format!("{e:?}")),
            }),
            Ok(_) => None,
        };
        if let Some(out) = out {
            on_event(out);
        }
    }
}
