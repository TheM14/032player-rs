# Design

## Context

参见 `proposal.md` 了解需求动机，参见 `specs/loudness-normalization/spec.md` 了解对外行为规范。当前 `032player-rs` 的关键架构约束如下：

- 依赖边界：`core`（`client-core`）不依赖 `player`、`asio` 或 `slint`，通过 `Engine` trait（`crates/core/src/engine.rs`）驱动音频后端，保证无界面 crate 可在无 libmpv 环境下运行 `cargo test`。
- 双本机后端：
  - `player::Player`（`crates/player/src/lib.rs`）封装 `libmpv`（Windows 捆绑 shinchiro `20261018` 构建，mpv 0.40+ / FFmpeg 7+），服务共享输出与 WASAPI 独占；目前 `set_fade_gain` 独占 `volume-gain` 属性用于播放/暂停 300 ms 淡化。
  - `player::AsioPlayer`（`crates/player/src/asio_player.rs`）纯 Rust 链路 `reqwest -> symphonia -> SpscRing (最长 ~11.8s @ 44.1kHz) -> make_render_fn -> asio`，支持帧精确同采样率无缝切换（`next_boundary_frame`）。
- 投放边界：Sonos 投放（`crates/core/src/controller/cast.rs`）由音箱直连服务端 HTTP 流，客户端不接触音频数据。
- 目标核心使用画像：软件音量 100% + 外置 DAC 硬件旋钮（WASAPI 独占或 ASIO 独占）。

## Goals / Non-Goals

**Goals:**

- 在 `client-core` 建立纯函数、可完整单元测试的响度规划引擎（`LoudnessPlan`），保证所有数学公式与边界条件脱离 UI 与 libmpv 即可验证。
- `libmpv` 与 `AsioPlayer` 两条后端对同一份 `LoudnessPlan` 呈现一致的听感、天花板保护、实时响应与无缝切歌行为。
- 针对「软件 100% + 外置 DAC」优先解决"一开就变小"和"大动态被压扁"两大痛点，同时兼容软件音量 < 100% 的余量感知。

**Non-Goals:**

- 不在客户端内置离线 EBU R128 整库扫描器或本地文件标签解析器（数据权威来自 `032music-server`）。
- 不实现实时动态自动增益（AGC / `dynaudnorm`），避免破坏单曲内部动态起伏与产生抽吸感。
- 不干预 Sonos 投放音箱的音量或音频流。
- 本期不提供逐曲自定义 dB 偏移或全局 Preamp 自由滑块（先验证三档目标与两档峰值保护的实际听感，按 YAGNI 原则后续需要再扩展）。

## Decisions

### 决策 1：统一增益与有界限幅规划公式（`crates/core/src/loudness.rs`）

定义强类型设置结构持久化到 `settings.json`（`loudness` 字段，全部带 `#[serde(default)]` 兼容旧配置）：

```rust
pub struct LoudnessSettings {
    pub enabled: bool,              // default: false
    pub target: LoudnessTarget,     // Loud(-11.0, default) | Standard(-14.0) | Dynamic(-18.0)
    pub mode: LoudnessMode,         // Smart(default) | Track | Album
    pub peak_mode: PeakProtection,  // SoftLimiter(default, max_gr=3.5 dB) | StrictLinear(0.0 dB)
}
```

每曲计算的纯数据产物 `LoudnessPlan`：

```rust
pub struct LoudnessPlan {
    pub gain_db: f64,           // applied linear gain before limiter
    pub ceiling_db: f64,        // -1.0 dBTP
    pub use_limiter: bool,      // true only when SoftLimiter and peak after gain+vol exceeds ceiling
    pub source: LoudnessSource, // Track | Album | Fallback | Bypass
    pub peak_reduction_db: f64, // estimated max transient GR (for TrackInfo display)
}
```

核心公式：
- `ceiling_db = -1.0`（为外置 DAC 升频滤波器留出 1 dB 采样间峰值余量）
- `user_vol_db`：音量 `v` 在 `0..=100` 下遵循立方曲线 `20 * log10((v / 100)^3)`（`v = 100` 时为 `0.0 dB`；`v <= 0` 时无需限幅）
- `raw_gain = target_lufs - measured_lufs`
- `linear_headroom = (ceiling_db - true_peak_db) - user_vol_db`
- `max_gr = match peak_mode { SoftLimiter => 3.5, StrictLinear => 0.0 }`
- `applied_gain = raw_gain.min(linear_headroom + max_gr).clamp(-24.0, 18.0)`
- `peak_after_all = true_peak_db + applied_gain + user_vol_db`
- `use_limiter = peak_mode == SoftLimiter && peak_after_all > ceiling_db + 0.05`
- 缺失测量值时：`fallback = library_median_lufs.map_or(0.0, |m| (target_lufs - m).clamp(-6.0, 0.0))`，`use_limiter = false`。

**理由**：单套统一公式同时覆盖 100% 外置 DAC 场景与软件低音量场景；将 `SoftLimiter` 上限锁定在 `3.5 dB` 既能在 `-11 LUFS` 档位把绝大多数流行/老歌完整对齐，又避免把 `-22 LUFS` 交响高潮暴力压毁。

### 决策 2：Smart 智能对齐判定采用无状态队列邻接规则

在 `client-core` 给定队列位置 `pos` 时，判定使用专辑响度的充要条件：
1. `!queue.shuffle()`
2. `pos < radio.manual_queue_count`（不在相似自动续播区）
3. `track.album_id > 0` 且 `pos - 1` 或 `pos + 1`（同在手动区内）的曲目具有相同 `album_id`
4. `track.album_loudness_lufs` 与 `track.album_true_peak_db` 均存在

**理由**：比记录"播放入口来源"更健壮，能天然兼容队列拖拽、插入下一首、切换随机、`queue.json` 重启恢复，且 `sync_preload` 预计算下一曲计划时只需传入 `pos + 1` 即可得出一致结果。

### 决策 3：`libmpv` 管线将响度增益与受控 `alimiter` 放入 `af`，与 `volume-gain` 淡入淡出彻底解耦

mpv 音频管线顺序为：`decoder -> af -> softvol (volume + volume-gain) -> AO`。

- **选择**：
  - `fade_gain` 继续独占 `volume-gain`，代码零冲突。
  - 响度均衡关闭或 `gain_db.abs() < 0.05 && !use_limiter` 时：设置 `af = ""`（零滤镜直通）。
  - 仅纯线性增益（`!use_limiter`）：设置 `af = lavfi=[volume=<gain>dB:precision=float]`。
  - 需要软限幅（`use_limiter`）：设置 `af = lavfi=[volume=<gain>dB:precision=float,alimiter=limit=0.89125:attack=5:release=50:asc=0:level=disabled]`。
    - **关键细节**：`alimiter` 必须显式传 `level=disabled`，否则 FFmpeg 默认开启输入自动归一化，会把轻歌错误推满到 0 dBFS！
    - `limit=0.89125` 对应 `-1.0 dB` 天花板；`attack=5` 开启 5 ms lookahead；`release=50` 为 50 ms 平滑恢复；`asc=0` 关闭自动慢释防抽吸。
- **无缝切歌与实时切换**：
  - 当前曲目加载与预加载下一曲时，将目标 `af` 字符串通过 `Engine::load_for` / `Engine::append_for`（或携带 `LoudnessPlan` 的重载）传给后端。
  - 对 `libmpv`：优先尝试通过 mpv `loadfile <url> <mode> -1 af=<escaped>` 传递 per-file `af`（兼容新版本 libmpv 帧边界生效）；若遇到旧版系统 libmpv 不接受 4 参数 `loadfile`（返回错误），回退为 2 参数 `loadfile` + 在 `FileLoaded` 时设置全局 `af`。
  - 用户在播放中修改设置或改变音量跨过限幅阈值时，直接对当前 mpv 实例更新 `af` 并触发 `sync_preload()` 刷新下一曲。
- **替代方案**：把 `norm_gain` 叠加进 `volume-gain` 而 `alimiter` 放在 `af` - **不可行**，因为 `af` 在 `volume-gain` 之前执行，正增益会绕过限幅器导致削波。

### 决策 4：`AsioPlayer` 在 `make_render_fn` 实时回调中执行零分配增益与 5 ms Lookahead 软限幅

`AsioPlayer` 的 `SpscRing` 缓存最长 11.8 秒 PCM，不能把增益烘焙在解码线程（否则切换设置延迟数秒）。

- **选择**：在 `crates/player/src/limiter.rs` 实现纯 Rust、零堆分配、实时安全的立体声 Lookahead 软拐点峰值限幅器，由 `make_render_fn` 持有（在 `start_fresh_sink` / 采样率变更时按 `sample_rate` 预分配一次 5 ms 环形延迟线与单调队列索引缓冲，音频回调内零分配、O(1) 均摊复杂度）：
  - **信号流程**（每帧立体声 `(L, R)`）：
    1. 检测到 `consumed_frames >= next_boundary_frame` 时，在回调内部将 `active_norm` 原子切换为 `next_norm`（通过 `SharedState` 中的原子打包字段传递 `gain_linear` 与 `use_limiter` / `ceiling`），并用 64 帧线性斜坡平滑过渡，无咔嗒声。
    2. 计算合成目标增益 `total_amp = user_vol_amp * fade_amp * norm_gain_linear`，按立体声帧平滑步进并乘到 `(L, R)`。
    3. 限幅器处理：
       - 当响度均衡**完全关闭**时：完全旁路延迟线，零额外延迟、bit-identical 直通。
       - 当响度均衡**开启**且 `use_limiter == true` 时：经过 5 ms Lookahead 延迟线，以立体声链接峰值 `max(|L|, |R|)` 驱动 5 ms 窗前瞻包络 + 50 ms 指数释放 + 软拐点（`-2.5 dB` 起软过渡到 `-1.0 dB` 硬顶），确保立体声像绝不偏移、输出绝不超出 ceiling。
       - 当响度均衡**开启**但当前曲目 `use_limiter == false` 时：保持极简固定旁路（若跨曲目从 `false` 切到 `true` 或反向切换，通过 64 帧交叉淡化对齐，或在响度开启期间统一固定经过 5 ms 延迟线而仅在峰值超限时计算增益衰减 `g < 1.0` - 推荐**响度开启时统一固定 5 ms 延迟线、不超限时 `g = 1.0` 纯直通**，彻底杜绝曲目边界与实时切换限幅状态时的 5 ms 跳变咔嗒声！）。
- **替代方案**：解码线程提前应用增益 + 限幅 - 切换设置存在高达 11.8 秒延迟，体验不可接受。

### 决策 5：旧 `queue.json` 恢复时的响度字段刷新

用户升级服务端并完成部分/全部响度分析后，本地重启恢复的 `queue.json` 中缓存的 `Track` 可能仍是不带响度字段的旧快照。

- **选择**：沿用 `Queue::refresh_artists` 的既有无侵入刷新思路，在后台曲库分页/详情加载拿到含响度字段的 `Track` 时顺带回填队列中同 `track.id` 条目的响度字段；此外若登录后恢复了队列、服务端 `features.loudness == true` 且当前队列曲目缺响度字段，在起播前或后台发起一次轻量的 `GET /tracks/{id}` 补全当前曲目响度并即时更新生效计划。

### 决策 6：UI 与设置页布局

- 遵循 `crates/app/ui/settings.slint` 的 `SettingRow` + `Switch` / `ComboBox` 风格，在「播放 / 暂停淡入淡出」下方新增响度均衡区块：
  - 主开关：`响度均衡`（说明：平衡不同曲目的听感响度；仅作用于本机播放，不影响 Sonos 投放；服务端不支持时禁用并提示）。
  - 开启后展开 3 行紧凑配置：`响度目标`（副标题展示 `当前曲库中位数约 -X.X LUFS` 若可用）、`对齐方式`、`峰值保护`。
- `crates/core/src/browse.rs` 的 `track_facts` 与 `Core::emit_track_info` 增加响度信息行（仅在曲目有响度数据时显示），输出如 `集成响度：-8.4 LUFS · 真峰值 +0.2 dBTP`、`专辑响度：-9.1 LUFS`、`响度补偿：-2.6 dB（纯线性）` 或 `+3.0 dB（柔和限幅）`。
- 同步更新 `tools/ui-snapshot` 假数据，覆盖设置页响度展开态与查看信息弹窗回归截图。

## Risks / Trade-offs

- [`libmpv` 在不同 Linux 发行版上的系统版本可能低于 0.38（`loadfile` 参数个数差异）] -> 封装 `Player::load_with_af` / `append_with_af` 时优先按 `mpv_client_api_version` 或 4 参数调用失败自动回退 2 参数 + `FileLoaded` 补设 `af`，保证 Windows 捆绑新版实现帧边界同步、Linux 旧系统库不报错。
- [`libmpv` 实时修改 `af` 在极少数 WASAPI 独占驱动下可能引发短暂停顿] -> 仅当有效 `af` 字符串实际变化时才下发 `set_property("af", ...)`；曲目内音量微调只要未改变 `use_limiter` 状态就不重设 `af`。
- [`libmpv` 在软件音量 < 100% 且 `af` 输出 > 0 dBFS 浮点时，不同 libmpv 版本是否在 `softvol` 前削波需验证] -> Windows 实测清单加入该用例；且由于核心场景是 100% 音量，若发现某版本 mpv 在 `af` 输出端提前钳位，可直接对 mpv 侧按 `user_vol_db = 0` 计算安全上限。
- [ASIO 响度开启期间引入 5 ms 固定 Lookahead 延迟] -> 5 ms 对音乐播放/暂停/seek 完全无感，且在 `output_latency_frames` 中可计入或忽略（远小于 250 ms 进度步长），换来曲目边界与实时切换零爆音。

## Migration Plan

1. `settings.json` 新增可选字段 `loudness`，缺省默认 `enabled: false`，老用户无缝平滑升级。
2. 客户端与旧版服务端搭配运行时，`features.loudness` 为 `false`，响度均衡保持关闭且不改变原有播放行为。
3. 桌面端与后端联调通过并经 Windows 实机（共享 / WASAPI 独占 / ASIO 外置 DAC）听感验证后，再基于同一服务端 API 契约推进 Android `MusicBridge` 接入。
