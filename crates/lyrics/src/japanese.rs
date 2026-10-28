//! Furigana and romaji for Japanese lyrics lines, like the Electron
//! client's `furigana.ts` (kuroshiro + kuromoji): lines without kana are
//! left alone (Chinese shares the kanji), kanji words get hiragana
//! readings without their okurigana, romaji is spaced per word.
//!
//! Segmentation uses lindera with the IPADIC dictionary embedded in the
//! binary (the same dictionary kuromoji ships).

use std::sync::OnceLock;

use lindera::dictionary::load_dictionary;
use lindera::mode::Mode;
use lindera::segmenter::Segmenter;
use wana_kana::ConvertJapanese;

/// A piece of a line: `reading` (hiragana) goes over `base`; empty for
/// text without kanji.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ruby {
    pub base: String,
    pub reading: String,
}

fn is_kana(c: char) -> bool {
    matches!(c, '\u{3041}'..='\u{309f}' | '\u{30a0}'..='\u{30ff}' | '\u{31f0}'..='\u{31ff}')
}

fn is_kanji(c: char) -> bool {
    matches!(c, '\u{4e00}'..='\u{9fff}' | '\u{3400}'..='\u{4dbf}' | '々' | '〆' | 'ヶ')
}

/// The line has kana, so it is Japanese (kuroshiro `hasKana`).
pub fn has_kana(text: &str) -> bool {
    text.chars().any(is_kana)
}

fn segmenter() -> Option<&'static Segmenter> {
    static SEGMENTER: OnceLock<Option<Segmenter>> = OnceLock::new();
    SEGMENTER
        .get_or_init(|| {
            load_dictionary("embedded://ipadic")
                .map(|d| Segmenter::new(Mode::Normal, d, None))
                .ok()
        })
        .as_ref()
}

/// Words of `text` with their katakana reading (IPADIC field 7; `None`
/// for unknown words).
fn words(text: &str) -> Vec<(String, Option<String>)> {
    let Some(segmenter) = segmenter() else {
        return vec![(text.to_owned(), None)];
    };
    let Ok(mut tokens) = segmenter.segment(text.into()) else {
        return vec![(text.to_owned(), None)];
    };
    tokens
        .iter_mut()
        .map(|t| {
            let surface = t.surface.as_ref().to_owned();
            let reading = t
                .details()
                .get(7)
                .filter(|r| **r != "*" && !r.is_empty())
                .map(|r| (*r).to_owned());
            (surface, reading)
        })
        .collect()
}

fn hira(c: char) -> char {
    c.to_string().to_hiragana().chars().next().unwrap_or(c)
}

/// Splits a kanji word's reading off its leading / trailing kana
/// ("沈む" + "しずむ" -> "沈" (しず) + "む").
fn ruby_word(surface: &str, reading: &str, out: &mut Vec<Ruby>) {
    let base: Vec<char> = surface.chars().collect();
    let read: Vec<char> = reading.to_hiragana().chars().collect();
    let (mut start, mut end) = (0, base.len());
    let (mut rstart, mut rend) = (0, read.len());
    while start < end && rstart < rend && is_kana(base[start]) && hira(base[start]) == read[rstart]
    {
        start += 1;
        rstart += 1;
    }
    while end > start
        && rend > rstart
        && is_kana(base[end - 1])
        && hira(base[end - 1]) == read[rend - 1]
    {
        end -= 1;
        rend -= 1;
    }
    let plain = |chars: &[char]| chars.iter().collect::<String>();
    push(out, &plain(&base[..start]), "");
    if start < end {
        let reading = plain(&read[rstart..rend]);
        push(out, &plain(&base[start..end]), &reading);
    }
    push(out, &plain(&base[end..]), "");
}

fn push(out: &mut Vec<Ruby>, base: &str, reading: &str) {
    if base.is_empty() {
        return;
    }
    if reading.is_empty()
        && let Some(last) = out.last_mut().filter(|r| r.reading.is_empty())
    {
        last.base.push_str(base);
        return;
    }
    out.push(Ruby {
        base: base.to_owned(),
        reading: reading.to_owned(),
    });
}

/// Ruby segments of a line; `None` when the line is not Japanese or has
/// no kanji to annotate.
pub fn furigana(text: &str) -> Option<Vec<Ruby>> {
    if !has_kana(text) || !text.chars().any(is_kanji) {
        return None;
    }
    let mut out = Vec::new();
    for (surface, reading) in words(text) {
        match reading {
            Some(r) if surface.chars().any(is_kanji) => ruby_word(&surface, &r, &mut out),
            _ => push(&mut out, &surface, ""),
        }
    }
    out.iter().any(|r| !r.reading.is_empty()).then_some(out)
}

/// Spaced romaji of a Japanese line (empty when it has no kana).
pub fn romaji(text: &str) -> String {
    if !has_kana(text) {
        return String::new();
    }
    let mut parts: Vec<String> = Vec::new();
    for (surface, reading) in words(text) {
        let japanese = surface.chars().any(|c| is_kana(c) || is_kanji(c));
        let part = match reading {
            Some(r) if japanese => r.to_romaji(),
            _ if japanese => surface.to_romaji(),
            _ => surface,
        };
        let part = part.trim().to_owned();
        if !part.is_empty() {
            parts.push(part);
        }
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(base: &str, reading: &str) -> Ruby {
        Ruby {
            base: base.into(),
            reading: reading.into(),
        }
    }

    #[test]
    fn readings_skip_okurigana() {
        assert_eq!(
            furigana("日が沈み出した").unwrap(),
            [
                r("日", "ひ"),
                r("が", ""),
                r("沈", "しず"),
                r("み", ""),
                r("出", "だ"),
                r("した", ""),
            ]
        );
    }

    #[test]
    fn chinese_and_kana_only_lines_are_left_alone() {
        assert_eq!(furigana("我的歌声里"), None);
        assert_eq!(furigana("さよなら"), None);
        assert_eq!(romaji("我的歌声里"), "");
    }

    #[test]
    fn romaji_is_spaced_per_word() {
        assert_eq!(romaji("君の姿"), "kimi no sugata");
        assert_eq!(romaji("Hello 世界だよ"), "Hello sekai da yo");
    }
}
