//! Lyrics display helpers. The server already resolves and parses lyrics into
//! timed lines (`GET /tracks/{id}/lyrics`); this crate maps a playback
//! position to the active line. Word-level cues, translation merging and
//! furigana / romaji come in M3.

/// One timed lyrics line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub time_ms: i64,
    pub text: String,
}

/// Synced lyrics, lines sorted by time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncedLyrics {
    lines: Vec<Line>,
}

impl SyncedLyrics {
    /// Sorts by time (stable, so lines sharing a timestamp keep their order).
    pub fn new(mut lines: Vec<Line>) -> Self {
        lines.sort_by_key(|l| l.time_ms);
        Self { lines }
    }

    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Index of the line active at `position_ms` shifted by `offset_ms`
    /// (positive = lyrics show later). `None` before the first line.
    pub fn active_index(&self, position_ms: i64, offset_ms: i64) -> Option<usize> {
        let at = position_ms.saturating_sub(offset_ms);
        self.lines
            .partition_point(|l| l.time_ms <= at)
            .checked_sub(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lyrics() -> SyncedLyrics {
        SyncedLyrics::new(vec![
            Line {
                time_ms: 5_000,
                text: "二行目".into(),
            },
            Line {
                time_ms: 1_000,
                text: "一行目".into(),
            },
            Line {
                time_ms: 5_000,
                text: "(翻訳)".into(),
            },
            Line {
                time_ms: 9_000,
                text: "最后一行".into(),
            },
        ])
    }

    #[test]
    fn active_line_follows_the_position() {
        let l = lyrics();
        assert_eq!(l.lines()[0].text, "一行目");
        assert_eq!(l.active_index(0, 0), None);
        assert_eq!(l.active_index(999, 0), None);
        assert_eq!(l.active_index(1_000, 0), Some(0));
        assert_eq!(l.active_index(4_999, 0), Some(0));
        // Same timestamp: the last of the group is active.
        assert_eq!(l.active_index(5_000, 0), Some(2));
        assert_eq!(l.active_index(60_000, 0), Some(3));
    }

    #[test]
    fn offset_shifts_the_lyrics() {
        let l = lyrics();
        assert_eq!(l.active_index(1_500, 1_000), None);
        assert_eq!(l.active_index(500, -500), Some(0));
    }

    #[test]
    fn empty_lyrics_have_no_active_line() {
        assert_eq!(SyncedLyrics::default().active_index(1_000, 0), None);
    }
}
