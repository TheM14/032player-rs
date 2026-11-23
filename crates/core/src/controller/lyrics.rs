//! Lyrics of the current track: loading, the active line (with a session
//! offset per track), server translation, and online search / pick / clear
//! (`features/lyrics/lyrics.tsx`, `lyrics-search-form.tsx`).

use std::collections::HashMap;

use api::{Feature, LyricsSearchResult};

use super::{Core, Event, Internal, LyricRow, LyricsResultItem, LyricsTools, Target};
use crate::settings::{LyricsSettings, TranslationMode};

/// `{ "error": { "code": "translation_disabled" } }`: switched off on the server.
const TRANSLATION_DISABLED: &str = "translation_disabled";

#[derive(Default)]
pub(super) struct LyricsState {
    /// Track the lyrics below belong to (0: none).
    track_id: i64,
    /// Lines sorted by time: (time, text, index in the server's lines).
    lines: Vec<(i64, String, usize)>,
    synced: bool,
    source: Option<String>,
    timeline: Option<lyrics::SyncedLyrics>,
    /// One entry per server line.
    translation: Option<Vec<String>>,
    show_translation: bool,
    translating: bool,
    /// Automatic translation of these lyrics failed: no retry.
    auto_failed: bool,
    /// The server answered `translation_disabled` this session.
    translation_off: bool,
    /// Session offsets per track (positive: lyrics earlier).
    offsets: HashMap<i64, i64>,
    results: Vec<LyricsSearchResult>,
    pub(super) active: Option<usize>,
}

impl Core {
    fn lyrics_offset(&self) -> i64 {
        let state = &self.lyr;
        state.offsets.get(&state.track_id).copied().unwrap_or(0)
    }

    /// Clears session-wide lyrics preferences (offsets, server translation flag)
    /// when signing out so switching servers starts fresh.
    pub(super) fn reset_lyrics_session(&mut self) {
        self.lyr.offsets.clear();
        self.lyr.translation_off = false;
        self.clear_lyrics();
    }

    /// Hides the lyrics (no current track, signed out).
    pub(super) fn clear_lyrics(&mut self) {
        self.latest.remove(&Target::Lyrics);
        self.latest.remove(&Target::LyricsTranslation);
        self.latest.remove(&Target::LyricsResults);
        self.latest.remove(&Target::LyricsPreview);
        let offsets = std::mem::take(&mut self.lyr.offsets);
        let translation_off = self.lyr.translation_off;
        self.lyr = LyricsState {
            offsets,
            translation_off,
            ..LyricsState::default()
        };
        self.emit(Event::Lyrics {
            lines: Vec::new(),
            synced: false,
            source: None,
        });
        self.emit(Event::LyricsActive(None));
        self.emit(Event::LyricsSearchResults(Vec::new()));
        self.emit(Event::LyricsPreview(Vec::new()));
        self.emit_lyrics_tools();
    }

    /// Clears the lyrics and fetches those of `track_id`.
    pub(super) fn load_lyrics(&mut self, track_id: i64) {
        self.clear_lyrics();
        let Some((client, tx)) = self.request() else {
            return;
        };
        self.lyr.track_id = track_id;
        let seq = self.next_seq(Target::Lyrics);
        self.emit_lyrics_tools();
        tokio::spawn(async move {
            let lyrics = match client.lyrics(track_id).await {
                Ok(lyrics) => lyrics,
                Err(e) => {
                    tracing::debug!("lyrics {track_id}: {e}");
                    if let Some(msg) = super::unauthorized(&client, &e) {
                        let _ = tx.send(msg);
                    }
                    None
                }
            };
            let _ = tx.send(Internal::Lyrics {
                seq,
                track_id,
                lyrics,
            });
        });
    }

    pub(super) fn lyrics_loaded(&mut self, seq: u64, track_id: i64, lyrics: Option<api::Lyrics>) {
        if self.latest.get(&Target::Lyrics) != Some(&seq)
            || self.lyr.track_id != track_id
            || self.queue.current().map(|i| i.track.id) != Some(track_id)
        {
            return;
        }
        self.latest.remove(&Target::LyricsTranslation);
        self.lyr.translating = false;
        self.lyr.translation = None;
        self.lyr.show_translation = false;
        let Some(lyrics) = lyrics.filter(|l| !l.lines.is_empty()) else {
            self.lyr.timeline = None;
            self.lyr.lines.clear();
            self.lyr.synced = false;
            self.lyr.source = None;
            self.lyr.active = None;
            self.emit_lyrics();
            self.emit(Event::LyricsActive(None));
            self.emit_lyrics_tools();
            return;
        };
        let mut lines: Vec<(i64, String, usize)> = lyrics
            .lines
            .into_iter()
            .enumerate()
            .map(|(i, l)| (l.time_ms, l.text, i))
            .collect();
        if lyrics.synced {
            lines.sort_by_key(|l| l.0);
            self.lyr.timeline = Some(lyrics::SyncedLyrics::new(
                lines
                    .iter()
                    .map(|(time_ms, text, _)| lyrics::Line {
                        time_ms: *time_ms,
                        text: text.clone(),
                    })
                    .collect(),
            ));
        } else {
            self.lyr.timeline = None;
        }
        self.lyr.lines = lines;
        self.lyr.synced = lyrics.synced;
        self.lyr.source = lyrics.source_name.or(lyrics.source);
        self.emit_lyrics();
        self.lyr.active = None;
        self.update_active_line();
        self.emit_lyrics_tools();
        if self.settings.lyrics.auto_translate && self.can_translate() && !self.lyr.auto_failed {
            self.translate(true, false);
        }
    }

    /// Rows as shown: the translation under each line (bilingual) or
    /// instead of it (translated only).
    fn emit_lyrics(&mut self) {
        let settings = &self.settings.lyrics;
        let state = &self.lyr;
        let translation = state
            .translation
            .as_ref()
            .filter(|_| state.show_translation);
        let mode = settings.translation_mode;
        let lines = state
            .lines
            .iter()
            .map(|(_, text, i)| {
                let translated = translation
                    .and_then(|t| t.get(*i))
                    .map(|t| t.trim())
                    .filter(|t| !t.is_empty());
                match (translated, mode) {
                    (Some(t), TranslationMode::Translated) => LyricRow {
                        text: t.to_owned(),
                        ..LyricRow::default()
                    },
                    (Some(t), TranslationMode::Bilingual) if t != text.trim() => LyricRow {
                        text: text.clone(),
                        translation: t.to_owned(),
                    },
                    _ => LyricRow {
                        text: text.clone(),
                        translation: String::new(),
                    },
                }
            })
            .collect();
        self.emit(Event::Lyrics {
            lines,
            synced: state.synced,
            source: state.source.clone(),
        });
    }

    fn can_translate(&self) -> bool {
        !self.lyr.translation_off
            && self
                .library
                .as_ref()
                .is_some_and(|l| l.capabilities.has(Feature::LyricsTranslation))
    }

    pub(super) fn emit_lyrics_tools(&self) {
        let has_lyrics = !self.lyr.lines.is_empty();
        let can_search = self
            .library
            .as_ref()
            .is_some_and(|l| l.capabilities.has(Feature::LyricsOnline))
            && self.lyr.track_id != 0;
        self.emit(Event::LyricsTools(LyricsTools {
            has_lyrics,
            can_search,
            can_translate: has_lyrics && self.can_translate(),
            translation_shown: self.lyr.show_translation && self.lyr.translation.is_some(),
            translating: self.lyr.translating,
            offset_ms: self.lyrics_offset(),
        }));
    }

    pub(super) fn update_active_line(&mut self) {
        // `active_index` takes "positive = later"; the offset is "positive = earlier".
        let offset = -self.lyrics_offset();
        let active = self
            .lyr
            .timeline
            .as_ref()
            .and_then(|l| l.active_index(self.position_ms, offset));
        if active != self.lyr.active {
            self.lyr.active = active;
            self.emit(Event::LyricsActive(active));
        }
    }

    // ------------------------------------------------------------ translation

    pub(super) fn toggle_translation(&mut self) {
        if self.lyr.translation.is_some() {
            self.lyr.show_translation = !self.lyr.show_translation;
            self.emit_lyrics();
            self.emit_lyrics_tools();
            return;
        }
        self.translate(false, false);
    }

    fn translate(&mut self, auto: bool, force: bool) {
        if (!force && self.lyr.translating) || self.lyr.lines.is_empty() || !self.can_translate() {
            return;
        }
        let Some((client, tx)) = self.request() else {
            return;
        };
        let track_id = self.lyr.track_id;
        let seq = self.next_seq(Target::LyricsTranslation);
        let api_token = client.api_token().to_owned();
        let language = self.settings.lyrics.translation_language.clone();
        self.lyr.translating = true;
        self.lyr.auto_failed = auto;
        self.emit_lyrics_tools();
        tokio::spawn(async move {
            let lines = client
                .translate_lyrics(track_id, &language)
                .await
                .map(|t| t.lines);
            let _ = tx.send(Internal::Translation {
                seq,
                api_token,
                track_id,
                lines,
            });
        });
    }

    pub(super) fn translation_loaded(
        &mut self,
        seq: u64,
        api_token: String,
        track_id: i64,
        lines: Result<Vec<String>, api::ApiError>,
    ) {
        if let Err(ref e) = lines
            && e.is_unauthorized()
        {
            if self.latest.get(&Target::LyricsTranslation) == Some(&seq)
                && self.lyr.track_id == track_id
            {
                self.lyr.translating = false;
                self.emit_lyrics_tools();
            }
            self.internal(Internal::Unauthorized { api_token });
            return;
        }
        if self.latest.get(&Target::LyricsTranslation) != Some(&seq)
            || self.lyr.track_id != track_id
            || !self.lyr.translating
        {
            return;
        }
        self.lyr.translating = false;
        match lines {
            Ok(lines) => {
                self.lyr.auto_failed = false;
                self.lyr.translation = Some(lines);
                self.lyr.show_translation = true;
                self.emit_lyrics();
            }
            Err(e) => {
                if e.code() == Some(TRANSLATION_DISABLED) {
                    self.lyr.translation_off = true;
                }
                self.emit(Event::Notice(format!("翻译失败：{e}")));
            }
        }
        self.emit_lyrics_tools();
    }

    // ------------------------------------------------------------ offset, seek

    pub(super) fn set_lyrics_offset(&mut self, offset_ms: i64) {
        if self.lyr.track_id == 0 {
            return;
        }
        self.lyr.offsets.insert(self.lyr.track_id, offset_ms);
        self.update_active_line();
        self.emit_lyrics_tools();
    }

    /// Seeks to the start of a synced line (shifted by the offset).
    pub(super) fn seek_to_lyric(&mut self, index: usize) {
        if !self.lyr.synced {
            return;
        }
        let Some(time_ms) = self.lyr.lines.get(index).map(|l| l.0) else {
            return;
        };
        self.seek((time_ms - self.lyrics_offset()).max(0));
    }

    pub(super) fn set_lyrics_settings(&mut self, lyrics: LyricsSettings) {
        let language_changed =
            lyrics.translation_language != self.settings.lyrics.translation_language;
        self.update_settings(|s| s.lyrics = lyrics);
        if language_changed {
            self.lyr.auto_failed = false;
            if self.lyr.translation.is_some() || self.lyr.translating {
                // Shown or in-flight in the old language: translate again with a new seq.
                self.lyr.translation = None;
                self.lyr.show_translation = false;
                self.translate(false, true);
            } else if self.settings.lyrics.auto_translate {
                self.translate(true, false);
            }
        } else if self.settings.lyrics.auto_translate
            && self.lyr.translation.is_none()
            && !self.lyr.auto_failed
        {
            self.translate(true, false);
        }
        self.emit_lyrics();
        self.emit_lyrics_tools();
    }

    // ------------------------------------------------------------ search

    pub(super) fn search_lyrics(&mut self, name: String, artist: String) {
        let track_id = self.lyr.track_id;
        if track_id == 0 {
            return;
        }
        let seq = self.next_seq(Target::LyricsResults);
        if name.trim().is_empty() && artist.trim().is_empty() {
            self.lyr.results.clear();
            self.emit(Event::LyricsSearchResults(Vec::new()));
            return;
        }
        let duration = self
            .queue
            .current()
            .map(|i| i.track.duration_millis)
            .filter(|d| *d > 0);
        let Some((client, tx)) = self.request() else {
            return;
        };
        tokio::spawn(async move {
            match client.search_lyrics(&name, &artist, duration).await {
                Ok(results) => {
                    let _ = tx.send(Internal::LyricsResults {
                        seq,
                        track_id,
                        results,
                    });
                }
                Err(e) => {
                    let _ = tx.send(Internal::LyricsResults {
                        seq,
                        track_id,
                        results: Vec::new(),
                    });
                    let _ = tx.send(Self::failed(&client, "搜索歌词", e));
                }
            }
        });
    }

    pub(super) fn lyrics_results(
        &mut self,
        seq: u64,
        track_id: i64,
        mut results: Vec<LyricsSearchResult>,
    ) {
        if self.latest.get(&Target::LyricsResults) != Some(&seq) || self.lyr.track_id != track_id {
            return;
        }
        results.sort_by(|a, b| a.score.total_cmp(&b.score));
        let items = results.iter().map(result_item).collect();
        self.lyr.results = results;
        self.emit(Event::LyricsSearchResults(items));
    }

    pub(super) fn preview_lyrics(&mut self, index: usize) {
        let Some(pick) = self.lyr.results.get(index).cloned() else {
            return;
        };
        let Some((client, tx)) = self.request() else {
            return;
        };
        let seq = self.next_seq(Target::LyricsPreview);
        self.emit(Event::LyricsPreview(Vec::new()));
        tokio::spawn(async move {
            match client.remote_lyrics(&pick.source, &pick.id).await {
                Ok(lyrics) => {
                    let _ = tx.send(Internal::LyricsPreview(
                        seq,
                        lyrics
                            .map(|l| l.lines.into_iter().map(|l| l.text).collect())
                            .unwrap_or_default(),
                    ));
                }
                Err(e) => {
                    let _ = tx.send(Internal::LyricsPreview(
                        seq,
                        vec!["（预览失败）".to_owned()],
                    ));
                    let _ = tx.send(Self::failed(&client, "预览歌词", e));
                }
            }
        });
    }

    pub(super) fn lyrics_preview(&mut self, seq: u64, lines: Vec<String>) {
        if self.latest.get(&Target::LyricsPreview) == Some(&seq) {
            let lines = if lines.is_empty() {
                vec!["（没有歌词）".to_owned()]
            } else {
                lines
            };
            self.emit(Event::LyricsPreview(lines));
        }
    }

    pub(super) fn apply_lyrics(&mut self, index: usize) {
        let track_id = self.lyr.track_id;
        let Some(pick) = self.lyr.results.get(index).cloned() else {
            return;
        };
        let Some((client, tx)) = self.request() else {
            return;
        };
        let generation = self.list_generation;
        tokio::spawn(async move {
            let msg = match client.set_lyrics_source(track_id, &pick).await {
                Ok(()) => Internal::LyricsChanged {
                    generation,
                    track_id,
                },
                Err(e) => Self::failed(&client, "更新歌词", e),
            };
            let _ = tx.send(msg);
        });
    }

    pub(super) fn clear_lyrics_source(&mut self) {
        let track_id = self.lyr.track_id;
        if track_id == 0 {
            return;
        }
        let Some((client, tx)) = self.request() else {
            return;
        };
        let generation = self.list_generation;
        tokio::spawn(async move {
            let msg = match client.clear_lyrics_source(track_id).await {
                Ok(()) => Internal::LyricsChanged {
                    generation,
                    track_id,
                },
                Err(e) => Self::failed(&client, "清除歌词", e),
            };
            let _ = tx.send(msg);
        });
    }

    /// The lyrics changed on the server: the translation belongs to the
    /// old ones.
    pub(super) fn lyrics_changed(&mut self, track_id: i64) {
        if self.lyr.track_id == track_id {
            self.load_lyrics(track_id);
        }
    }
}

fn result_item(r: &LyricsSearchResult) -> LyricsResultItem {
    let source = match r.source.as_str() {
        "lrclib" => "LRCLIB",
        "netease" => "NetEase",
        other => other,
    };
    let synced = match r.synced {
        Some(true) => "同步",
        Some(false) => "非同步",
        None => "未知",
    };
    LyricsResultItem {
        title: r.name.clone(),
        artist: r.artist.clone(),
        meta: [source, r.id.as_str(), synced].join(" - "),
        score: format!("{:.2}%", (1.0 - r.score) * 100.0),
    }
}
