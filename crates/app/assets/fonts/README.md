# 字体

| 文件 | 来源 | 许可 |
|------|------|------|
| `Inter.ttf` | Electron 版 `assets/fonts/Inter-VariableFont_opsz,wght.woff2` 转 TTF | SIL OFL 1.1（The Inter Project Authors） |
| `NotoSansJP.ttf` | Electron 版 `assets/fonts/NotoSansJP-VariableFont_wght.woff2` 转 TTF | SIL OFL 1.1（Google） |

转换：`fontTools`，`TTFont(woff2).flavor = None; save(ttf)`。Slint 读不了 WOFF2。
