//! User settings in `settings.json`. Missing or corrupt files and fields
//! fall back to defaults.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::queue::Repeat;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// 0-100.
    pub volume: f64,
    /// mpv `audio-device`; `None` = system default (`auto`).
    pub audio_device: Option<String>,
    /// WASAPI exclusive mode (`audio-exclusive`).
    pub exclusive: bool,
    /// Prefer ASIO output when `exclusive` is enabled and available.
    pub asio: bool,
    /// User-selected ASIO driver CLSID.
    pub asio_driver: Option<String>,
    /// Per-driver ASIO output port settings keyed by CLSID.
    pub asio_profiles: BTreeMap<String, AsioProfile>,
    pub right_panel: RightPanel,
    /// Left sidebar width in logical px (dragged by its right edge).
    pub sidebar_width: f32,
    /// Main window size when it was last closed.
    pub window: WindowState,
    pub repeat: Repeat,
    pub shuffle: bool,
    /// Closing the window hides it to the tray (Windows).
    pub close_to_tray: bool,
    /// 300 ms volume fade on play / pause (`audioFadeOnStatusChange`).
    pub fade_on_pause: bool,
    /// Auto-append 032 similar tracks when approaching the end of the queue.
    pub auto_play_similar: bool,
    /// 032 Dark / Light or the system's choice.
    pub theme: ThemeMode,
    /// Stream a transcode instead of the original file (`None`: original,
    /// the bit-perfect default). Ignored when the server cannot.
    pub transcode: Option<api::Transcode>,
    pub lyrics: LyricsSettings,
    pub shortcuts: crate::shortcuts::Shortcuts,
    /// Sonos output (MusicBridge).
    pub cast: CastSettings,
    /// One-time layout migration version (0 = legacy pre-v3 defaults, 1 = migrated).
    #[serde(default)]
    pub layout_version: u32,
}

/// Exclusive audio output mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExclusiveMode {
    #[default]
    Off,
    Wasapi,
    Asio,
}

/// Per-driver ASIO configuration persisted by CLSID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AsioProfile {
    /// Selected stereo output channel pair `[left, right]` (0-based indices).
    pub channel_pair: [usize; 2],
}

impl Default for AsioProfile {
    fn default() -> Self {
        Self {
            channel_pair: [0, 1],
        }
    }
}

impl AsioProfile {
    fn sanitize(&mut self) {
        if self.channel_pair[0] == self.channel_pair[1] {
            self.channel_pair = [0, 1];
        }
    }
}

/// Sonos hi-res lossless (> 48 kHz) tracks (MusicBridge `SonosHiResTranscodeMode`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CastHiRes {
    /// Live MP3 320k from the server (recommended: starts at once).
    #[default]
    Mp3,
    /// FLAC at 48 kHz from the server cache (the first play waits for it).
    Flac48,
    /// The original file (some speakers cannot play it).
    Original,
}

/// Sonos Opus tracks (MusicBridge `OpusTranscodeMode`; Sonos has no Opus).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CastOpus {
    /// Live MP3 320k.
    #[default]
    Mp3,
    /// FLAC from the server cache.
    Flac,
}

/// A Sonos room chosen as the output, remembered across restarts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CastRoomSetting {
    /// Coordinator UUID.
    pub id: String,
    pub name: String,
    /// `http://ip:1400` when last seen (refreshed by discovery).
    pub base_url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CastSettings {
    /// The selected room; `None`: this computer.
    pub room: Option<CastRoomSetting>,
    pub hires: CastHiRes,
    pub opus: CastOpus,
    /// Speaker addresses probed besides SSDP (`192.168.1.30`, comma or
    /// space separated), for networks where discovery does not get through.
    pub hosts: String,
}

/// The right "now playing" panel: the current track over 歌词 | 队列 tabs
/// (v3 redesign). `queue`, `lyrics`, `lyrics_first` and `split` described
/// the earlier stacked layout (Electron `sidebar-play-queue`); they are
/// still read and written so older files round-trip, but nothing uses them.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RightPanel {
    /// The player bar's lyrics / queue buttons open and close the panel.
    pub open: bool,
    /// The queue tab is selected (else lyrics).
    pub queue_tab: bool,
    pub queue: bool,
    pub lyrics: bool,
    pub lyrics_first: bool,
    pub split: f32,
    /// Width in logical px (wide windows).
    pub width: f32,
}

/// Layout migration version; older files without `layoutVersion` migrate once.
const LAYOUT_VERSION: u32 = 1;
/// Default panel width before the redesign; legacy files still holding it
/// get migrated once to the new default.
const OLD_PANEL_WIDTH: f32 = 360.0;
/// Default sidebar width before the redesign (see [`OLD_PANEL_WIDTH`]).
const OLD_SIDEBAR_WIDTH: f32 = 210.0;

impl Default for RightPanel {
    fn default() -> Self {
        Self {
            open: true,
            queue_tab: false,
            queue: true,
            lyrics: true,
            lyrics_first: false,
            split: 0.5,
            width: 420.0,
        }
    }
}

impl RightPanel {
    fn sanitize(&mut self, migrate_legacy: bool) {
        let d = Self::default();
        if migrate_legacy && (self.width - OLD_PANEL_WIDTH).abs() < 0.01 {
            self.width = d.width;
        }
        self.split = if self.split.is_finite() {
            self.split.clamp(0.15, 0.85)
        } else {
            d.split
        };
        self.width = if self.width.is_finite() {
            self.width.clamp(260.0, 720.0)
        } else {
            d.width
        };
    }
}

/// Main window size (logical px), position (outer top-left, physical px;
/// `None`: let the system place it), both from before maximizing, and
/// whether it was maximized.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WindowState {
    pub width: f32,
    pub height: f32,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub maximized: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            width: 1280.0,
            height: 800.0,
            x: None,
            y: None,
            maximized: false,
        }
    }
}

impl WindowState {
    /// At least the window's minimum size (960 x 600), at most 8K.
    pub fn sanitize(&mut self) {
        let d = Self::default();
        let fix = |v: f32, min: f32, default: f32| {
            if v.is_finite() {
                v.clamp(min, 7680.0)
            } else {
                default
            }
        };
        self.width = fix(self.width, 960.0, d.width);
        self.height = fix(self.height, 600.0, d.height);
        // Both or neither; absurd values (e.g. a minimized window's
        // -32000 on Windows) fall back to the system placement.
        let sane = |v: Option<i32>| v.filter(|v| (-20_000..=40_000).contains(v));
        if let (Some(x), Some(y)) = (sane(self.x), sane(self.y)) {
            (self.x, self.y) = (Some(x), Some(y));
        } else {
            (self.x, self.y) = (None, None);
        }
    }
}

/// Default left sidebar width (logical px).
pub const SIDEBAR_WIDTH: f32 = 248.0;

/// Sidebar width limits (Electron allows dragging between these).
pub fn sidebar_width(width: f32) -> f32 {
    if width.is_finite() {
        width.clamp(180.0, 400.0)
    } else {
        SIDEBAR_WIDTH
    }
}

/// Color scheme of the app. Dark by default (the user's choice).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThemeMode {
    /// Follow the system light / dark setting.
    System,
    #[default]
    Dark,
    Light,
}

/// How translated lyrics show (`lyrics.translationDisplayMode`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TranslationMode {
    /// The translation under each line.
    #[default]
    Bilingual,
    /// The translation instead of the original (same timing).
    Translated,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LyricsSettings {
    /// `targetLanguage` of the server translation (zh-Hans / zh-Hant / en).
    pub translation_language: String,
    pub translation_mode: TranslationMode,
    /// Translate as soon as lyrics load (server translation enabled).
    pub auto_translate: bool,
}

impl Default for LyricsSettings {
    fn default() -> Self {
        Self {
            translation_language: "zh-Hans".into(),
            translation_mode: TranslationMode::Bilingual,
            auto_translate: false,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 100.0,
            audio_device: None,
            exclusive: false,
            asio: false,
            asio_driver: None,
            asio_profiles: BTreeMap::new(),
            right_panel: RightPanel::default(),
            sidebar_width: SIDEBAR_WIDTH,
            window: WindowState::default(),
            repeat: Repeat::Off,
            shuffle: false,
            close_to_tray: false,
            fade_on_pause: true,
            auto_play_similar: true,
            theme: ThemeMode::Dark,
            transcode: None,
            lyrics: LyricsSettings::default(),
            shortcuts: crate::shortcuts::Shortcuts::default(),
            cast: CastSettings::default(),
            layout_version: LAYOUT_VERSION,
        }
    }
}

impl Settings {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("settings.json")
    }

    /// Effective exclusive mode persisted in settings.
    #[must_use]
    pub fn exclusive_mode(&self) -> ExclusiveMode {
        match (self.exclusive, self.asio) {
            (false, _) => ExclusiveMode::Off,
            (true, false) => ExclusiveMode::Wasapi,
            (true, true) => ExclusiveMode::Asio,
        }
    }

    /// Updates `exclusive` and `asio` flags from `mode`.
    pub fn set_exclusive_mode(&mut self, mode: ExclusiveMode) {
        match mode {
            ExclusiveMode::Off => {
                self.exclusive = false;
                self.asio = false;
            }
            ExclusiveMode::Wasapi => {
                self.exclusive = true;
                self.asio = false;
            }
            ExclusiveMode::Asio => {
                self.exclusive = true;
                self.asio = true;
            }
        }
    }

    /// Returns the saved stereo channel pair `(left, right)` for `clsid`.
    #[must_use]
    pub fn asio_channel_pair(&self, clsid: &str) -> (usize, usize) {
        let key = normalize_asio_clsid(clsid);
        self.asio_profiles
            .get(&key)
            .or_else(|| {
                self.asio_profiles
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(&key))
                    .map(|(_, v)| v)
            })
            .filter(|p| p.channel_pair[0] != p.channel_pair[1])
            .map_or((0, 1), |p| (p.channel_pair[0], p.channel_pair[1]))
    }

    /// Saves the selected stereo channel pair `(left, right)` for `clsid`.
    pub fn set_asio_channel_pair(&mut self, clsid: &str, left: usize, right: usize) {
        if left == right {
            return;
        }
        let key = normalize_asio_clsid(clsid);
        if key.is_empty() {
            return;
        }
        self.asio_profiles
            .retain(|k, _| k == &key || !k.eq_ignore_ascii_case(&key));
        let profile = self.asio_profiles.entry(key).or_default();
        profile.channel_pair = [left, right];
    }

    /// Reads `settings.json` in `data_dir`; backs up corrupt files and falls
    /// back to defaults when missing or corrupt.
    pub fn load(data_dir: &Path) -> Self {
        let path = Self::path(data_dir);
        let mut settings: Self = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(parsed) => parsed,
                Err(e) => {
                    let backup = data_dir.join("settings.json.bak");
                    tracing::warn!(
                        error = %e,
                        path = %path.display(),
                        backup = %backup.display(),
                        "corrupt settings.json; backing up and resetting to defaults"
                    );
                    if let Err(copy_err) = fs::copy(&path, &backup) {
                        tracing::warn!(error = %copy_err, "failed to back up corrupt settings.json");
                    }
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "could not read settings.json");
                Self::default()
            }
        };
        let migrate_legacy = settings.layout_version < LAYOUT_VERSION;
        if !settings.volume.is_finite() {
            settings.volume = 100.0;
        }
        settings.volume = settings.volume.clamp(0.0, 100.0);
        settings.right_panel.sanitize(migrate_legacy);
        if migrate_legacy && (settings.sidebar_width - OLD_SIDEBAR_WIDTH).abs() < 0.01 {
            settings.sidebar_width = SIDEBAR_WIDTH;
        }
        settings.sidebar_width = sidebar_width(settings.sidebar_width);
        settings.layout_version = LAYOUT_VERSION;
        settings.window.sanitize();
        settings.shortcuts.merge_defaults();
        settings.audio_device = settings
            .audio_device
            .filter(|d| !d.is_empty() && d != "auto");
        settings.asio_driver = settings
            .asio_driver
            .map(|s| normalize_asio_clsid(&s))
            .filter(|s| !s.is_empty());
        let mut normalized_profiles = BTreeMap::new();
        for (clsid, mut profile) in std::mem::take(&mut settings.asio_profiles) {
            let key = normalize_asio_clsid(&clsid);
            if key.is_empty() {
                continue;
            }
            profile.sanitize();
            normalized_profiles.insert(key, profile);
        }
        settings.asio_profiles = normalized_profiles;
        settings
    }

    /// Writes atomically (temp file + rename).
    pub fn save(&self, data_dir: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        fs::create_dir_all(data_dir).map_err(|e| e.to_string())?;
        let path = Self::path(data_dir);
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &path).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            e.to_string()
        })
    }
}

fn normalize_asio_clsid(clsid: &str) -> String {
    clsid.trim().to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_tolerates_bad_files() {
        let dir = std::env::temp_dir().join(format!("032player-settings-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(Settings::load(&dir), Settings::default());

        let settings = Settings {
            volume: 42.0,
            audio_device: Some("wasapi/{x}".into()),
            exclusive: true,
            asio: true,
            asio_driver: Some("{TEST-ASIO-CLSID}".into()),
            asio_profiles: BTreeMap::from([(
                "{TEST-ASIO-CLSID}".into(),
                AsioProfile {
                    channel_pair: [2, 3],
                },
            )]),
            right_panel: RightPanel {
                open: false,
                queue_tab: true,
                queue: true,
                lyrics: false,
                lyrics_first: true,
                split: 0.3,
                width: 420.0,
            },
            sidebar_width: 260.0,
            window: WindowState {
                width: 1600.0,
                height: 1000.0,
                x: Some(-1800),
                y: Some(40),
                maximized: true,
            },
            repeat: Repeat::One,
            shuffle: true,
            close_to_tray: true,
            fade_on_pause: false,
            auto_play_similar: false,
            theme: ThemeMode::Light,
            shortcuts: crate::shortcuts::Shortcuts::default(),
            transcode: Some(api::Transcode::Mp3(320)),
            lyrics: LyricsSettings {
                translation_language: "en".into(),
                translation_mode: TranslationMode::Translated,
                auto_translate: true,
            },
            cast: CastSettings {
                room: Some(CastRoomSetting {
                    id: "RINCON_X".into(),
                    name: "客厅".into(),
                    base_url: "http://10.0.0.2:1400".into(),
                }),
                hires: CastHiRes::Flac48,
                opus: CastOpus::Flac,
                hosts: "10.0.0.2".into(),
            },
            layout_version: LAYOUT_VERSION,
        };
        settings.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir), settings);

        fs::write(
            Settings::path(&dir),
            br#"{"volume": 250, "audioDevice": "auto"}"#,
        )
        .unwrap();
        let loaded = Settings::load(&dir);
        assert_eq!(loaded.volume, 100.0);
        assert_eq!(loaded.audio_device, None);
        assert_eq!(loaded.right_panel, RightPanel::default());

        fs::write(
            Settings::path(&dir),
            br#"{"rightPanel": {"split": 3, "width": 10}}"#,
        )
        .unwrap();
        let loaded = Settings::load(&dir).right_panel;
        assert_eq!((loaded.split, loaded.width), (0.85, 260.0));

        fs::write(Settings::path(&dir), br#"{"sidebarWidth": 900}"#).unwrap();
        assert_eq!(Settings::load(&dir).sidebar_width, 400.0);

        // The old defaults move to the redesign's (248 / 420); dragged
        // widths stay, and the old stacked-panel fields are ignored.
        fs::write(
            Settings::path(&dir),
            br#"{"sidebarWidth": 210, "rightPanel": {"width": 360, "split": 0.3, "lyricsFirst": true}}"#,
        )
        .unwrap();
        let loaded = Settings::load(&dir);
        assert_eq!(loaded.sidebar_width, SIDEBAR_WIDTH);
        assert_eq!(loaded.right_panel.width, 420.0);
        assert!(!loaded.right_panel.queue_tab);
        fs::write(
            Settings::path(&dir),
            br#"{"sidebarWidth": 230, "rightPanel": {"width": 500}}"#,
        )
        .unwrap();
        let loaded = Settings::load(&dir);
        assert_eq!(
            (loaded.sidebar_width, loaded.right_panel.width),
            (230.0, 500.0)
        );

        fs::write(
            Settings::path(&dir),
            br#"{"window": {"width": 100, "height": 1e9}}"#,
        )
        .unwrap();
        let w = Settings::load(&dir).window;
        assert_eq!((w.width, w.height, w.maximized), (960.0, 7680.0, false));
        assert_eq!((w.x, w.y), (None, None));

        fs::write(
            Settings::path(&dir),
            br#"{"window": {"x": -32000, "y": -32000}}"#,
        )
        .unwrap();
        let w = Settings::load(&dir).window;
        assert_eq!((w.x, w.y), (None, None));

        fs::write(Settings::path(&dir), br#"{"window": {"x": 120}}"#).unwrap();
        assert_eq!(Settings::load(&dir).window.x, None);

        fs::write(Settings::path(&dir), b"nope").unwrap();
        assert_eq!(Settings::load(&dir), Settings::default());
        assert_eq!(fs::read(dir.join("settings.json.bak")).unwrap(), b"nope");

        // Once migrated (layoutVersion >= 1), explicit 210 / 360 widths are preserved.
        let custom = Settings {
            sidebar_width: OLD_SIDEBAR_WIDTH,
            right_panel: RightPanel {
                width: OLD_PANEL_WIDTH,
                ..RightPanel::default()
            },
            ..Settings::default()
        };
        custom.save(&dir).unwrap();
        let reloaded = Settings::load(&dir);
        assert_eq!(
            (reloaded.sidebar_width, reloaded.right_panel.width),
            (OLD_SIDEBAR_WIDTH, OLD_PANEL_WIDTH)
        );

        // ASIO driver CLSIDs and profile keys normalize case-insensitively without duplicates.
        fs::write(
            Settings::path(&dir),
            br#"{
                "asioDriver": " {d323b478-a475-b063-6a36-197faf1d0689} ",
                "asioProfiles": {
                    "{d323b478-a475-b063-6a36-197faf1d0689}": { "channelPair": [2, 3] }
                }
            }"#,
        )
        .unwrap();
        let mut mixed = Settings::load(&dir);
        assert_eq!(
            mixed.asio_driver.as_deref(),
            Some("{D323B478-A475-B063-6A36-197FAF1D0689}")
        );
        assert_eq!(
            mixed.asio_channel_pair("{D323B478-A475-B063-6A36-197FAF1D0689}"),
            (2, 3)
        );
        assert_eq!(
            mixed.asio_channel_pair("{d323b478-a475-b063-6a36-197faf1d0689}"),
            (2, 3)
        );
        mixed.set_asio_channel_pair("{d323b478-a475-b063-6a36-197faf1d0689}", 4, 5);
        assert_eq!(mixed.asio_profiles.len(), 1);
        assert_eq!(
            mixed.asio_channel_pair("{D323B478-A475-B063-6A36-197FAF1D0689}"),
            (4, 5)
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
