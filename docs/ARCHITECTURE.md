# 架构

初稿，M0 实现时按实际情况修改。

## Workspace 布局

```
032player-rs/
  Cargo.toml            # [workspace]
  crates/
    api/                # 032 HTTP 客户端 + 类型（无 UI、无 mpv）
    session/            # 播放上报状态机（纯逻辑，可注入时钟 / 传输 / 存储）
    lyrics/             # LRC / 逐字解析、翻译显示合并、注音 / 罗马音
    core/               # 包名 client-core（`core` 与标准库冲突）。队列、设置、登录态、图片缓存，
                        # 串起 api + session；播放器通过 trait 注入
    player/             # libmpv 封装（只有这里依赖 libmpv）
    os/                 # SMTC / MPRIS / 托盘 / 单实例 / 更新
    app/                # Slint UI，二进制入口
```

依赖方向：`app -> {core, player, os}`，`core -> {api, session, lyrics}`。
`core` 不依赖 `player` / `os`：播放器和系统集成以 trait 形式在 `core` 定义，由 `app` 注入实现。
这样 `api` / `session` / `lyrics` / `core` 都能在无 libmpv、无显示的机器上 `cargo test`。

## 选型（暂定）

| 用途 | crate | 备注 |
|------|-------|------|
| UI | `slint` | D2，M0 验证 |
| 异步运行时 | `tokio` | UI 线程之外跑网络和解码 |
| HTTP | `reqwest`（rustls） | |
| 序列化 | `serde`、`serde_json` | 类型对照 `music032-types.ts` 和服务端 `internal/storage` |
| 错误 | `thiserror`（库）、`anyhow`（app） | |
| 播放 | `libmpv2` | Windows 随包带 `libmpv-2.dll` |
| 媒体控制 | `souvlaki` | SMTC / MPRIS / macOS 一套 API |
| 托盘 | `tray-icon` | |
| 凭据存储 | `keyring` | API Token、媒体凭据 |
| 设置 / 状态 | `serde` + TOML / JSON 文件，`directories` 定位 | |
| 缓存 | 磁盘图片缓存 + 内存 LRU（`lru` 或 `moka`） | |
| 图片解码 | `image`（后台线程，按显示尺寸缩放） | |
| 日语分词 | `lindera`（替代 kuromoji） | M3 |
| 罗马音 | `wana_kana` | M3 |
| i18n | `slint` 内置 `@tr()` + gettext，或 `fluent` | M4 决定 |
| 日志 | `tracing` + `tracing-subscriber`（滚动文件） | |
| 更新 | 待定（`self_update` / velopack） | M4 |

## 关键设计

### 线程模型

- UI 线程只做渲染和事件分发。
- `core` 在 tokio 运行时上跑，持有全部状态；UI 通过命令通道发请求，`core` 通过
  `slint::invoke_from_event_loop` 推送状态更新。
- 播放进度：`player` 观察 mpv 的 `time-pos`，`core` 节流后只更新进度相关的少数属性，
  不触发整页重建（这正是 Electron 版的痛点之一）。

### api

- `Client { base_url, api_token, media_token, http }`，方法按资源分组（albums、tracks、browse、
  playlists、works、series、home、daily_mixes、lyrics、playback）。
- 统一错误 `ApiError { status, code, message }`；401 单独可识别，交给 `core` 触发重新登录。
- 分页上限 500（`limits.maxPageSize`）。
- 先只移植用到的类型，字段用 `Option` 容忍旧服务端缺字段。

### session

- 1:1 移植 `music032-playback-session.ts` 和 `music032-playback-adapter.ts`，保持语义与服务端
  `playback-reporter.js` 一致。
- sans-IO：状态机不碰网络。驱动方循环 `poll_transmit()` 取事件、发 HTTP、`handle_outcome()` 回填结果；
  `keepalive` 事件（退出时的 `client_closed`）不需要回填。同一时刻最多一个事件在途，等价于参考实现的串行队列。
- 定时器（心跳、重试）通过 `Scheduler` trait 注入，到期后驱动方调 `on_timer(id)`。存储、uuid、clientKind 同样注入。
- 测试用假实现，对照 `scripts/music032/playback-reporter.test.mjs` 逐条移植（`crates/session/tests/`）。

### player

- 一个 mpv 实例，事件循环在专用线程，向 `core` 发 `PlayerEvent`（时间、暂停、缓冲、结束 + 原因、错误）。
- 无缝播放用 mpv playlist 预追加下一首（`loadfile ... append`）或 `--prefetch-playlist`，M1 实测决定。
- 独占：Windows `audio-exclusive=yes` + WASAPI；设备列表来自 `audio-device-list`。
- libmpv 来源：Windows 由 `crates/player/build.rs` 下载固定版本的 shinchiro mpv-dev 包
  （SourceForge `mpv-player-windows/libmpv` 永久保留，sha256 校验），解压到 `target/libmpv/`，
  `libmpv.dll.a` 复制为 `mpv.lib` 供 MSVC 链接，`libmpv-2.dll` 复制到输出目录。
  `MPV_LIB_DIR` 指定本地目录可跳过下载。Linux / macOS 链接系统 libmpv。

### 图片

- 按显示尺寸向服务端请求（`size` 参数，原图不带参数）。
- 后台下载 + 解码 + 磁盘缓存，UI 只拿 `slint::Image`。列表滚动时取消不可见项的请求。
