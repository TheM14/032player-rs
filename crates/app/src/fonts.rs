//! Bundled fonts, as in the Electron client: Inter for Latin text and
//! Noto Sans JP for kana / kanji (`use-app-theme.ts` font stack). Without
//! them each glyph falls back to whatever system font covers it, so one
//! Japanese line can mix two or three fonts.

use slint::fontique_011::fontique;

static INTER: &[u8] = include_bytes!("../assets/fonts/Inter.ttf");
static NOTO_SANS_JP: &[u8] = include_bytes!("../assets/fonts/NotoSansJP.ttf");

/// Makes Inter, then Noto Sans JP, the default (sans-serif) font stack.
/// Call once the platform is set up, before the first window is created.
pub fn register() {
    let mut collection = slint::fontique_011::shared_collection();
    let mut families = Vec::new();
    for data in [INTER, NOTO_SANS_JP] {
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
