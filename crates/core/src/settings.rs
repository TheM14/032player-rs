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
    pub repeat: Repeat,
    pub shuffle: bool,
    /// Closing the window hides it to the tray (Windows).
    pub close_to_tray: bool,
    /// Stream a transcode instead of the original file (`None`: original,
    /// the bit-perfect default). Ignored when the server cannot.
    pub transcode: Option<api::Transcode>,
    pub lyrics: LyricsSettings,
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
            repeat: Repeat::Off,
            shuffle: false,
            close_to_tray: false,
            transcode: None,
            lyrics: LyricsSettings::default(),
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
            repeat: Repeat::One,
            shuffle: true,
            close_to_tray: true,
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

        fs::write(Settings::path(&dir), b"nope").unwrap();
        assert_eq!(Settings::load(&dir), Settings::default());
        let _ = fs::remove_dir_all(&dir);
    }
}
