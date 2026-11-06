"""Remove every code point Inter covers from the bundled CJK fonts.

Slint takes a single `font-family` and falls back to the generic stack
(Inter, Noto Sans JP, Noto Sans SC). With the CJK fonts unable to map
Latin text, `font-family: "Noto Sans SC"` behaves like the Electron CSS
stack `Inter, "Noto Sans SC"`: Latin from Inter, Han from SC.

Idempotent: run from the repo root after replacing a font file.
    python3 tools/fonts/strip-inter-coverage.py
"""

from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont

FONTS = Path(__file__).resolve().parents[2] / "crates/app/assets/fonts"

inter = set(TTFont(FONTS / "Inter.ttf").getBestCmap())

for name in ["NotoSansJP.ttf", "NotoSansSC.ttf"]:
    path = FONTS / name
    font = TTFont(path)
    cmap = set(font.getBestCmap())
    keep = cmap - inter
    if keep == cmap:
        print(f"{name}: nothing to strip")
        continue
    options = subset.Options()
    options.layout_features = ["*"]
    options.name_IDs = ["*"]
    options.name_languages = ["*"]
    options.notdef_outline = True
    options.glyph_names = True
    options.hinting = True
    options.legacy_kern = True
    subsetter = subset.Subsetter(options)
    subsetter.populate(unicodes=keep)
    subsetter.subset(font)
    font.save(path)
    print(f"{name}: removed {len(cmap - keep)} code points, kept {len(keep)}")
