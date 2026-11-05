//! User settings in `settings.json`. Missing or corrupt files and fields
//! fall back to defaults.

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
    pub right_panel: RightPanel,
    /// Left sidebar width in logical px (dragged by its right edge).
    pub sidebar_width: f32,
    /// Main window size when it was last closed.
    pub window: WindowState,
    pub repeat: Repeat,
    pub shuffle: bool,
    /// Closing the window hides it to the tray (Windows).
    pub close_to_tray: bool,
    /// Stream a transcode instead of the original file (`None`: original,
    /// the bit-perfect default). Ignored when the server cannot.
    pub transcode: Option<api::Transcode>,
    pub lyrics: LyricsSettings,
    pub shortcuts: crate::shortcuts::Shortcuts,
}

/// The right panel: queue and lyrics stacked (Electron `sidebar-play-queue`:
/// `rightExpanded`, `showQueueInSidebar`, `showLyricsInSidebar`,
/// `sidebarPanelOrder`, the split and the width).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RightPanel {
    /// The player bar's queue button toggles the whole panel.
    pub open: bool,
    pub queue: bool,
    pub lyrics: bool,
    /// Lyrics above the queue.
    pub lyrics_first: bool,
    /// Share of the height taken by the upper pane when both show.
    pub split: f32,
    /// Width in logical px.
    pub width: f32,
}

impl Default for RightPanel {
    fn default() -> Self {
        Self {
            open: true,
            queue: true,
            lyrics: true,
            lyrics_first: false,
            split: 0.5,
            width: 360.0,
        }
    }
}

impl RightPanel {
    fn sanitize(&mut self) {
        let d = Self::default();
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
pub const SIDEBAR_WIDTH: f32 = 210.0;

/// Sidebar width limits (Electron allows dragging between these).
pub fn sidebar_width(width: f32) -> f32 {
    if width.is_finite() {
        width.clamp(180.0, 400.0)
    } else {
        SIDEBAR_WIDTH
    }
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
            right_panel: RightPanel::default(),
            sidebar_width: SIDEBAR_WIDTH,
            window: WindowState::default(),
            repeat: Repeat::Off,
            shuffle: false,
            close_to_tray: false,
            transcode: None,
            lyrics: LyricsSettings::default(),
            shortcuts: crate::shortcuts::Shortcuts::default(),
        }
    }
}

impl Settings {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("settings.json")
    }

    /// Reads `settings.json` in `data_dir`; defaults when missing or corrupt.
    pub fn load(data_dir: &Path) -> Self {
        let mut settings: Self = fs::read(Self::path(data_dir))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if !settings.volume.is_finite() {
            settings.volume = 100.0;
        }
        settings.volume = settings.volume.clamp(0.0, 100.0);
        settings.right_panel.sanitize();
        settings.sidebar_width = sidebar_width(settings.sidebar_width);
        settings.window.sanitize();
        settings.audio_device = settings
            .audio_device
            .filter(|d| !d.is_empty() && d != "auto");
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
            right_panel: RightPanel {
                open: false,
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
            shortcuts: crate::shortcuts::Shortcuts::default(),
            transcode: Some(api::Transcode::Mp3(320)),
            lyrics: LyricsSettings {
                translation_language: "en".into(),
                translation_mode: TranslationMode::Translated,
                auto_translate: true,
            },
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
        let _ = fs::remove_dir_all(&dir);
    }
}
