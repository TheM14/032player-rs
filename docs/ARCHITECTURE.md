# 架构

初稿，M0 实现时按实际情况修改。

## Workspace 布局

```
032player-rs/
  Cargo.toml            # [workspace]
  crates/
    api/                # 032 HTTP 客户端 + 类型（无 UI、无 mpv）
    session/            # 播放上报状态机（纯逻辑，可注入时钟 / 传输 / 存储）
    lyrics/             # 歌词时间轴（当前行定位）
    core/               # 包名 client-core（`core` 与标准库冲突）。队列、设置、登录态、图片缓存，
                        # 串起 api + session；播放器通过 trait 注入
    sonos/              # Sonos 控制：SSDP 发现、拓扑、AVTransport 队列（无 032 知识，可 cargo test）
    asio/               # Windows ASIO 驱动 COM 封装与无锁 SPSC 环形缓冲（零 UI / libmpv 依赖）
    player/             # libmpv 封装 + ASIO 播放后端（只有这里依赖 libmpv / asio）
    os/                 # SMTC / MPRIS / 托盘 / 单实例 / 代理 / 文件对话框
    app-ui/             # 编译 crates/app/ui/app.slint 的独立叶 crate（隔离生成代码与 profile）
    app/                # Slint UI 胶水层与二进制入口
```

依赖方向：`app -> {app-ui, core, player, os}`，`player -> {asio}`，`core -> {api, session, lyrics, sonos}`。
`core` 不依赖 `player` / `asio` / `os`：播放器和系统集成以 trait 形式在 `core` 定义，由 `app` 注入实现。
这样 `api` / `session` / `lyrics` / `sonos` / `core` 都能在无 libmpv、无显示的机器上 `cargo test`。

## 选型（暂定）

| 用途 | crate | 备注 |
|------|-------|------|
| UI | `slint` | D2，M0 验证 |
| 异步运行时 | `tokio` | UI 线程之外跑网络和解码 |
| HTTP | `reqwest`（rustls） | |
| 序列化 | `serde`、`serde_json` | 类型对照 `music032-types.ts` 和服务端 `internal/storage` |
| 错误 | `thiserror`（库）、`anyhow`（app） | |
| 播放 | `libmpv2`、`symphonia`、`asio` | Windows 随包带 `libmpv-2.dll`，独占模式可选 WASAPI 或 ASIO |
| 媒体控制 | `souvlaki` | SMTC / MPRIS / macOS 一套 API |
| 托盘 | `tray-icon` | |
| 文件对话框 | `rfd`（Linux 走 xdg-portal，不依赖 GTK） | 歌单封面（异步非阻塞） |
| 凭据存储 | `keyring` | API Token、媒体凭据 |
| 设置 / 状态 | `serde` + JSON 文件，`directories` 定位 | |
| 缓存 | 内存压缩字节 LRU（`lru`，400 项）+ UI 解码 LRU（96 项） | |
| 图片解码 | `image`（后台线程，按显示尺寸 Lanczos3 缩放） | |
| 时间 | `chrono` | M3（相对时间、本地时间） |
| 打开网页 | `open` | M3 |
| 日志 | `tracing` + `tracing-subscriber`（滚动文件） | |
| 更新 | `api::update`（读取 GitHub Releases，只提示不自动安装） | M4 |

## 关键设计

### 线程模型

- UI 线程只做渲染和事件分发。
- `core` 在自己线程上的 tokio 运行时里跑一个循环，持有全部状态：
  UI 发 `Command`（`Commander` 可克隆），引擎信号以 `EngineEvent` 进来，后台任务结果走内部通道；
  状态变化以 `Event` 推给 sink，`app` 在 sink 里用 `slint::invoke_from_event_loop` 切回 UI 线程。
  `core` 不依赖 Slint，UI 侧状态放在 UI 线程的 thread_local 里。
- 播放进度：`player` 观察 mpv 的 `time-pos`，`core` 按 250 ms 步长节流后只更新进度相关的少数属性，
  不触发整页重建（这正是 Electron 版的痛点之一）。
- 切歌时序：`loadfile replace` 之后、新文件 `FileLoaded` 之前，上一首遗留的进度 / 时长 / EOF 一律丢弃。
- 无缝播放：当前文件 `FileLoaded` 后，把队列的"自动下一首"`append` 到 mpv 播放列表（`prefetch-playlist`），
  队列或循环模式变化时先 `clear_upcoming` 再重新 `append`。EOF 时若下一首就是预加载的那首，mpv 已经自己切过去，
  `core` 只更新状态不再 `load`。`FileLoaded` 带回 mpv `path`，与预期不符就重新 `load`（队列恰好在曲目边界被改的竞态）。
- 列表：登录后后台分页（500 / 页）全量拉取专辑和曲目，逐页追加到模型；ListView 只实例化可见行。
- 封面：专辑网格是自定义 Model，`row_data` 只对可见行调用，在那里才发封面请求；
  请求尺寸 = 卡片逻辑尺寸 x 缩放比例，向上取最接近的服务端尺寸（256 / 512 / 768 / ...）；
  `core` 缓存压缩字节（LRU 400），在 blocking 线程解码并缩放到显示尺寸，UI 只做 `SharedPixelBuffer` 拷贝；
  UI 侧最多保留 96 张解码封面，超出丢弃最早的，滚回来再从字节缓存解码。
  艺人页发行分组在同一个 Flickable 里用普通 `for`（所有行都实例化），网格设为 lazy：
  `CardRows` 的行进入视口（上下各留半屏）时回调 `card-row-shown`，只有显示过的卡片才请求封面。
- HTTP：reqwest 开 `gzip`，服务端压缩 JSON 响应（大列表页压缩到约 1 / 13）。

### 曲库浏览（M2）

- `core::browse`：列表筛选（`AlbumFilter` 等）、UI 用的行 / 详情视图类型，以及收藏和歌单在客户端的筛选排序（`/favorites/*`、`/playlists` 只分页）。
- `core/src/controller/library.rs`：`Core` 上的列表查询、详情、搜索、收藏、歌单编辑。请求在 tokio 任务里跑，结果以
  `Internal::Loaded(target, seq, ..)` 回到循环；每个目标（专辑页、曲目页、艺人页、三个收藏页、歌单、详情、搜索）只应用最新序号的结果，
  退出登录时清空序号表，旧会话的结果全部丢弃。
- 可播放的曲目列表按 `TrackList`（曲目页 / 收藏 / 详情 / 搜索）存在 `core`，UI 只传"哪个列表的第几行"；
  专辑、艺人、歌单作为 `TrackSource` 由 `core` 取曲目。
- 随机排序：`api::RandomSeeds` 按查询（不含分页）记 seed，分页加载时每页取同一个 seed，重新从 offset 0 加载换新 seed。
- UI：一个通用卡片网格模型（`app/src/cards.rs`，专辑 / 艺人 / 歌单共用，可见行才请求封面），曲目列表共用一个组件；
  导航历史在 `app/src/ui.rs`，每项同时保存主内容区和专辑信息栏的滚动偏移；前进从顶部开始，返回时恢复，
  异步详情到达后重试。详情页进入时向 `core` 发 `Open*`，返回时重新请求。
- 转码：`settings.json` `transcode`，服务端支持才生效。MP3 / Opus 实时转码不支持 Range，拖动进度时带 `offsetMs` 重新加载，
  `core` 记住偏移，引擎回报的时间加上偏移才是播放位置。

### 032 专属页面（M3）

- `api/src/m032.rs`：主页、作品、系列、每日推荐、播放记录、`/tracks/{id}/works`、歌词搜索 / 选用 / 翻译的类型和端点。
- `core::m032`：UI 用的视图类型和标签（用途 OP / ED、作品类型、专辑关系、每日推荐副标题等，对照 Electron `home/music032/lib/format.ts`），纯函数，有单元测试。
  时间显示（"3 天前"、本地时间）用 `chrono`。
- `core/src/controller/m032.rs`：主页（含部分刷新：作品 / 主题曲换一批、编年、换一张、每日推荐 5 秒轮询）、作品 / 系列 / 播放记录列表、
  每日推荐操作、正在播放标识。主页的曲目行是独立的 `TrackList`（聚焦卡页签、接着听、最近播放、主题曲、播放记录），沿用"哪个列表第几行"的模式。
- 每日推荐的 `playbackContext` 挂在队列条目上（`QueueItem::context`，随 `queue.json` 保存），上报 start 时由 adapter 带出；
  接着听的断点是 `QueueItem::start_ms`，只在该条目第一次播放时生效。
- 正在播放标识：当前曲目变化时（服务端有 `trackWorks`）查 `/tracks/{id}/works`，再并上本次会话展开过的作品 / 系列 -> 曲目映射（最多 200 个），
  以 `Event::PlayingSources` 推给 UI。
- `core/src/controller/lyrics.rs`：歌词加载、按曲目的会话偏移（正值 = 歌词提前，对照 Electron `位置 + offset`）、服务端翻译（双语 / 只看译文、自动翻译）、
  在线搜索 / 预览 / 选用 / 清除。
- app：新页面的数据放在 Slint 全局 `Home` / `Lib032` / `LyricsSearch`（`ui/m032.slint`），Rust 用 `window.global::<..>()` 设置，避免层层转发属性；
  作品 / 系列海报和主页货架复用 `CardGridModel`（卡片 kind 3 / 4 为 2:3 海报）；主页小行、聚焦卡、每日推荐封面用按图片 key 回填的绑定
  （`ui/m032.rs` `bind_image`，按组在换数据时丢弃）。主页货架用左右翻页按钮，不嵌套 Flickable。
- 「在网页中打开」：`os::browser::open_url`（`open` crate）。
- 字体：`app/src/fonts.rs` 把内嵌的 Inter + Noto Sans JP（与 Electron 版同一套）+ Noto Sans SC（简体缺字回退，`assets/fonts`）注册到 Slint 共享的 fontique 集合，
  艺人页和主页的中文区块标题显式指定 Noto Sans SC，避免 JP 覆盖不全导致同一标题混排；日文内容仍优先 JP。
  设为 sans-serif / system-ui 的字体栈（需要 slint 的 `unstable-fontique-011` 特性；先 `BackendSelector::select()`，再注册，再建窗口）。ui-snapshot 用同一个文件。
- 主页聚焦卡背景：v3 改版采用中性横向渐变色带，不再对封面做离线模糊。

### Sonos 投放

- `sonos` crate：SOAP 调用（`roxmltree` 解析应答、DIDL-Lite）、SSDP（`if-addrs` 列出网卡，每个 IPv4 网卡各发一次 M-SEARCH）+ 手填地址、
  `GetZoneGroupState` 按协调器合并房间、`Speaker` 的队列操作（替换 / 插入 / 删除 / 移动 / 替换单条 / 跳转）、状态、音量。
  请求不走系统代理。`fake` 特性提供模拟音箱（wiremock，记录收到的动作），`core` 的测试也用它。
- `core::cast`（纯函数，有单元测试）：每首歌给音箱的形式（原文件 / 实时 MP3 / FLAC 缓存，对照 MusicBridge `SonosPlan.kt`）、
  队列窗口（> 2000 首）、队列编辑 diff（对照 032 网页 `cast.js`）、按 Track 序号 + URI 路径定位条目、识别音箱队列里的 032 曲目（接管）。
- `core/src/controller/cast.rs`：每个房间一个 actor 任务，按顺序执行命令，空闲时每秒轮询（每 10 次带音量），每 250 ms 发时钟 tick；
  消息带房间 generation，切换房间后旧消息丢弃。`mirror` 是所有已发命令执行完后音箱队列的样子，命令未执行完时轮询结果丢弃（避免把乐观状态打回去）。
  投放时引擎停着：`start_current` / `seek` / 暂停 / 停止 / 音量 / 队列变化各自分支到 cast，引擎事件只保留音量设置。
  音箱切歌时 `start_current(false, false)`（与无缝切歌同一路径），所以上报、歌词、正在播放、睡眠定时都沿用本机逻辑。
  切换输出：当前条目记为 `resume_at`、清 `started_uid`，再在新输出上 `play_current`，上报器看不到换曲，同一次聆听继续。
- 选定的房间存 `settings.json` `cast.room`，登录后先连回（空闲时探测并接管音箱正在播的 032 队列），再后台重新搜索以更新地址。

### 登录态

- 服务器地址存 `data_dir/session.json`；API Token 和媒体凭据存系统凭据库（服务名 `032player`，
  键 `api-token@{baseUrl}` / `media-token@{baseUrl}`）。`core` 定义 `SecretStore` trait，
  `os::secrets::Keyring`（`keyring` 4：Windows 凭据管理器 / macOS 钥匙串 / Linux Secret Service，纯 Rust）由 `app` 注入。
  凭据库调用可能阻塞，`core` 只在 `spawn_blocking` 里调用；打不开时只记日志，不保存。
- 启动发 `Command::Restore`：两个凭据都在就自动登录。登录表单里留空的凭据取已保存值。
- 已保存的凭据被服务端拒绝时删掉对应那个。会话中任一请求 401（列表、歌词、上报）：
  删 API Token、保留媒体凭据，停止播放回到登录页。用户退出登录：两个都删。
- 每次登录 / 退出递增 generation，旧会话的列表分页和上报定时器结果直接丢弃。

### 系统集成

- `os` 不依赖 `core`，`app/src/system.rs` 在 UI 线程上把两边接起来：core 事件同步到媒体控件和托盘；
  系统回调（其他线程）经 `slint::invoke_from_event_loop` 回到 UI 线程，再发 `Command`。
- 关闭窗口：开了"关闭时最小化到托盘"且有托盘则隐藏，否则 `quit_event_loop`。主循环是 `run_event_loop_until_quit`。
- 单实例只用标准库：锁文件 + 本机回环端口，不引入额外依赖。

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
- `paused-for-cache` 变化写入应用日志；mpv 原生警告写入 `logs/mpv.log`（每次启动截断），用于区分网络缓存与音频输出 underrun。
  原生日志可能含带 `mediaToken` 的 URL，分享前必须脱敏。断音根因未确认前不叠加全局缓存参数。
- 无缝播放用 mpv playlist 预追加下一首（`loadfile ... append`）或 `--prefetch-playlist`，M1 实测决定。
- 独占：Windows 支持 WASAPI 独占（mpv `audio-exclusive=yes`）以及原生 ASIO 后端（`crates/asio` + `symphonia` 解码，Opus 自动回退 mpv / WASAPI）；设备列表来自 `audio-device-list` 与 ASIO 驱动探测。
- libmpv 来源：Windows 由 `crates/player/build.rs` 下载固定版本的 shinchiro mpv-dev 包
  （SourceForge `mpv-player-windows/libmpv` 永久保留，sha256 校验），解压到 `target/libmpv/`，
  `libmpv.dll.a` 复制为 `mpv.lib` 供 MSVC 链接，`libmpv-2.dll` 复制到输出目录。
  `MPV_LIB_DIR` 指定本地目录可跳过下载。Linux / macOS 链接系统 libmpv。

### 图片

- 按显示尺寸向服务端请求（`size` 参数，原图不带参数）。
- 后台下载 + 内存压缩字节 LRU（400 项）+ 解码缩放到显示盒，UI 侧按 96 项上限缓存 `slint::Image`。
