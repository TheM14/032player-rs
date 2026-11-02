# 字体

| 文件 | 来源 | 许可 |
|------|------|------|
| `Inter.ttf` | Electron 版 `assets/fonts/Inter-VariableFont_opsz,wght.woff2` 转 TTF | SIL OFL 1.1（The Inter Project Authors） |
| `NotoSansJP.ttf` | Electron 版 `assets/fonts/NotoSansJP-VariableFont_wght.woff2` 转 TTF | SIL OFL 1.1（Google） |
| `NotoSansSC.ttf` | [Google Fonts](https://github.com/google/fonts/blob/main/ofl/notosanssc/NotoSansSC%5Bwght%5D.ttf)，可变字重 100-900 | SIL OFL 1.1，见 `OFL-NotoSansSC.txt` |

默认内容字体栈为 Inter → Noto Sans JP → Noto Sans SC，保留日文内容的日文字形。
中文区块标题显式指定 Noto Sans SC，避免 JP 缺少简体字导致同一标题混用系统字体。
单纯把 SC 放在 JP 后面只能补缺字，不能统一中文标题的字形。

转换：`fontTools`，`TTFont(woff2).flavor = None; save(ttf)`。Slint 读不了 WOFF2。
