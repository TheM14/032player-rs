# 字体

| 文件 | 来源 | 许可 |
|------|------|------|
| `Inter.ttf` | Electron 版 `assets/fonts/Inter-VariableFont_opsz,wght.woff2` 转 TTF | SIL OFL 1.1（The Inter Project Authors） |
| `NotoSansJP.ttf` | Electron 版 `assets/fonts/NotoSansJP-VariableFont_wght.woff2` 转 TTF | SIL OFL 1.1（Google） |
| `NotoSansSC.ttf` | [Google Fonts](https://github.com/google/fonts/blob/main/ofl/notosanssc/NotoSansSC%5Bwght%5D.ttf)，可变字重 100-900 | SIL OFL 1.1，见 `OFL-NotoSansSC.txt` |

`NotoSansJP.ttf` / `NotoSansSC.ttf` 已删掉 Inter 覆盖的全部码位（`tools/fonts/strip-inter-coverage.py`，可重复执行）。
Slint 的 `font-family` 只能写一个字体，之后回落到通用字体栈 Inter → Noto Sans JP → Noto Sans SC；
CJK 字体不含拉丁字符后，`font-family: "Noto Sans SC"` 等价于 Electron 的 CSS 栈 `Inter, "Noto Sans SC"`。

- 窗口默认 `default-font-family: "Noto Sans SC"`：界面文字是中文，统一用 SC 字形。
- 内容文字（标题、艺人、歌词、翻译等绑定文本）写 `font-family: Fonts.family(self.text)`，
  由 `src/fonts.rs` 的 `family_for` 判断：含假名用 Noto Sans JP，否则用 Noto Sans SC（同 Electron `detectCjkLang`）。
- 只靠字体栈回落不行：JP 缺少部分简体字（如“词”“进”），同一行会混用 JP 和 SC 两种字形。

转换：`fontTools`，`TTFont(woff2).flavor = None; save(ttf)`。Slint 读不了 WOFF2。
