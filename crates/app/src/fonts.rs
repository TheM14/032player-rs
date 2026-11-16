//! Bundled fonts, as in the Electron client: Inter for Latin text, Noto
//! Sans JP for Japanese and Noto Sans SC for Chinese (`use-app-theme.ts`
//! font stacks). Without them each glyph falls back to whatever system font
//! covers it, so one line can mix two or three fonts.
//!
//! The CJK files have Inter's code points removed
//! (`tools/fonts/strip-inter-coverage.py`), so a Text with
//! `font-family: "Noto Sans SC"` gets Latin from Inter and every Han glyph
//! from SC, like the CSS stack `Inter, "Noto Sans SC"`. The UI is Chinese,
//! so the window defaults to SC; content text picks its family with
//! `Fonts.family(text)` ([`family_for`]).

use std::sync::OnceLock;

use slint::fontique_011::fontique;
use ttf_parser::Face;

static INTER: &[u8] = include_bytes!("../assets/fonts/Inter.ttf");
static NOTO_SANS_JP: &[u8] = include_bytes!("../assets/fonts/NotoSansJP.ttf");
static NOTO_SANS_SC: &[u8] = include_bytes!("../assets/fonts/NotoSansSC.ttf");
static SOURCE_SERIF: &[u8] = include_bytes!("../assets/fonts/SourceSerif4.ttf");

pub const JP: &str = "Noto Sans JP";
pub const SC: &str = "Noto Sans SC";
/// Latin display face for the artist-page title (Source Serif 4).
pub const SERIF: &str = "Source Serif 4";

/// Uses Inter, Noto Sans JP, then Noto Sans SC as the generic font stack.
/// Source Serif 4 is registered for [`display_family`] only; it is not part
/// of the generic stack, so a missing glyph still falls back to Inter / Noto.
/// Call once the platform is set up, before the first window is created.
pub fn register() {
    let mut collection = slint::fontique_011::shared_collection();
    let mut families = Vec::new();
    for data in [INTER, NOTO_SANS_JP, NOTO_SANS_SC] {
        let fonts = collection.register_fonts(fontique::Blob::new(std::sync::Arc::new(data)), None);
        for (family, _) in fonts {
            if !families.contains(&family) {
                families.push(family);
            }
        }
    }
    // Addressed by name from `display_family`. Left out of the generic stack
    // so a missing glyph still falls back to Inter, then Noto.
    collection.register_fonts(fontique::Blob::new(std::sync::Arc::new(SOURCE_SERIF)), None);
    for generic in [
        fontique::GenericFamily::SansSerif,
        fontique::GenericFamily::SystemUi,
        fontique::GenericFamily::UiSansSerif,
    ] {
        collection.set_generic_families(generic, families.iter().copied());
    }
}

/// Artist-page display face: Source Serif 4 for a Latin name, otherwise the
/// same Noto family as [`family_for`]. A CJK name must not use the serif
/// family, because missing glyphs fall through the generic stack's JP face
/// before SC.
pub fn display_family(text: &str) -> &'static str {
    if has_cjk(text) {
        family_for(text)
    } else {
        SERIF
    }
}

/// Han or kana (the katakana middle dot `・` also appears in Chinese names).
fn has_cjk(text: &str) -> bool {
    text.chars().any(|c| {
        c != '\u{30FB}'
            && matches!(
                c,
                '\u{3040}'..='\u{30FF}'
                    | '\u{31F0}'..='\u{31FF}'
                    | '\u{3400}'..='\u{9FFF}'
                    | '\u{F900}'..='\u{FAFF}'
                    | '\u{FF66}'..='\u{FF9D}'
            )
    })
}

/// Family for a piece of content text, as Electron `detectCjkLang`: kana
/// means Japanese (JP glyph forms); anything else is treated as Chinese, so
/// a simplified line never mixes JP glyphs with SC fallbacks.
pub fn family_for(text: &str) -> &'static str {
    // U+30FB `・` also separates names in Chinese text.
    let kana = text.chars().any(|c| {
        c != '\u{30FB}'
            && matches!(c,
            '\u{3041}'..='\u{309F}' // Hiragana
            | '\u{30A0}'..='\u{30FF}' // Katakana
            | '\u{31F0}'..='\u{31FF}' // Katakana phonetic extensions
            | '\u{FF66}'..='\u{FF9D}') // Halfwidth katakana
    });
    if kana { JP } else { SC }
}

struct FontMetrics {
    inter: Face<'static>,
    jp: Face<'static>,
    sc: Face<'static>,
}

fn metrics() -> &'static FontMetrics {
    static METRICS: OnceLock<FontMetrics> = OnceLock::new();
    METRICS.get_or_init(|| FontMetrics {
        inter: Face::parse(INTER, 0).expect("bundled Inter font parses"),
        jp: Face::parse(NOTO_SANS_JP, 0).expect("bundled Noto Sans JP font parses"),
        sc: Face::parse(NOTO_SANS_SC, 0).expect("bundled Noto Sans SC font parses"),
    })
}

fn char_advance(face: &Face<'_>, ch: char) -> Option<f32> {
    let gid = face.glyph_index(ch)?;
    let adv = face.glyph_hor_advance(gid)?;
    let upem = face.units_per_em().max(1) as f32;
    Some(adv as f32 / upem)
}

/// Measures single-line width (logical px) using the bundled font stack.
pub fn text_width(text: &str, font_size: f32, font_weight: i32) -> f32 {
    if text.is_empty() || font_size <= 0.0 {
        return 0.0;
    }
    let m = metrics();
    let is_jp = family_for(text) == JP;
    let (primary, secondary) = if is_jp {
        (&m.jp, &m.sc)
    } else {
        (&m.sc, &m.jp)
    };
    let mut em_sum = 0.0_f32;
    for ch in text.chars() {
        let adv = char_advance(primary, ch)
            .or_else(|| char_advance(&m.inter, ch))
            .or_else(|| char_advance(secondary, ch))
            .unwrap_or(1.0);
        em_sum += adv;
    }
    let weight_scale = if font_weight >= 600 { 1.05 } else { 1.02 };
    em_sum * font_size * weight_scale
}

/// Total logical width for all artist links joined with `" | "`.
pub fn artists_full_width<'a, I>(names: I, font_size: f32, font_weight: i32) -> f32
where
    I: IntoIterator<Item = &'a str>,
{
    let sep_w = text_width(" | ", font_size, 400);
    let mut total = 0.0_f32;
    let mut count = 0usize;
    for name in names {
        if count > 0 {
            total += sep_w;
        }
        total += text_width(name, font_size, font_weight);
        count += 1;
    }
    if count == 0 { 0.0 } else { total + 2.0 }
}

/// Width of the `+N` overflow chip for `hidden` collapsed artists.
pub fn artist_chip_width(hidden: usize, font_size: f32) -> f32 {
    if hidden == 0 || font_size <= 0.0 {
        return 0.0;
    }
    let chip_font_size = (font_size * 0.82).max(11.0);
    let chip_pad_x = (font_size * 0.9).max(12.0);
    text_width(&format!("+{hidden}"), chip_font_size, 600) + chip_pad_x
}

/// Returns how many leading artists fit inside `avail_width` (at least 1 when
/// non-empty) while leaving room for a `+N` overflow chip when truncated.
pub fn fit_artist_count<'a, I>(
    names: I,
    avail_width: f32,
    font_size: f32,
    font_weight: i32,
) -> usize
where
    I: IntoIterator<Item = &'a str>,
{
    let widths: Vec<f32> = names
        .into_iter()
        .map(|s| text_width(s, font_size, font_weight))
        .collect();
    let n = widths.len();
    if n == 0 {
        return 0;
    }
    if avail_width <= 0.0 {
        return 1;
    }
    let sep_w = text_width(" | ", font_size, 400);
    let total: f32 = widths.iter().sum::<f32>() + sep_w * (n - 1) as f32;
    if total + 2.0 <= avail_width {
        return n;
    }

    let chip_gap = (font_size * 0.45).max(6.0);

    let mut prefix = 0.0_f32;
    let mut best = 1usize;
    for (i, w) in widths.iter().enumerate().take(n - 1) {
        if i > 0 {
            prefix += sep_w;
        }
        prefix += *w;
        let k = i + 1;
        let rem = n - k;
        let chip_w = artist_chip_width(rem, font_size);
        if prefix + chip_gap + chip_w + 2.0 <= avail_width {
            best = k;
        } else {
            break;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kana_selects_japanese() {
        assert_eq!(family_for("全てを均す聖者の行進"), JP);
        assert_eq!(family_for("ナイト・オブ・ナイツ"), JP);
        assert_eq!(family_for("将一切抚平的圣者行进"), SC);
        assert_eq!(family_for("東方紅魔郷"), SC);
        assert_eq!(family_for("Bad Apple!!"), SC);
        assert_eq!(family_for("列奥纳多・达・芬奇"), SC);
        assert_eq!(family_for(""), SC);
    }

    #[test]
    fn display_face_is_serif_only_for_latin() {
        assert_eq!(display_family("Keina Suda"), SERIF);
        assert_eq!(display_family("YOASOBI"), SERIF);
        assert_eq!(display_family(""), SERIF);
        assert_eq!(display_family("須田景凪"), SC);
        assert_eq!(display_family("ヨアソビ"), JP);
        assert_eq!(display_family("将一切抚平的圣者行进"), SC);
    }

    #[test]
    fn fit_artist_count_handles_many_and_short_lists() {
        let nine = [
            "愛城華恋",
            "神楽ひかり",
            "天堂真矢",
            "星見純那",
            "露崎まひる",
            "大場なな",
            "西條クロディーヌ",
            "石動双葉",
            "花柳香子",
        ];
        assert_eq!(fit_artist_count(Vec::<&str>::new(), 200.0, 14.0, 400), 0);
        // Wide album artist column (~300px) fits several full names + "+N".
        let fit_300 = fit_artist_count(nine, 300.0, 14.0, 400);
        assert!((3..=5).contains(&fit_300), "fit_300={fit_300}");
        // Very narrow width still keeps at least the first artist.
        assert_eq!(fit_artist_count(nine, 40.0, 14.0, 400), 1);
        // 4 artists in 300px should all fit without collapsing.
        let four = ["星見純那", "佐藤日向", "愛城華恋", "小山百代"];
        assert_eq!(fit_artist_count(four, 300.0, 14.0, 400), 4);
    }
}
