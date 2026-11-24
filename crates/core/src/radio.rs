//! Pure logic and state types for 032 Similar Track Radio and Queue Auto-Continue.
//!
//! Tracks in play-order range `[0, manual_queue_count)` belong to the user's
//! explicit queue; tracks in `[manual_queue_count, len)` are auto-appended
//! similar recommendations.

use std::collections::{HashMap, HashSet};

use api::{SimilarTrack, Track};
use serde::{Deserialize, Serialize};

use crate::controller::QueuePosition;
use crate::queue::Repeat;

/// Initial batch size when explicitly starting Track Radio.
pub const RADIO_INITIAL_BATCH_SIZE: usize = 15;
/// Batch size when auto-continuing at queue end or refreshing ("换一批").
pub const AUTO_CONTINUE_BATCH_SIZE: usize = 12;
/// Prefetch next batch when remaining tracks after current is at most this.
pub const AUTO_CONTINUE_THRESHOLD: usize = 2;
/// Maximum dismissed auto-similar track IDs remembered per session.
pub const MAX_DISMISSED_HISTORY: usize = 120;
/// Maximum cached similarity reason lists per session.
pub const MAX_CACHED_REASONS: usize = 300;

/// Seed track that initiated an explicit Track Radio session.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RadioSeed {
    pub track_id: i64,
    pub title: String,
}

/// Failure category for similar-track fetches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadioErrorKind {
    OfflineOrFailed,
    Busy,
    Empty,
}

/// Similarity scoring mode advertised by `capabilities.similarity.method`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SimilarityMethod {
    Hybrid,
    Metadata,
    #[default]
    Unknown,
}

impl SimilarityMethod {
    pub fn from_capability(method: Option<&str>) -> Self {
        match method.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("hybrid") => Self::Hybrid,
            Some("metadata") => Self::Metadata,
            _ => Self::Unknown,
        }
    }
}

/// Why auto-continue is currently paused even though the toggle is enabled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RadioPauseReason {
    #[default]
    None,
    RepeatAll,
    RepeatOne,
    SleepTrack,
    SleepAlbum,
}

/// UI-facing snapshot of Track Radio & Queue Auto-Continue state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RadioStatus {
    pub supported: bool,
    pub auto_play_enabled: bool,
    pub manual_queue_count: usize,
    pub radio_seed: Option<RadioSeed>,
    pub last_auto_seed_title: Option<String>,
    pub is_loading: bool,
    pub error_kind: Option<RadioErrorKind>,
    pub similarity_method: SimilarityMethod,
    pub pause_reason: RadioPauseReason,
}

/// Radio metadata persisted alongside `queue.json` (backward-compatible).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RadioSnapshot {
    /// `None` on legacy `queue.json` files: treat entire queue as manual.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manual_queue_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radio_seed: Option<RadioSeed>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_auto_seed_title: Option<String>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub similar_reasons: HashMap<i64, Vec<String>>,
}

fn is_structural_reason(reason: &str) -> bool {
    matches!(
        reason.trim(),
        "sameWork" | "sharedCredit" | "coPlaylist" | "sameArtist"
    )
}

fn has_structural_reason(reasons: &[String]) -> bool {
    reasons.iter().any(|r| is_structural_reason(r))
}

fn is_acoustic_only_reason(reasons: &[String]) -> bool {
    let mut has_acoustic = false;
    for reason in reasons {
        let trimmed = reason.trim();
        if is_structural_reason(trimmed) {
            return false;
        }
        if trimmed == "soundsSimilar" {
            has_acoustic = true;
        }
    }
    has_acoustic
}

/// Maps raw 032 similarity reason codes to the most informative short Chinese label.
/// Specific structural relationships (`sameWork`, `sharedCredit`, `coPlaylist`, `sameArtist`)
/// take precedence over `soundsSimilar` so hybrid matches do not collapse into "听感相似",
/// while pure acoustic matches still display "听感相似" ahead of broad `genre`/`era` tags.
pub fn primary_reason_label(reasons: &[String]) -> Option<&'static str> {
    if reasons.is_empty() {
        return None;
    }
    let has = |key: &str| reasons.iter().any(|r| r.trim() == key);
    if has("sameWork") {
        Some("同作品")
    } else if has("sharedCredit") {
        Some("同作者")
    } else if has("coPlaylist") {
        Some("共现歌单")
    } else if has("sameArtist") {
        Some("同艺人")
    } else if has("soundsSimilar") {
        Some("听感相似")
    } else if has("genre") {
        Some("同曲风")
    } else if has("era") {
        Some("同年代")
    } else {
        None
    }
}

/// Pure check for whether auto-continue should proactively prefetch a new batch.
#[allow(clippy::too_many_arguments)]
pub fn should_trigger_auto_continue(
    supported: bool,
    auto_play_enabled: bool,
    repeat: Repeat,
    sleep_blocking: bool,
    queue_len: usize,
    current_index: Option<usize>,
    is_loading: bool,
    remaining_threshold: usize,
) -> bool {
    if !supported || !auto_play_enabled || is_loading || sleep_blocking {
        return false;
    }
    if repeat != Repeat::Off {
        return false;
    }
    let Some(current) = current_index else {
        return false;
    };
    if queue_len == 0 || current >= queue_len {
        return false;
    }
    let remaining_after_current = queue_len - 1 - current;
    remaining_after_current <= remaining_threshold
}

fn fold_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    for ch in title.chars() {
        match ch {
            '\u{FF01}'..='\u{FF5E}' => {
                if let Some(c) = char::from_u32(u32::from(ch) - 0xFEE0) {
                    out.extend(c.to_lowercase());
                }
            }
            '\u{3000}' => out.push(' '),
            _ => out.extend(ch.to_lowercase()),
        }
    }
    out
}

fn is_sep(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '.' | '_' | '-' | '・')
}

fn is_inst_boundary(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '(' | ')'
                | '['
                | ']'
                | '【'
                | '】'
                | '「'
                | '」'
                | '-'
                | '\u{2013}'
                | '\u{2014}'
                | '~'
                | '～'
                | '.'
        )
}

fn contains_joined(haystack: &str, left: &str, rights: &[&str]) -> bool {
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(left) {
        let abs = start + pos;
        let mut rest = &haystack[abs + left.len()..];
        rest = rest.trim_start_matches(is_sep);
        if rights.iter().any(|r| rest.starts_with(r)) {
            return true;
        }
        start = abs + left.len();
    }
    false
}

fn contains_ascii_word(haystack: &str, word: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(word) {
        let abs = start + pos;
        let before_ok = haystack[..abs]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_');
        let after_ok = haystack[abs + word.len()..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_');
        if before_ok && after_ok {
            return true;
        }
        start = abs + word.len();
    }
    false
}

fn contains_bounded_inst(haystack: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = haystack[start..].find("inst") {
        let abs = start + pos;
        let before_ok = haystack[..abs]
            .chars()
            .next_back()
            .is_none_or(is_inst_boundary);
        if before_ok {
            let mut rest = &haystack[abs + 4..];
            if let Some(after_dot) = rest.strip_prefix('.') {
                rest = after_dot;
            }
            // Optional " ver." / " version" / " mix" / " edit" suffix.
            let trimmed = rest.trim_start_matches(char::is_whitespace);
            let after_opt = if trimmed.len() < rest.len() {
                ["version", "ver.", "ver", "mix", "edit"]
                    .into_iter()
                    .find_map(|kw| trimmed.strip_prefix(kw))
                    .unwrap_or(rest)
            } else {
                rest
            };
            if after_opt.chars().next().is_none_or(is_inst_boundary) {
                return true;
            }
        }
        start = abs + 4;
    }
    false
}

/// Returns true when a track's title looks like an accompaniment / off-vocal /
/// instrumental bonus track (common as the final track on J-Pop / ACG singles).
pub fn is_likely_accompaniment_title(title: &str) -> bool {
    if title.trim().is_empty() {
        return false;
    }
    let folded = fold_title(title);
    if [
        "karaoke",
        "カラオケ",
        "からおけ",
        "ボーカルなし",
        "ボーカルレス",
        "歌なし",
        "伴奏",
        "インスト",
        "纯音乐版",
        "純音楽版",
    ]
    .iter()
    .any(|kw| folded.contains(kw))
    {
        return true;
    }
    if contains_joined(&folded, "off", &["vocal", "vo"])
        || contains_joined(&folded, "オフ", &["ボーカル", "ヴォーカル"])
        || contains_joined(&folded, "backing", &["track"])
        || contains_joined(&folded, "minus", &["one"])
        || contains_joined(&folded, "less", &["vocal"])
        || contains_joined(&folded, "without", &["vocal"])
        || contains_joined(&folded, "no", &["vocal"])
        || contains_joined(&folded, "w/o", &["vocal"])
        || contains_joined(&folded, "vocal", &["less"])
    {
        return true;
    }
    contains_ascii_word(&folded, "instrumental")
        || contains_ascii_word(&folded, "instrumentals")
        || contains_bounded_inst(&folded)
}

/// Selects the next rolling seed track for auto-continue: prefers the latest
/// non-accompaniment track in the queue so singles ending with `(Off Vocal)`
/// or `(Instrumental)` still seed from the main vocal track.
pub fn select_rolling_seed(tracks: &[Track], current_index: Option<usize>) -> Option<&Track> {
    if tracks.is_empty() {
        return None;
    }
    if let Some(track) = tracks
        .iter()
        .rev()
        .find(|t| t.id > 0 && !is_likely_accompaniment_title(&t.title))
    {
        return Some(track);
    }
    if let Some(track) = tracks.iter().rev().find(|t| t.id > 0) {
        return Some(track);
    }
    current_index
        .and_then(|i| tracks.get(i))
        .filter(|t| t.id > 0)
}

fn artist_key_for(candidate: &SimilarTrack) -> String {
    candidate
        .track
        .artists
        .first()
        .filter(|a| a.id > 0)
        .map(|a| format!("id:{}", a.id))
        .or_else(|| {
            let name = candidate.track.artist.trim();
            (!name.is_empty()).then(|| format!("name:{}", name.to_lowercase()))
        })
        .unwrap_or_else(|| format!("unknown:{}", candidate.track.id))
}

/// Client-side deduplication, two-pass artist cap, and acoustic/structural
/// interleaving safeguard.
pub fn filter_and_diversify(
    candidates: Vec<SimilarTrack>,
    excluded_ids: &HashSet<i64>,
    max_per_artist: usize,
    limit: usize,
) -> Vec<SimilarTrack> {
    if candidates.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut seen: HashSet<i64> = excluded_ids.clone();
    let mut unique = Vec::new();
    for candidate in candidates {
        if candidate.track.id > 0 && seen.insert(candidate.track.id) {
            unique.push(candidate);
        }
    }
    if unique.is_empty() || max_per_artist == 0 {
        unique.truncate(limit);
        return unique;
    }

    let top_score = unique.iter().map(|c| c.score).fold(0.0_f64, f64::max);
    let min_structural_score = (top_score * 0.50).max(0.18);
    let artist_keys: Vec<String> = unique.iter().map(artist_key_for).collect();
    let mut slots: Vec<Option<SimilarTrack>> = unique.into_iter().map(Some).collect();
    let mut artist_counts: HashMap<String, usize> = HashMap::new();
    let mut picked = Vec::with_capacity(limit.min(slots.len()));
    let mut consecutive_acoustic_only = 0usize;
    let mut next_general = 0usize;
    let mut next_structural = 0usize;

    while picked.len() < limit {
        while next_general < slots.len() {
            if slots[next_general].is_some()
                && artist_counts
                    .get(&artist_keys[next_general])
                    .copied()
                    .unwrap_or(0)
                    < max_per_artist
            {
                break;
            }
            next_general += 1;
        }
        if next_general >= slots.len() {
            break;
        }

        let mut chosen = next_general;
        if consecutive_acoustic_only >= 2
            && slots[next_general]
                .as_ref()
                .is_some_and(|c| is_acoustic_only_reason(&c.reasons))
        {
            while next_structural < slots.len() {
                let eligible = slots[next_structural].as_ref().is_some_and(|c| {
                    artist_counts
                        .get(&artist_keys[next_structural])
                        .copied()
                        .unwrap_or(0)
                        < max_per_artist
                        && has_structural_reason(&c.reasons)
                        && c.score >= min_structural_score
                });
                if eligible {
                    chosen = next_structural;
                    break;
                }
                next_structural += 1;
            }
        }

        if let Some(candidate) = slots[chosen].take() {
            *artist_counts
                .entry(artist_keys[chosen].clone())
                .or_insert(0) += 1;
            if is_acoustic_only_reason(&candidate.reasons) {
                consecutive_acoustic_only += 1;
            } else {
                consecutive_acoustic_only = 0;
            }
            picked.push(candidate);
        }
    }

    for candidate in slots.into_iter().flatten() {
        if picked.len() >= limit {
            break;
        }
        picked.push(candidate);
    }
    picked
}

/// Keeps the boundary between manual `[0, manual_count)` and auto-continued
/// `[manual_count, queue_len)` tracks consistent after a single-item drag move.
pub fn remap_manual_count_after_move(
    manual_count: usize,
    from_index: usize,
    to_index: usize,
    queue_len: usize,
) -> usize {
    let clamped = manual_count.min(queue_len);
    if from_index == to_index {
        return clamped;
    }
    let from_manual = from_index < clamped;
    let to_manual = to_index < clamped;
    if !from_manual && to_manual {
        (clamped + 1).min(queue_len)
    } else if from_manual && !to_manual {
        clamped.saturating_sub(1)
    } else {
        clamped
    }
}

/// Keeps `manual_count` aligned after removing a track at `remove_index`.
pub fn remap_manual_count_after_remove(
    manual_count: usize,
    remove_index: usize,
    new_queue_len: usize,
) -> usize {
    let adjusted = if remove_index < manual_count {
        manual_count.saturating_sub(1)
    } else {
        manual_count
    };
    adjusted.min(new_queue_len)
}

/// Computes the play-order insertion index for a manual enqueue while
/// respecting the manual/auto-continue boundary: `Last` inserts at the end of
/// the manual queue (before auto-continued tracks).
pub fn manual_insert_index(
    at: QueuePosition,
    current_index: Option<usize>,
    queue_len: usize,
    manual_count: usize,
) -> usize {
    let boundary = manual_count.min(queue_len);
    let after_current = current_index.map_or(0, |c| c + 1);
    match at {
        QueuePosition::Next => after_current.min(queue_len),
        QueuePosition::Last => boundary.max(after_current).min(queue_len),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: i64, title: &str, artist: &str, artist_id: i64) -> Track {
        Track {
            id,
            title: title.into(),
            artist: artist.into(),
            artists: if artist_id > 0 {
                vec![api::ArtistRef {
                    id: artist_id,
                    name: artist.into(),
                    join_phrase: None,
                }]
            } else {
                Vec::new()
            },
            ..Track::default()
        }
    }

    fn sim_with(
        id: i64,
        artist: &str,
        artist_id: i64,
        score: f64,
        reasons: &[&str],
    ) -> SimilarTrack {
        SimilarTrack {
            track: track(id, &format!("t{id}"), artist, artist_id),
            distance: 1.0 - score,
            score,
            reasons: reasons.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn sim(id: i64, artist: &str, artist_id: i64) -> SimilarTrack {
        sim_with(id, artist, artist_id, 0.9, &["soundsSimilar"])
    }

    #[test]
    fn reason_label_follows_priority() {
        assert_eq!(
            primary_reason_label(&[
                "sameWork".into(),
                "sharedCredit".into(),
                "soundsSimilar".into()
            ]),
            Some("同作品")
        );
        assert_eq!(
            primary_reason_label(&[
                "sharedCredit".into(),
                "coPlaylist".into(),
                "soundsSimilar".into()
            ]),
            Some("同作者")
        );
        assert_eq!(
            primary_reason_label(&[
                "coPlaylist".into(),
                "sameArtist".into(),
                "soundsSimilar".into()
            ]),
            Some("共现歌单")
        );
        assert_eq!(
            primary_reason_label(&["sameArtist".into(), "soundsSimilar".into()]),
            Some("同艺人")
        );
        assert_eq!(
            primary_reason_label(&["genre".into(), "era".into(), "soundsSimilar".into()]),
            Some("听感相似")
        );
        assert_eq!(
            primary_reason_label(&["genre".into(), "era".into()]),
            Some("同曲风")
        );
        assert_eq!(primary_reason_label(&["era".into()]), Some("同年代"));
        assert_eq!(
            primary_reason_label(&["sameAlbum".into(), "oftenSkipped".into()]),
            None
        );
        assert_eq!(primary_reason_label(&["unknown".into()]), None);
    }

    #[test]
    fn accompaniment_detection_and_rolling_seed() {
        for title in [
            "群青 (Off Vocal)",
            "群青（オフボーカル）",
            "Song -Instrumental-",
            "Song (Inst.)",
            "Song [inst ver.]",
            "夜に駆ける (カラオケ)",
            "Theme (伴奏)",
            "Theme (Vocal Less)",
            "Theme (w/o vocal)",
            "Theme インストゥルメンタル",
        ] {
            assert!(
                is_likely_accompaniment_title(title),
                "should match: {title}"
            );
        }
        for title in ["Instant Crush", "Instinct", "Installation", "群青", "Voice"] {
            assert!(
                !is_likely_accompaniment_title(title),
                "should not match: {title}"
            );
        }

        let single = vec![
            track(1, "群青", "YOASOBI", 10),
            track(2, "優しい彗星", "YOASOBI", 10),
            track(3, "群青 (Off Vocal)", "YOASOBI", 10),
        ];
        assert_eq!(select_rolling_seed(&single, Some(2)).map(|t| t.id), Some(2));

        // All accompaniment: fall back to tail track.
        let all_inst = vec![
            track(4, "A (Instrumental)", "X", 1),
            track(5, "B (Off Vocal)", "X", 1),
        ];
        assert_eq!(
            select_rolling_seed(&all_inst, Some(1)).map(|t| t.id),
            Some(5)
        );
    }

    #[test]
    fn trigger_respects_repeat_sleep_and_threshold() {
        assert!(should_trigger_auto_continue(
            true,
            true,
            Repeat::Off,
            false,
            5,
            Some(2),
            false,
            2
        ));
        assert!(!should_trigger_auto_continue(
            true,
            true,
            Repeat::Off,
            false,
            5,
            Some(1),
            false,
            2
        ));
        assert!(!should_trigger_auto_continue(
            true,
            true,
            Repeat::All,
            false,
            5,
            Some(4),
            false,
            2
        ));
        assert!(!should_trigger_auto_continue(
            true,
            true,
            Repeat::One,
            false,
            5,
            Some(4),
            false,
            2
        ));
        assert!(!should_trigger_auto_continue(
            true,
            true,
            Repeat::Off,
            true,
            5,
            Some(4),
            false,
            2
        ));
        assert!(!should_trigger_auto_continue(
            false,
            true,
            Repeat::Off,
            false,
            5,
            Some(4),
            false,
            2
        ));
    }

    #[test]
    fn diversify_caps_artist_then_backfills() {
        let candidates = vec![
            sim(1, "A", 1),
            sim(2, "A", 1),
            sim(3, "A", 1),
            sim(4, "B", 2),
            sim(5, "C", 3),
        ];
        let excluded: HashSet<i64> = [1].into_iter().collect();
        let picked = filter_and_diversify(candidates, &excluded, 1, 4);
        // Pass 1 picks 2 (A), 4 (B), 5 (C); pass 2 backfills deferred 3 (A).
        let ids: Vec<i64> = picked.iter().map(|s| s.track.id).collect();
        assert_eq!(ids, [2, 4, 5, 3]);
    }

    #[test]
    fn diversify_interleaves_structural_tracks_among_acoustic_neighbors() {
        let candidates = vec![
            sim_with(1, "A", 1, 0.44, &["genre", "soundsSimilar"]),
            sim_with(2, "B", 2, 0.43, &["soundsSimilar"]),
            sim_with(3, "C", 3, 0.42, &["soundsSimilar"]),
            sim_with(4, "D", 4, 0.41, &["soundsSimilar"]),
            sim_with(5, "E", 5, 0.40, &["soundsSimilar"]),
            sim_with(6, "F", 6, 0.34, &["sameWork", "soundsSimilar"]),
            sim_with(7, "G", 7, 0.32, &["sharedCredit", "era"]),
            sim_with(8, "H", 8, 0.12, &["coPlaylist"]), // below quality floor -> stays behind
        ];
        let picked = filter_and_diversify(candidates, &HashSet::new(), 2, 6);
        let ids: Vec<i64> = picked.iter().map(|s| s.track.id).collect();
        assert_eq!(ids, [1, 2, 6, 3, 4, 7]);
    }

    #[test]
    fn boundary_remapping_on_move_remove_and_insert() {
        // Promote auto track at 4 into manual slot 1 (manual count 3 -> 4).
        assert_eq!(remap_manual_count_after_move(3, 4, 1, 6), 4);
        // Demote manual track at 1 into auto slot 4 (manual count 3 -> 2).
        assert_eq!(remap_manual_count_after_move(3, 1, 4, 6), 2);
        // Move within manual or within auto keeps count unchanged.
        assert_eq!(remap_manual_count_after_move(3, 0, 2, 6), 3);
        assert_eq!(remap_manual_count_after_move(3, 3, 5, 6), 3);

        // Removing manual item shrinks boundary; removing auto item keeps boundary.
        assert_eq!(remap_manual_count_after_remove(3, 1, 5), 2);
        assert_eq!(remap_manual_count_after_remove(3, 4, 5), 3);

        // Manual "Last" inserts at manual boundary (before auto tracks).
        assert_eq!(manual_insert_index(QueuePosition::Last, Some(1), 7, 3), 3);
        // If playing inside auto zone (index 4), "Last" inserts right after current.
        assert_eq!(manual_insert_index(QueuePosition::Last, Some(4), 7, 3), 5);
        // "Next" always inserts right after current.
        assert_eq!(manual_insert_index(QueuePosition::Next, Some(1), 7, 3), 2);
    }
}
