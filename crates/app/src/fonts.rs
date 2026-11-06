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

use slint::fontique_011::fontique;

static INTER: &[u8] = include_bytes!("../assets/fonts/Inter.ttf");
static NOTO_SANS_JP: &[u8] = include_bytes!("../assets/fonts/NotoSansJP.ttf");
static NOTO_SANS_SC: &[u8] = include_bytes!("../assets/fonts/NotoSansSC.ttf");

pub const JP: &str = "Noto Sans JP";
pub const SC: &str = "Noto Sans SC";

/// Uses Inter, Noto Sans JP, then Noto Sans SC as the generic font stack.
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
    for generic in [
        fontique::GenericFamily::SansSerif,
        fontique::GenericFamily::SystemUi,
        fontique::GenericFamily::UiSansSerif,
    ] {
        collection.set_generic_families(generic, families.iter().copied());
    }
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
}
