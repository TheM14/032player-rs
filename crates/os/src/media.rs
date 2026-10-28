//! System media controls: SMTC on Windows (media keys, the volume flyout,
//! the lock screen), MPRIS on Linux, Now Playing on macOS.

use std::time::Duration;

use souvlaki::{
    MediaControlEvent, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig, SeekDirection,
};

/// Step of the "fast forward" / "rewind" buttons.
const SEEK_STEP: Duration = Duration::from_secs(10);

/// A request from the system (media keys, SMTC flyout, MPRIS client).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaCommand {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
    Stop,
    SeekTo(Duration),
    /// Relative seek; `forward` false goes back.
    SeekBy {
        forward: bool,
        by: Duration,
    },
    /// Show the main window.
    Raise,
    Quit,
}

impl MediaCommand {
    fn from_event(event: MediaControlEvent) -> Option<Self> {
        Some(match event {
            MediaControlEvent::Play => Self::Play,
            MediaControlEvent::Pause => Self::Pause,
            MediaControlEvent::Toggle => Self::Toggle,
            MediaControlEvent::Next => Self::Next,
            MediaControlEvent::Previous => Self::Previous,
            MediaControlEvent::Stop => Self::Stop,
            MediaControlEvent::Seek(direction) => Self::SeekBy {
                forward: direction == SeekDirection::Forward,
                by: SEEK_STEP,
            },
            MediaControlEvent::SeekBy(direction, by) => Self::SeekBy {
                forward: direction == SeekDirection::Forward,
                by,
            },
            MediaControlEvent::SetPosition(MediaPosition(at)) => Self::SeekTo(at),
            MediaControlEvent::Raise => Self::Raise,
            MediaControlEvent::Quit => Self::Quit,
            _ => return None,
        })
    }
}

/// What is playing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// `http(s)://` or `file://` URL of the cover.
    pub cover_url: Option<String>,
    pub duration: Option<Duration>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Playback {
    Stopped,
    Paused(Duration),
    Playing(Duration),
}

pub struct MediaControls(souvlaki::MediaControls);

impl MediaControls {
    /// `window` is the native window handle (HWND), required on Windows.
    /// `on_command` runs on a system thread.
    pub fn new(
        window: Option<isize>,
        on_command: impl Fn(MediaCommand) + Send + 'static,
    ) -> Result<Self, String> {
        if cfg!(windows) && window.is_none() {
            return Err("no window handle".into());
        }
        let config = PlatformConfig {
            display_name: "032player",
            // A D-Bus name element must not start with a digit.
            dbus_name: "music032player",
            hwnd: window.map(|h| std::ptr::without_provenance_mut(h.cast_unsigned())),
        };
        let mut controls = souvlaki::MediaControls::new(config).map_err(|e| format!("{e:?}"))?;
        controls
            .attach(move |event| {
                if let Some(command) = MediaCommand::from_event(event) {
                    on_command(command);
                }
            })
            .map_err(|e| format!("{e:?}"))?;
        Ok(Self(controls))
    }

    pub fn set_metadata(&mut self, metadata: &Metadata) {
        let result = self.0.set_metadata(MediaMetadata {
            title: Some(&metadata.title),
            artist: Some(&metadata.artist),
            album: Some(&metadata.album),
            cover_url: metadata.cover_url.as_deref(),
            duration: metadata.duration,
        });
        if let Err(e) = result {
            tracing::debug!("media controls metadata: {e:?}");
        }
    }

    pub fn set_playback(&mut self, playback: Playback) {
        let playback = match playback {
            Playback::Stopped => MediaPlayback::Stopped,
            Playback::Paused(at) => MediaPlayback::Paused {
                progress: Some(MediaPosition(at)),
            },
            Playback::Playing(at) => MediaPlayback::Playing {
                progress: Some(MediaPosition(at)),
            },
        };
        if let Err(e) = self.0.set_playback(playback) {
            tracing::debug!("media controls playback: {e:?}");
        }
    }
}
