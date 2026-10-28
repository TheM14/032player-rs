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
    pub show_lyrics: bool,
    pub repeat: Repeat,
    pub shuffle: bool,
    /// Closing the window hides it to the tray (Windows).
    pub close_to_tray: bool,
    /// Stream a transcode instead of the original file (`None`: original,
    /// the bit-perfect default). Ignored when the server cannot.
    pub transcode: Option<api::Transcode>,
    pub lyrics: LyricsSettings,
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
    /// Kana readings over kanji of Japanese lines.
    pub furigana: bool,
    /// A romaji line under Japanese lines.
    pub romaji: bool,
}

impl Default for LyricsSettings {
    fn default() -> Self {
        Self {
            translation_language: "zh-Hans".into(),
            translation_mode: TranslationMode::Bilingual,
            auto_translate: false,
            furigana: false,
            romaji: false,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 100.0,
            audio_device: None,
            exclusive: false,
            show_lyrics: true,
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
            show_lyrics: false,
            repeat: Repeat::One,
            shuffle: true,
            close_to_tray: true,
            transcode: Some(api::Transcode::Mp3(320)),
            lyrics: LyricsSettings {
                translation_language: "en".into(),
                translation_mode: TranslationMode::Translated,
                auto_translate: true,
                furigana: true,
                romaji: false,
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
        assert!(loaded.show_lyrics);

        fs::write(Settings::path(&dir), b"nope").unwrap();
        assert_eq!(Settings::load(&dir), Settings::default());
        let _ = fs::remove_dir_all(&dir);
    }
}
