# Tasks

## 1. API 协议类型与设置持久化

- [x] 1.1 在 `crates/api/src/types.rs` 新增 `Feature::Loudness`、`LoudnessCapabilities`（`available`、`analyzed_tracks`、`total_tracks`、`library_median_lufs`）以及 `Track` 的 5 个可选响度字段（`loudness_lufs`、`true_peak_db`、`loudness_range`、`album_loudness_lufs`、`album_true_peak_db`），并补齐兼容新旧服务端 JSON 与 `0.0` 真峰值的反序列化测试（`cargo test -p api`）。
- [x] 1.2 在 `crates/core/src/settings.rs` 新增 `LoudnessSettings`、`LoudnessTarget`（含默认 `LibraryMedian` 跟随曲库中位数、固定 `Loud` -11 / `Standard` -14 / `Dynamic` -18 LUFS）、`LoudnessMode`（默认 `Smart`）与 `PeakProtection`（默认 `SoftLimiter` 3.5 dB、`StrongLimiter` 6.0 dB、`StrictLinear` 0 dB），接入 `Settings` 持久化并补齐旧配置迁移与往返序列化单元测试（`cargo test -p client-core settings`）。

## 2. 核心响度规划模块（`client-core`）

- [x] 2.1 新增 `crates/core/src/loudness.rs`，实现纯函数 `compute_loudness_plan`（涵盖跟随曲库中位数与固定三档目标、Smart 队列邻接专辑/单曲判定、余量感知增益合并、`3.5 dB` 柔和限幅 / `6.0 dB` 强力限幅 / `0 dB` 纯线性防削波、基于曲库中位数的保守回退），并补齐覆盖热曲库中位数、现代热歌、老歌、轻 OST、大动态古典、低软件音量余量、缺失数据与 Smart 边界的单元测试（`cargo test -p client-core loudness`）。
- [x] 2.2 扩展 `crates/core/src/engine.rs`（`Engine` trait 新增响度计划更新及加载/预加载参数传递，提供默认空实现保持既有假引擎兼容），并在 `crates/core/src/queue.rs` 与 `crates/core/src/controller/` 接入：切歌/预加载同步应用响度计划、播放中切换设置或跨越音量限幅阈值实时更新、与 300 ms 暂停淡化零冲突、Sonos 投放时自动不应用，以及队列曲目响度刷新与旧 `queue.json` 恢复按需补全，补齐 `client-core` 控制器回归测试（`cargo test -p client-core`）。
- [x] 2.3 扩展 `crates/core/src/browse.rs` 的曲目信息项与 `Core::emit_track_info`，为含响度数据的曲目附加集成响度、真峰值、专辑响度及当前生效补偿/限幅状态，并补齐单元测试验证格式化输出（`cargo test -p client-core`）。

## 3. 双音频后端执行（`player` + `HybridEngine`）

- [x] 3.1 新增 `crates/player/src/limiter.rs`，实现纯 Rust、零堆分配的立体声链接 5 ms Lookahead 软拐点真峰值限幅器（`SoftPeakLimiter`，支持任意采样率初始化、`-1.0 dBTP` 天花板与 50 ms 平滑释放），并补齐正弦波未超限比特直通、瞬态尖峰限幅不超顶及立体声像一致性的单元测试（`cargo test -p player limiter`）。
- [x] 3.2 改造 `crates/player/src/asio_player.rs`：在 `SharedState` 与 `make_render_fn` 中接入当前曲目与预加载下一曲的响度参数、在 `next_boundary_frame` 处原子帧精确切换并 64 帧平滑斜坡过渡、响度开启时接入预分配 `SoftPeakLimiter`、关闭时完全旁路直通，并补齐假声卡测试验证无缝切歌边界切换、实时开关切换与限幅不削波（`cargo test -p player asio`）。
- [x] 3.3 改造 `crates/player/src/lib.rs`（`libmpv`）：实现响度 `af` 滤镜构造（空滤镜直通 / 纯线性 `volume` / `volume + alimiter(..., level=disabled)`），保持 `volume-gain` 专用于暂停淡化，支持 `loadfile` 每曲 `af` 参数与旧 libmpv 回退、实时 `set_loudness` 更新，并在 `crates/app/src/engine.rs` 的 `HybridEngine` 透传到双后端，通过滤镜字符串单元测试与 `cargo check -p player`（含 Windows target check 若工具链已装）验证。

## 4. 设置页 UI、文档与全工作区验证

- [x] 4.1 更新 `crates/app/ui/settings.slint`、`crates/app/src/ui/wire.rs`、`crates/app/src/ui/events.rs` 与 `crates/app/src/ui.rs`：在设置页「播放」分区新增响度均衡开关（带 Sonos 边界与服务端能力提示）及响度目标（附曲库中位数说明）、对齐方式、峰值保护下拉框，同步更新 `tools/ui-snapshot` 假数据并渲染检查设置页与查看信息弹窗截图（`cargo run --manifest-path tools/ui-snapshot/Cargo.toml -- /tmp`）。
- [x] 4.2 更新项目文档 `docs/FEATURES.md`、`docs/PLAN.md` 与 `docs/TASKS.md`（记录响度均衡功能及待集中执行的 **[Win]** 外置 DAC / WASAPI 独占 / ASIO 实机验证清单），并运行全工作区检查确认全部通过：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace --exclude player --exclude app --exclude app-ui` 及 `cargo test -p player`。
