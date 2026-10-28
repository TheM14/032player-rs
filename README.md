# 032player-rs

032 Music Server 原生桌面客户端的 Rust 实验版。

目标：在不依赖 Electron / Web 技术栈的前提下，实现现有 Electron 客户端
[032musicplayer](../032musicplayer)（基于 Feishin）里 **032 服务端能用的全部功能**。
其他服务端（Jellyfin / Navidrome / Subsonic / Plex）不做。

状态：实验项目，Electron 版继续作为日常主力和后备，本仓库不影响它。

## 文档

| 文档 | 内容 |
|------|------|
| [docs/PLAN.md](docs/PLAN.md) | 为什么做、路线对比、已定决策、里程碑 |
| [docs/FEATURES.md](docs/FEATURES.md) | 功能对齐清单（以 Electron 版为准，带源码位置） |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | crate 划分、依赖选型、关键设计 |
| [AGENTS.md](AGENTS.md) | 给编码代理的约定和环境说明 |

## 许可

GPL-3.0（部分逻辑移植自 GPL-3.0 的 032musicplayer / Feishin）。
