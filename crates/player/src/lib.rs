//! Audio players: `Player` (libmpv) and `AsioPlayer` (clean-room Windows ASIO).

mod asio_player;
mod decoder;
mod http_source;
mod limiter;

pub use asio::{AsioCapabilities, AsioChannelPair, AsioDriverInfo, AsioSampleType, AsioTarget};
#[cfg(windows)]
pub use asio_player::WindowsAsioSink;
pub use asio_player::{AsioPlayer, AudioSink};
pub use limiter::SoftPeakLimiter;

use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
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
    /// A file started; `path` is its URL (mpv `path`).
    FileLoaded {
        path: Option<String>,
    },
    EndFile {
        reason: EndReason,
        /// mpv error text for [`EndReason::Error`].
        error: Option<String>,
    },
    /// A seek finished and playback continues.
    PlaybackRestart,
    Shutdown,
}

/// Per-track loudness normalization parameters consumed by audio backends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoudnessParams {
    pub enabled: bool,
    pub gain_db: f64,
    pub ceiling_db: f64,
    pub use_limiter: bool,
}

impl Default for LoudnessParams {
    fn default() -> Self {
        Self {
            enabled: false,
            gain_db: 0.0,
            ceiling_db: -1.0,
            use_limiter: false,
        }
    }
}

pub(crate) fn db_to_linear(db: f64) -> f64 {
    if db.is_finite() {
        10.0_f64.powf(db / 20.0)
    } else {
        1.0
    }
}

/// Builds the mpv `af` filter chain for loudness normalization.
///
/// Always passes `level=disabled` to FFmpeg `alimiter` so it never applies
/// automatic makeup gain on top of the planned loudness gain. Returns an empty
/// string when disabled or when unity gain without limiting is requested.
#[must_use]
pub fn build_mpv_af(params: LoudnessParams) -> String {
    if !params.enabled {
        return String::new();
    }
    let gain_db = if params.gain_db.is_finite() {
        params.gain_db.clamp(-24.0, 18.0)
    } else {
        0.0
    };
    if !params.use_limiter && gain_db.abs() < 0.05 {
        return String::new();
    }
    if !params.use_limiter {
        return format!("lavfi=[volume={gain_db:.2}dB:precision=float]");
    }
    let limit = db_to_linear(params.ceiling_db).clamp(0.0625, 1.0);
    format!(
        "lavfi=[volume={gain_db:.2}dB:precision=float,alimiter=limit={limit:.5}:attack=5:release=50:asc=0:level=disabled]"
    )
}

/// Formats the per-file `options` argument passed to mpv's 4-arg `loadfile`.
#[must_use]
pub fn build_mpv_loadfile_af_option(params: LoudnessParams) -> String {
    let af = build_mpv_af(params);
    format!("af=\"{af}\"")
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
    /// Optional diagnostic log file written by `event_loop` after redacting
    /// query-carried credentials (`mediaToken`) and bearer tokens.
    pub log_file: Option<PathBuf>,
    /// Minimum libmpv log level requested via `mpv_request_log_messages`
    /// when `log_file` is set (defaults to `"warn"`).
    pub log_level: Option<String>,
    /// Extra mpv options (name, value), applied before initialization.
    pub extra: Vec<(String, String)>,
}

impl Default for PlayerOptions {
    fn default() -> Self {
        Self {
            audio_device: None,
            exclusive: false,
            volume: 100.0,
            log_file: None,
            log_level: None,
            extra: Vec::new(),
        }
    }
}

/// Redacts `mediaToken=...` query values and `Bearer ...` credentials from
/// libmpv log lines before they are written to disk.
#[must_use]
pub fn redact_log_text(input: &str) -> String {
    fn is_delim(b: u8) -> bool {
        matches!(
            b,
            b'&' | b'#'
                | b'\''
                | b'"'
                | b')'
                | b']'
                | b'>'
                | b','
                | b';'
                | b' '
                | b'\t'
                | b'\r'
                | b'\n'
        )
    }

    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        let rest = &input[i..];
        let prev = i.checked_sub(1).map(|p| bytes[p]);
        if rest.len() >= 11
            && rest.as_bytes()[..11].eq_ignore_ascii_case(b"mediatoken=")
            && prev.is_none_or(|b| matches!(b, b'?' | b'&' | b' ' | b'\t' | b'\'' | b'"' | b'('))
        {
            out.push_str(&rest[..11]);
            out.push_str("<redacted>");
            i += 11;
            while i < bytes.len() && !is_delim(bytes[i]) {
                i += 1;
            }
        } else if rest.len() >= 7
            && rest.as_bytes()[..7].eq_ignore_ascii_case(b"bearer ")
            && prev.is_none_or(|b| matches!(b, b':' | b' ' | b'\t' | b'\'' | b'"'))
        {
            out.push_str(&rest[..7]);
            out.push_str("<redacted>");
            i += 7;
            while i < bytes.len() && bytes[i] == b' ' {
                i += 1;
            }
            while i < bytes.len() && !is_delim(bytes[i]) {
                i += 1;
            }
        } else if let Some(ch) = rest.chars().next() {
            out.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }
    out
}

#[allow(unsafe_code)]
fn request_log_messages(mpv: &Mpv, min_level: &str) -> Result<()> {
    let level =
        CString::new(min_level).map_err(|_| PlayerError::Mpv("invalid mpv log level".into()))?;
    // SAFETY: `mpv.ctx` is a valid initialized `mpv_handle` owned by `mpv` and
    // `level` is a valid NUL-terminated C string.
    let rc = unsafe { libmpv2_sys::mpv_request_log_messages(mpv.ctx.as_ptr(), level.as_ptr()) };
    if rc == 0 {
        Ok(())
    } else {
        Err(PlayerError::from(libmpv2::Error::Raw(rc)))
    }
}

fn open_log_file(path: &std::path::Path) -> Option<File> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
    {
        Ok(file) => Some(file),
        Err(e) => {
            tracing::warn!("opening mpv log {}: {e}", path.display());
            None
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
    /// Fallback `af` applied on the next `FileLoaded` when an older libmpv
    /// rejects 4-argument per-file `loadfile` options.
    pending_fallback_af: Arc<Mutex<Option<String>>>,
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
            // Allow for scheduling delays without starving the WASAPI output buffer.
            #[cfg(target_os = "windows")]
            init.set_option("wasapi-exclusive-buffer", "100000")?;
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
        let log_file = options.log_file.as_deref().and_then(open_log_file);
        if log_file.is_some() {
            let level = options.log_level.as_deref().unwrap_or("warn");
            if let Err(e) = request_log_messages(&mpv, level) {
                tracing::warn!("requesting mpv log messages ({level}): {e}");
            }
        }

        let mpv = Arc::new(mpv);
        let stop = Arc::new(AtomicBool::new(false));
        let pending_fallback_af = Arc::new(Mutex::new(None));
        let thread = {
            let mpv = mpv.clone();
            let stop = stop.clone();
            let pending_af = pending_fallback_af.clone();
            std::thread::Builder::new()
                .name("mpv-events".into())
                .spawn(move || event_loop(&mpv, &stop, &pending_af, log_file, &mut on_event))
                .map_err(|e| PlayerError::Mpv(e.to_string()))?
        };
        Ok(Self {
            mpv,
            thread: Some(thread),
            stop,
            output_change: Mutex::new(()),
            pending_fallback_af,
        })
    }

    /// Replaces the playlist with `url` and starts it.
    pub fn load(&self, url: &str) -> Result<()> {
        *self.pending_fallback_af.lock().expect("pending af lock") = None;
        Ok(self.mpv.command("loadfile", &[url, "replace"])?)
    }

    /// Replaces the playlist with `url` and applies `params` via mpv `af`
    /// (keeping `volume-gain` reserved for play/pause fades).
    pub fn load_with_loudness(&self, url: &str, params: LoudnessParams) -> Result<()> {
        *self.pending_fallback_af.lock().expect("pending af lock") = None;
        self.set_loudness(params)?;
        Ok(self.mpv.command("loadfile", &[url, "replace"])?)
    }

    /// Appends `url` to the playlist (prefetched for gapless playback).
    pub fn append(&self, url: &str) -> Result<()> {
        *self.pending_fallback_af.lock().expect("pending af lock") = None;
        Ok(self.mpv.command("loadfile", &[url, "append"])?)
    }

    /// Appends `url` with per-file loudness `af` options (mpv 0.38+), falling
    /// back to 2-arg `loadfile` + `FileLoaded` `af` update on older libmpv.
    pub fn append_with_loudness(&self, url: &str, params: LoudnessParams) -> Result<()> {
        let opt = build_mpv_loadfile_af_option(params);
        match self.mpv.command("loadfile", &[url, "append", "-1", &opt]) {
            Ok(()) => {
                *self.pending_fallback_af.lock().expect("pending af lock") = None;
                Ok(())
            }
            Err(err) => {
                tracing::debug!(
                    ?err,
                    "mpv rejected 4-arg loadfile per-file af; falling back to FileLoaded update"
                );
                *self.pending_fallback_af.lock().expect("pending af lock") =
                    Some(build_mpv_af(params));
                Ok(self.mpv.command("loadfile", &[url, "append"])?)
            }
        }
    }

    /// Updates loudness normalization live via mpv's `af` property.
    pub fn set_loudness(&self, params: LoudnessParams) -> Result<()> {
        let af = build_mpv_af(params);
        Ok(self.mpv.set_property("af", af.as_str())?)
    }

    /// HTTP proxy (`http://host:port`) for streams opened from now on;
    /// `None` connects directly.
    pub fn set_http_proxy(&self, proxy: Option<&str>) -> Result<()> {
        Ok(self.mpv.set_property("http-proxy", proxy.unwrap_or(""))?)
    }

    /// Drops every playlist entry except the currently playing one.
    pub fn clear_upcoming(&self) -> Result<()> {
        *self.pending_fallback_af.lock().expect("pending af lock") = None;
        Ok(self.mpv.command("playlist-clear", &[])?)
    }

    pub fn next(&self) -> Result<()> {
        Ok(self.mpv.command("playlist-next", &["force"])?)
    }

    pub fn stop(&self) -> Result<()> {
        *self.pending_fallback_af.lock().expect("pending af lock") = None;
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

    /// Extra linear gain on top of `volume` (0 silent, 1 unchanged), via
    /// mpv `volume-gain` in dB (default range -96..12). Used for fades;
    /// `volume` and its observers stay untouched.
    pub fn set_fade_gain(&self, gain: f64) -> Result<()> {
        let db = if gain > 0.0 {
            (20.0 * gain.min(1.0).log10()).max(-96.0)
        } else {
            -96.0
        };
        Ok(self.mpv.set_property("volume-gain", db)?)
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

/// Enumerates all installed ASIO drivers registered on the system.
#[must_use]
pub fn list_asio_drivers() -> Vec<AsioDriverInfo> {
    asio::list_drivers()
}

/// Probes an ASIO driver by CLSID to inspect its output ports and supported
/// sample rates.
pub fn probe_asio_driver(clsid: &str) -> Result<AsioCapabilities> {
    asio::probe_driver(clsid).map_err(|e| PlayerError::Mpv(e.to_string()))
}

/// Opens an ASIO driver's native control panel standalone (when no ASIO stream
/// is currently active).
pub fn open_asio_control_panel_standalone(clsid: &str) -> Result<()> {
    asio::open_control_panel_standalone(clsid).map_err(|e| PlayerError::Mpv(e.to_string()))
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

fn event_loop(
    mpv: &Mpv,
    stop: &AtomicBool,
    pending_fallback_af: &Mutex<Option<String>>,
    mut log_file: Option<File>,
    on_event: &mut impl FnMut(PlayerEvent),
) {
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
            Ok(Event::LogMessage {
                prefix,
                level,
                text,
                ..
            }) => {
                if let Some(file) = log_file.as_mut() {
                    let redacted = redact_log_text(text.trim_end_matches(['\r', '\n']));
                    let _ = writeln!(file, "[{prefix}] {level}: {redacted}");
                }
                None
            }
            Ok(Event::PropertyChange { name, change, .. }) => property_event(name, &change),
            Ok(Event::FileLoaded) => {
                if let Some(af) = pending_fallback_af.lock().expect("pending af lock").take() {
                    let _ = mpv.set_property("af", af.as_str());
                }
                Some(PlayerEvent::FileLoaded {
                    path: mpv.get_property::<String>("path").ok(),
                })
            }
            Ok(Event::PlaybackRestart) => Some(PlayerEvent::PlaybackRestart),
            Ok(Event::EndFile(reason)) => Some(PlayerEvent::EndFile {
                reason: end_reason(reason),
                error: None,
            }),
            // A non-UTF-8 log line or property string must not abort playback.
            Err(libmpv2::Error::InvalidUtf8) => {
                tracing::debug!("skipping non-UTF-8 mpv event");
                None
            }
            // Only end-file (and async replies, which are unused) carry an error.
            Err(e) => Some(PlayerEvent::EndFile {
                reason: EndReason::Error,
                error: Some(redact_log_text(&format!("{e:?}"))),
            }),
            Ok(_) => None,
        };
        if let Some(out) = out {
            if let PlayerEvent::Buffering(buffering) = &out {
                tracing::info!(buffering, "network cache buffering changed");
            }
            on_event(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LoudnessParams, build_mpv_af, build_mpv_loadfile_af_option, redact_log_text};

    #[test]
    fn redacts_media_token_and_bearer_in_mpv_logs() {
        assert_eq!(
            redact_log_text(
                "Opening 'https://music.example.com/api/v1/tracks/5/stream?mediaToken=s3cr3t%26abc&foo=1'"
            ),
            "Opening 'https://music.example.com/api/v1/tracks/5/stream?mediaToken=<redacted>&foo=1'"
        );
        assert_eq!(
            redact_log_text("Authorization: Bearer top-secret-token\n曲目：星街すいせい"),
            "Authorization: Bearer <redacted>\n曲目：星街すいせい"
        );
        assert_eq!(
            redact_log_text("ao/wasapi: buffer underrun"),
            "ao/wasapi: buffer underrun"
        );
    }

    #[test]
    fn builds_mpv_loudness_af_strings_correctly() {
        // Disabled -> empty string (complete bypass).
        assert_eq!(build_mpv_af(LoudnessParams::default()), "");
        assert_eq!(
            build_mpv_loadfile_af_option(LoudnessParams::default()),
            "af=\"\""
        );

        // Enabled at near-zero unity gain without limiter -> empty string.
        assert_eq!(
            build_mpv_af(LoudnessParams {
                enabled: true,
                gain_db: 0.02,
                ceiling_db: -1.0,
                use_limiter: false,
            }),
            ""
        );

        // Pure linear gain without limiter.
        assert_eq!(
            build_mpv_af(LoudnessParams {
                enabled: true,
                gain_db: -3.25,
                ceiling_db: -1.0,
                use_limiter: false,
            }),
            "lavfi=[volume=-3.25dB:precision=float]"
        );

        // Gain with soft limiter MUST include level=disabled to prevent auto-boost.
        let with_lim = build_mpv_af(LoudnessParams {
            enabled: true,
            gain_db: 2.5,
            ceiling_db: -1.0,
            use_limiter: true,
        });
        assert_eq!(
            with_lim,
            "lavfi=[volume=2.50dB:precision=float,alimiter=limit=0.89125:attack=5:release=50:asc=0:level=disabled]"
        );
        assert_eq!(
            build_mpv_loadfile_af_option(LoudnessParams {
                enabled: true,
                gain_db: 2.5,
                ceiling_db: -1.0,
                use_limiter: true,
            }),
            format!("af=\"{with_lim}\"")
        );
    }
}
