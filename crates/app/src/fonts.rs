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
}
