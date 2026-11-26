# Proposal

## Why

不同年代与风格的曲目在 032 曲库中的响度落差很大，但市面上常见的响度均衡（如固定 -18 LUFS 的 ReplayGain 或纯线性防削波）往往会在开启后让现代流行曲骤降 6-12 dB、轻歌又因怕削波拉不上来，尤其在「软件音量 100% + 外置 DAC 旋钮」的听音场景下产生明显的"声音突然变小、切去其他应用炸耳"的糟糕体感。本项目需要一套既不破坏曲内动态、又能避免"整体变小"体感的客户端响度均衡，统一覆盖 `libmpv`（共享 / WASAPI 独占）与纯 Rust `AsioPlayer` 两条音频管线。

## What Changes

- 扩展 `api` crate：解析服务端 `GET /api/v1/capabilities` 的 `features.loudness` 与 `loudness` 统计对象（含 `libraryMedianLufs`），以及 `Track` 的可选响度物理字段（`loudnessLufs`、`truePeakDb`、`loudnessRange`、`albumLoudnessLufs`、`albumTruePeakDb`）。
- 在 `client-core` 新增无 IO 的响度均衡规划模块：
  - 支持多档参考基准，默认使用贴近现代流行/ACG 曲库的 **饱满（-11 LUFS）**，并提供 **标准（-14 LUFS）** 与 **动态优先（-18 LUFS）**；
  - 支持 **智能（顺序连播同专辑按专辑响度，随机/混播/自动续播按单曲响度）/ 按单曲 / 按专辑** 三种对齐方式；
  - 采用 **余量感知增益 + 有界瞬态软限幅** 策略（默认柔和限幅上限 3.5 dB，天花板 -1.0 dBTP；可选严格纯线性防削波零限幅），在 100% 软件音量外接 DAC 场景下保住响度与动态平衡；
  - 为后台尚未分析完的曲目提供基于曲库中位数的保守只降不升回退策略。
- 扩展 `Engine` trait 与双播放后端（`player::Player` / `player::AsioPlayer` / `HybridEngine`）：
  - `libmpv`：将响度增益与受控 `alimiter`（显式 `level=disabled`）放入 `af` 滤镜链，与独占 `volume-gain` 的暂停淡入淡出彻底解耦，关闭时还原 `af=""` 保持直通，并处理好无缝切歌与实时设置切换。
  - `AsioPlayer`：在 `make_render_fn` 音频回调内实现零分配的 5 ms Lookahead 软拐点真峰值限幅器与 `next_boundary_frame` 帧精确切歌增益切换，确保设置页实时切换 0 延迟生效。
  - Sonos 投放保持明确边界，不受本机响度均衡影响。
- 扩展设置持久化与 Slint UI：设置页「播放」分区新增响度均衡开关、目标基准（展示曲库中位数参考）、对齐方式、峰值保护选项；曲目「查看信息」弹窗补充展示响度物理量与当前生效补偿，方便用户对照试听体验。

## Capabilities

### New Capabilities

- `loudness-normalization`: 基于服务端 EBU R128 物理量的客户端响度均衡规划、高基准与有界软限幅听感优化、双后端（`libmpv` / `AsioPlayer`）实时及无缝执行、设置持久化与曲目信息诊断展示。

### Modified Capabilities

<!-- 无既有 spec 需求变更 -->

## Impact

- `crates/api`：`types.rs` 新增 `Feature::Loudness`、`LoudnessCapabilities` 及 `Track` 响度字段（均带 `Option` 默认兼容旧服务端）。
- `crates/core`：新增 `loudness.rs` 纯逻辑模块与单元测试；更新 `settings.rs`、`engine.rs`、`controller.rs`、`controller/playback.rs`（队列切歌、预加载、实时设置更新、曲目信息展示及队列元数据刷新）。
- `crates/player`：`lib.rs`（`libmpv` 滤镜与每曲应用）、`asio_player.rs`（实时回调合并增益与预分配 Lookahead 软限幅器）及新增纯 Rust 限幅器模块与单测。
- `crates/app`：`engine.rs`（`HybridEngine` 透传）、`ui/settings.slint`、`ui/wire.rs`、`ui/events.rs`、`tools/ui-snapshot`（同步假数据与截图回归）。
- 依赖：纯 Rust 实现核心数学与限幅器，无新增重型原生库依赖；无界面 crate 仍可在无 libmpv/无显示环境下通过 `cargo test`。
