# 任务清单

当前里程碑的细粒度任务。完成打勾；需要用户在 Windows 上验证的标 **[Win]**。
里程碑定义见 `PLAN.md`，功能条目见 `FEATURES.md`。

## 当前状态（2026-10-28）

- M0 的代码部分已完成；用户在 Windows 上用 preview 包实测：**播放正常、封面清晰、没有阻断性问题**。
- 测试包：Actions 里手动运行 **Package** 工作流，产物在 Release `preview`（便携 zip：exe + `libmpv-2.dll`，静态 CRT）。
- 还没收到反馈的 M0 项：中日文输入法、滚动流畅度、内存、冷启动耗时、服务端播放记录、独占模式。
- 已知缺口（不阻断 M0，按里程碑排期）：
  - 歌词翻译（双语 / 只看译文）：M3，见 `FEATURES.md` 歌词一节。
  - 歌词面板不能手动滚动 / 点击跳转（M3）。

## 下一次对话从这里开始

1. 收尾 M0：向用户要剩余实测数据（内存、冷启动、滚动、输入法、播放记录），写进 `PLAN.md` 的 M0 小节，和 Electron 版对比，决定 D2（Slint）是否确认。
2. M1（顺序建议，进度见下方 M1 小节）：
   1. ~~登录态持久化~~（已完成，[Win] 项留到 M1 整体验证）。
   2. ~~设置持久化 + 设置页~~（已完成）。
   3. ~~队列 + 无缝播放~~（已完成；拖动排序未做，Slint ListView 拖放要自己实现，放到 M2 曲库交互时一起做）。
   4. ~~上报补全 + 重启恢复队列~~（已完成）。
   5. ~~系统集成~~（已完成）。
   6. 睡眠定时。
   M1 各项做完后再汇总 [Win] 项请用户一次性验证（不逐项请用户测，见 `AGENTS.md`）。
3. 如果用户更想先要歌词翻译，可以把 M3 的"翻译 + 显示方式"提前（接口：`POST /tracks/{id}/lyrics/translation`，能力位 `lyricsTranslation`）。

## M1 - 能日常使用

### 1. 登录态

- [x] `core`：`SecretStore` trait；服务器地址存 `session.json`；登录成功保存两个凭据，`Restore` 启动自动登录
- [x] 登录表单留空的凭据沿用已保存值（输入框占位提示"已保存，留空即沿用"）
- [x] 已保存凭据被拒绝时删除对应凭据；会话中 401（列表、歌词、上报）回到登录页，只删 API Token
- [x] 侧栏"退出登录"：结束播放会话，删除两个凭据
- [x] `os::secrets::Keyring`（`keyring` 4，纯 Rust，Windows target 编译检查通过）
- [x] 测试 3 条（`crates/core/tests/sign_in.rs`）：恢复 + 退出、401 后只重填 API Token、已保存 Token 失效
- [ ] 凭据管理器实测：重启后自动登录、退出登录后凭据管理器里条目消失 **[Win]**

### 2. 设置

- [x] `Settings`（`settings.json`，缺字段 / 损坏回落默认值）：音量、输出设备、独占模式、歌词面板开关
- [x] app 启动先读设置再创建 mpv（设备、独占、音量作为初始化选项）；core 保存改动（500 ms 防抖，退出时补写）
- [x] 音量以 mpv 回报的 `volume` 为准保存
- [x] `Engine` 加 `audio_devices` / `set_audio_device` / `set_exclusive`，在 blocking 线程执行；失败提示并回到原值
- [x] 设置页：输出设备下拉（系统默认 + mpv 设备列表，已保存但未连接的设备标"未连接"）、独占模式开关
- [x] 测试：`crates/core/tests/settings.rs`、`Settings` 单元测试
- [ ] 输出设备切换、独占模式播放中切换、重启后设置保留 **[Win]**

### 3. 队列与无缝播放

- [x] `Queue`：队列顺序 + 播放顺序（随机时为排列）+ 当前位置；语义对照 `player.store.ts`
  - 随机：开启时当前曲排第一、其余打乱；关闭回到队列顺序，停在同一曲
  - 循环：列表循环首尾回绕；单曲循环自动结束时重播，手动"下一首"仍前进；不循环到末尾回到第一首并停止
  - 下一首播放（插在当前曲后，随机时新曲打乱后紧跟当前曲）、加到末尾、移除（移除当前曲则播放接替它的那首）、清空（停止）
- [x] 循环 / 随机存进 `settings.json`
- [x] 无缝播放：当前曲 `FileLoaded` 后 `append` 下一首（mpv `prefetch-playlist=yes`）；队列 / 循环变化时 `clear_upcoming` + 重新 `append`；
  自动结束时若引擎已进入预加载的那首就不再 `load`
- [x] `FileLoaded` 带 mpv `path`，与预期 URL 不符（队列在曲目边界被改）时重新 `load` 正确的曲目；播放失败时 `stop`，防止 mpv 自己进入预加载
- [x] 单曲循环每一遍上报一次完整播放（adapter `on_repeated`）
- [x] UI：播放条随机 / 循环按钮（单曲循环带"1"角标）、队列按钮；右侧队列面板（双击播放、悬停移除、清空）；
  曲目行和专辑卡片右键菜单（播放 / 下一首播放 / 添加到队列末尾）
- [x] 测试：`queue.rs` 单元测试 9 条，`crates/core/tests/queue_flow.rs` 端到端（无缝、单曲循环上报、过期预加载、移除、末尾停止、清空）
- [ ] 拖动排序（M2）
- [ ] 无缝衔接听感（曲间无空隙）、右键菜单、单曲循环 **[Win]**

### 4. 上报补全与重启恢复

- [x] 核对 M0 已接好的上报：缓冲（`paused-for-cache` -> adapter `on_buffering`，播放条"缓冲中"）、播放失败（`on_error`）、
  clientKind `desktop`、退出 `client_closed`（keepalive，媒体凭据鉴权）
- [x] 队列存 `queue.json`（服务器、曲目、播放顺序、当前位置、随机），变化后 1 秒防抖写入，退出时补写
- [x] 播放位置存 `session.json`：暂停、停止、切歌、退出时写，播放中每 10 秒写一次
- [x] 登录同一服务器后恢复队列，停止状态显示当前曲、位置和歌词；adapter `on_queue_restored`，播放时用快照续上同一条播放链
  （`resumedFromSessionId`），文件加载后跳到保存的位置；未播放前拖进度条改的是续播位置
- [x] 测试：`queue.rs` 快照往返，`queue_flow.rs` 重启恢复（随机顺序、位置、续链）
- [ ] 重启后续播（位置、服务端播放记录不重复计次） **[Win]**

### 5. 系统集成

- [x] `os::media`（`souvlaki`，Linux 用纯 Rust 的 zbus 后端）：标题 / 艺人 / 专辑 / 封面 URL（512，带媒体凭据）/ 时长，
  播放状态和位置（状态变化、跳转、播放中每 5 秒）；媒体键、SMTC 浮窗按钮、快进快退（±10 秒）、拖动进度、Raise / Quit
- [x] Windows 取 HWND：Slint `raw-window-handle-06`；窗口 `show()` 后再创建媒体控件
- [x] `os::tray`（`tray-icon`，仅 Windows）：菜单与 Electron 版一致（播放 / 暂停、下一首、上一首、停止、打开主窗口、退出），
  左键单击显示窗口，提示文字为当前曲目
- [x] 设置"关闭时最小化到托盘"（默认关，与 Electron 一致；只在有托盘时显示）。事件循环改为 `run_event_loop_until_quit`
- [x] `os::instance` 单实例：`instance.lock`（`File::try_lock`）+ 回环 TCP 端口（`instance.port`），第二次启动唤起已有窗口后退出
- [x] `Command::Stop`（托盘 / SMTC 的"停止"：保留队列，再播放从头开始）
- [x] 窗口图标、托盘图标沿用 Electron 版 `assets/icons`
- [x] 修 CI 上偶发失败的测试（`start` 未被确认前关闭不会保存续播快照，测试改为等服务端收到 `pause`；固定等待改为轮询）
- [ ] 未做：最小化到托盘（Electron 默认关，Slint 没有最小化事件，要接 winit）、Linux 托盘（需要 gtk 或改用 `ksni`）
- [ ] 媒体键、SMTC 浮窗（封面、进度、按钮）、托盘菜单、关闭到托盘、第二次启动唤起窗口 **[Win]**

## M0 - 可行性验证

### 0. 环境与仓库

- [x] 容器内装 Rust stable（rustup，`~/.cargo`），clippy + rustfmt
- [x] GitHub 仓库 `lux032/032player-rs`（private），remote `origin`
- [x] 首次提交并推送
- [x] CI 首次跑通（Linux + Windows job 均通过，Windows 上 libmpv 下载成功）

### 1. Workspace 骨架

- [x] workspace（edition 2024，resolver 3），7 个 crate：`api` `session` `lyrics` `core`（包名 `client-core`）`player` `os` `app`
- [x] `rust-toolchain.toml`（stable）、`rustfmt.toml`、workspace lints
- [x] CI：`.github/workflows/ci.yml`，Linux（fmt / clippy / test）+ Windows（clippy / test / build app）
- [x] 依赖方向调整：`core` 不依赖 `player` / `os`（见 `ARCHITECTURE.md`），保证 `core` 可无 libmpv 测试

### 2. session（播放上报状态机）

- [x] 移植 `music032-playback-session.ts`（sans-IO：`poll_transmit` / `handle_outcome` / `on_timer`）
- [x] 移植 `music032-playback-adapter.ts`
- [x] 移植 `playback-reporter.test.mjs` 全部 25 条用例，全部通过
- [x] 补充：事件体 JSON 形状、网络错误按配置重试且复用同一 body

### 3. api

- [x] 类型：`Album` `Track` `ArtistRef` `TrackCredit` `Page` `Capabilities` `Lyrics`，缺字段走默认值
- [x] `Client`：Bearer 请求，非 2xx 解析 `{error:{code,message}}`，`is_unauthorized()`
- [x] `authenticate`：两个凭据必填且不同、`apiRevision == 3`、媒体凭据 Range 探测（空库跳过）
- [x] `/browse/albums`、`/browse/tracks`（sort / order / yearFrom / yearTo / seed），无 browse 回退 `/albums`、`/tracks`
- [x] 媒体 URL：`mediaToken`、`stream_url`、`image_url`（`size` 为 0 不发送）
- [x] `lyrics(track_id)`（404 返回 `None`）
- [x] wiremock 测试 13 条
- [x] 用真实服务端冒烟（登录、列表、播放在用户机器上正常）

### 4. lyrics

- [x] `SyncedLyrics::active_index`（当前行、偏移）

### 5. player

- [x] libmpv2 封装：load / append / next / stop / pause / seek / 音量 / 静音
- [x] 事件线程：time-pos、duration、pause、paused-for-cache、playlist-pos、idle、end-file（含 error）
- [x] 输出设备列表 / 切换，独占模式切换（回读校验 + 回滚，对照 `audio-exclusive.ts`）
- [x] `build.rs`：Windows 自动下载固定版本 mpv-dev（SourceForge 永久镜像，sha256 校验），生成 `mpv.lib`，复制 `libmpv-2.dll`；`MPV_LIB_DIR` / `MPV_DEV_URL` 可覆盖
- [x] 容器内 `cargo check --target x86_64-pc-windows-msvc` 验证下载 / 解压 / 缓存
- [x] MSVC 链接 `mpv.lib`（由 `libmpv.dll.a` 复制而来）可用（CI 构建 app 通过）
- [x] 实际播放（用户实测正常）
- [ ] 独占模式切换（入口在 M1 设置页，随 M1 一起验证） **[Win]**

### 6. core + app

- [x] Slint 1.18 依赖与构建（`fluent-dark` 风格；容器内能编译、链接）
- [x] `core`：独立线程上的 tokio 运行时、命令通道、事件 sink；`Engine` trait（`app` 用 mpv 实现）
- [x] `core`：队列（替换 / 下一首 / 上一首，>10 秒时"上一首"改为回到开头）
- [x] `core`：上报接线（adapter + 定时器 / HTTP 驱动，clientKind 按能力位，退出时 keepalive 发 `client_closed`，续播快照存 `playback.json`）
- [x] `core`：load 之后到 `FileLoaded` 之前忽略上一首遗留的进度 / 时长 / EOF（防止误判单曲循环或多跳一首）
- [x] `core`：封面加载（按显示尺寸选最接近的服务端尺寸，后台解码缩放到显示尺寸；缓存压缩字节 LRU 400）
- [x] `core` 端到端测试（wiremock + 假引擎）：登录、列表、播放、歌词、自动下一首、暂停、退出上报
- [x] 登录页（服务器地址、API Token、媒体凭据，错误提示；环境变量预填 / 自动登录，记住上次服务器地址）
- [x] 专辑网格（按宽度分列；只为可见行请求封面；封面尺寸 = 卡片尺寸 x 缩放比例；最多保留 96 张解码封面）
- [x] 曲目列表：后台分页全量加载（每页 500），ListView 虚拟化，双击播放，正在播放高亮
- [x] 播放条：封面、标题、艺人、音质、进度拖动、音量、上一首 / 播放暂停 / 下一首
- [x] 逐行同步歌词（自动居中滚动），非同步歌词可自由滚动
- [x] 开发工具 `tools/ui-snapshot`：软件渲染出 PNG，容器内检查布局
- [x] 打包：`.github/workflows/package.yml` 手动触发，产出便携 zip 到 Release `preview`
- [x] release 构建保留控制台窗口（M0 收集日志；M4 打包时隐藏）
- [x] 首次运行：mpv 初始化、播放（用户实测正常）
- [ ] 中日文字体显示和输入法 **[Win]**
- [x] 封面清晰度（按缩放比例请求，用户实测清晰）
- [ ] 滚动时内存、1 万行滚动流畅度 **[Win]**

### 7. 测量

日志里会打印 `window shown (first event loop turn)` 和耗时（从进程启动算起），可作为冷启动参考。

- [ ] 冷启动时间、空闲 / 播放 / 滚动内存、滚动帧率，与 Electron 版对比，写进 `PLAN.md` **[Win]**
