# 任务清单

当前里程碑的细粒度任务。完成打勾；需要用户在 Windows 上验证的标 **[Win]**。
里程碑定义见 `PLAN.md`，功能条目见 `FEATURES.md`。

## 当前状态（2026-10-28）

- **M4 进行中**：用户看过 M1-M3，基本功能可用；M4 先做界面细节和能力与 Electron 版对齐，再做主题 / 打包 / i18n。任务见下方"M4 - 对齐与打磨"。
  P0 五项的主体已完成（未提交），ui-snapshot 检查过（`home-*.png`、`queue*.png`、`artist.png`、`album-works*.png`），等用户按"M4 验证清单"验证。
- **M3 代码完成**，已提交（`466003b`），preview 包已更新（`032player-windows-x64-466003b.zip`，约 75 MB）（主页、作品、系列、每日推荐、播放记录、正在播放标识、歌词搜索 / 纠正 / 翻译 / 偏移 / 点击跳转、
  假名注音 / 罗马音）。界面用 ui-snapshot 检查过；等用户按"M3 验证清单"验证。
- **M2 代码完成**（曲库浏览：列表排序 / 筛选 / 随机 seed、专辑 / 艺人 / 歌单详情、收藏、搜索、歌单增删改 / 排序 / 封面、相似歌曲、
  随机播放全部、可选转码、队列拖动排序）。已合并到 `main`（`cf2a2be`），界面用 ui-snapshot 检查过。等用户按下方"M2 验证清单"验证，
  可以和 M1 验证清单一起做。M2 曾有三条并行实现分支，用户选定本实现，其余分支已删除。
- **M1 代码全部完成**（登录态、设置、队列与无缝播放、上报补全与重启恢复、系统集成、睡眠定时），CI 的 Linux / Windows job 均通过。
  等用户按下方"M1 验证清单"在 Windows 上一次性验证。
- M0：用户实测播放正常、封面清晰、没有阻断性问题；测量数据（内存、冷启动、滚动）还没收到，可以和 M1 一起测。
- 测试包：Actions 里手动运行 **Package** 工作流，产物在 Release `preview`（便携 zip：exe + `libmpv-2.dll`，静态 CRT）。
  当前 preview 是 `main` 的 `466003b`（M3）。注意 `preview` 只有一个：从任何分支运行 Package 都会覆盖它（每次运行的 Actions 构件仍保留 90 天）。
- M1 有意没做的（记在各小节）：最小化到托盘、Linux 托盘。
- 已知缺口（按里程碑排期）：歌词翻译、歌词面板手动滚动 / 点击跳转（M3）；主页、作品、系列、播放记录（M3）。

## M4 验证清单（Windows，进行中，P0 做完后用新 preview 包）

1. **图片清晰度**：专辑网格、主页、作品 / 系列海报、详情页封面、播放条封面，与 Electron 版同一专辑截图对比；拖动窗口大小后约 0.2 秒封面按新尺寸重新解码；
   拖到另一块缩放比例不同的屏幕后约 1 秒内重新加载。
2. **主页缩放**：窗口从窄到宽拖动，聚焦卡封面随宽度变化、≥1440 页面宽度时右侧出现曲目预览；双栏在 ≤1040 时上下排列；
   "最近添加"列数 / 张数随宽度变化；"按作品漫游"是横向作品卡；主题曲在宽窗口为两列。
3. **右栏**：队列按钮开关右栏；右栏里队列在上、歌词在下同时显示，拖动中间分隔线、拖动右栏左边缘改宽度，重启后保持；
   歌词面板右上角上移 / 下移 / 关闭；队列搜索、定位到当前曲、另存为歌单、清空。
4. **艺术家**：侧栏"专辑艺术家""艺术家"两个入口，后者包含作曲 / 作词 / 编曲等幕后人员。
5. **艺人页**：关于（展开 / 收起）、作品按"专辑 / 单曲与 EP / 合辑与现场 / 其他发行 / 合作发行 / 参与作品 / 幕后参与"分组、热门歌曲"查看全部"。
6. **专辑页**：标题下的作品胶囊；曲目标题下的署名行、"◆ OP"标签（点击进作品）；右侧信息栏（关联作品与曲目用途、发行信息、参与者、媒体信息、简介），
   窄窗口时"曲目 / 信息"页签。

## M3 验证清单（Windows，用最新 preview 包）

1. **主页**（服务端有 `/home`）：登录后默认进主页。聚焦卡四个页签；"最近在听"的"从 x 继续"从断点开始，"从头播放"播整张专辑；
   "随机重温"的"换一张"；右侧预览曲目单击从该首开始播放；"作品聚焦"的作品标签、播放系列全部歌曲、查看系列。
2. **接着听 / 最近播放 / 今日推荐**：有今日推荐时左栏是推荐 + 最近播放（行数缩短），没有时是最近播放 + 接着听（进度条）；
   今日推荐生成中会提示并每 5 秒刷新。
3. **按作品漫游 / 主题曲与插曲 / 编年 / 歌单 / 页脚**：类型切换、换一批（跨天后提示"今日推荐已更新"）、左右翻页；
   主题曲行的作品名进作品页，"+"加到队列末尾；编年点柱子切年份，"查看全部"跳到带年份筛选的专辑页；页脚"作品关联待审核"打开网页。
4. **作品**：列表搜索 / 类型 / 排序；详情的系列横幅、关联专辑（关系角标、曲目用途）、收录于精选集曲目（用途标签）、播放全部、
   加入队列、在网页中打开、在 Bangumi 查看。
5. **系列**：列表搜索；详情的成员（多类型时有类型角标）、系列专辑、播放全部 / 加入队列；旧服务端点侧栏"系列"打开网页。
6. **每日推荐**：详情的新发现 / 文件缺失 / 不再推荐、喜欢 / 不喜欢（原因）/ 撤销、换一个（用完后提示）、另存为歌单、
   今天的其他推荐；从推荐播放后服务端播放记录里应带推荐上下文（"加入播放队列"的不带）。
7. **播放记录**：状态、进度、次数；双击播放；"清空播放历史"确认后列表清空。
8. **正在播放标识**：播放作品 / 系列里的歌时，作品卡片右上角、系列详情标题出现标识；从每日推荐播放时推荐行高亮。
9. **歌词**：点歌词行跳转；滚动歌词后约 4 秒恢复自动居中；底部"−/+"偏移（点数字归零）；"翻译"（服务端开启翻译时）显示双语，
   设置里改成"只看译文"；"搜索"对话框选一条预览后"确定"替换歌词，"清除"去掉在线歌词；
   设置里打开"假名注音""罗马音"，日语歌曲的汉字上方出现读音、下方出现罗马音，中文歌词不受影响；包体比 M2 大约 11 MB（zip）。

## M2 验证清单（Windows，用最新 preview 包）

1. **列表**：专辑 / 曲目 / 艺人页的排序下拉、升降序按钮、搜索框（停止输入约 0.4 秒后刷新）、年份区间；"随机"排序每次重新选择都换一种顺序，
   滚动加载的后续页不重复。旧服务端（无 browse）不显示升降序和随机。
2. **详情**：点专辑卡片进专辑详情（卡片右下角圆形按钮直接播放）；多碟专辑序号显示"碟-曲"；音质汇总（不一致时"最高 …"）；
   点艺人名进艺人页（热门歌曲 + 作品，非本人专辑带"合作 / 参与 / 制作"角标）；艺人页"更多"里的"查看全部曲目 / 专辑"跳到带艺人筛选的列表，
   筛选标签可点 × 取消；左上角返回按钮。
3. **收藏**：曲目行的心形、卡片右键、详情页心形、播放条心形；收藏页三个标签页（曲目 / 专辑 / 艺人）及其排序筛选，取消收藏后列表刷新。
4. **歌单**：新建（歌单页按钮、右键"添加到歌单 → 新建歌单…"）、添加曲目 / 专辑 / 艺人到已有歌单（重复的会提示跳过）、
   歌单详情里右键"从歌单移除"、拖动行调整顺序、"更多"里编辑名称简介、更换封面（系统文件对话框）/ 恢复默认封面、删除歌单。
5. **搜索**：侧栏"搜索"，曲目 / 专辑 / 艺人三个标签页。
6. **相似歌曲**：曲目右键"相似歌曲"，详情页可播放或"另存为歌单"。
7. **随机播放全部**：曲目页按钮，替换队列为 100 首随机曲目（受年份区间影响）。
8. **队列拖动排序**：队列面板里按住一行上下拖，松开后顺序改变，当前曲不变；随机模式下也可以。
9. **转码**（服务端开启转码时设置页才出现）：切到 MP3 / Opus 后下一首起生效；拖动进度会重新请求（有短暂停顿）；切回"原始文件"恢复 bit-perfect。

## M1 验证清单（Windows，用最新 preview 包）

日志在控制台窗口里（release 版暂时保留控制台）。有问题请附截图和相关日志。

1. **登录**：登录后关掉程序重开，应自动登录。点侧栏"退出登录"，回到登录页，Windows 凭据管理器（"Windows 凭据"）里 `032player` 的两条应消失。
2. **设置**（侧栏"设置"）：切换输出设备；播放中开关独占模式（不支持的设备应提示失败并回到原状态）；重启后音量、设备、独占、歌词面板开关保持。
3. **队列**：曲目 / 专辑右键"下一首播放""添加到队列末尾"；播放条右侧队列按钮打开队列面板（双击播放、悬停 × 移除、清空）；
   随机、循环（列表 / 单曲，单曲带"1"）；**同一专辑连续曲目之间应无空隙**（无缝）。
4. **重启续播**：播放到一半暂停，关掉重开：队列、当前曲、进度应恢复（停止状态），按播放从原位置继续；服务端播放记录里这次不应被算成两次。
5. **系统集成**：键盘媒体键（播放 / 暂停、上一首、下一首）；音量浮窗 / 锁屏的媒体控件显示标题、艺人、封面，按钮可用，进度可拖；
   托盘图标右键菜单各项；设置里开"关闭时最小化到托盘"后关窗口应隐藏到托盘、继续播放，单击托盘图标恢复；
   程序运行时再双击 exe，应唤起已有窗口而不是开第二个。
6. **睡眠定时**：播放条月亮按钮，菜单位置是否正常；"本曲结束后"应停在下一首；定时（可选 5 分钟）到点暂停，旁边显示剩余时间。
7. **M0 遗留**：中日文输入法（登录页输入框）；1 万行曲目列表滚动是否流畅；任务管理器里的内存（空闲 / 播放 / 滚动时）；
   日志里 `window shown` 的耗时（冷启动）。有 Electron 版同场景的数据更好，用来决定 D2（Slint）。

## 下一次对话从这里开始

1. P0 剩余：设置页右栏开关、曲目"查看信息"、艺人页分组播放按钮和收藏歌曲；然后 P1。
2. 收用户的 M1-M3 验证结果，修问题；把 M0 测量数据写进 `PLAN.md`，决定 D2（Slint）是否确认。

## M4 - 对齐与打磨

来源：用户反馈（2026-10-28）+ 对照 Electron 版逐项核对。P0 是用户点名的问题，先做；P1 是核对中发现的明显差异；P2 是原 M4 范围。
每项完成后用 ui-snapshot 检查（需要时加宽 / 窄窗口的快照）。

### P0 - 用户反馈

#### 1. 图片清晰度（Electron 明显更清晰）

Electron：`ItemImage` 按 `imageRes`（卡片 300、表格 80、页头 300、侧栏 400、全屏 0=原图）请求，服务端吸附到 256/512/…，
浏览器按设备像素比高质量缩放。Rust 版目前的问题（`core/images.rs`、`app/cards.rs`、`app/ui/m032.rs`）：

- [x] 网格封面尺寸只在超过已解码尺寸 125% 时才重载（`CardGrid::set_layout`），放大窗口后最多被拉伸 25%；改为按显示尺寸精确解码（尺寸稳定 200 ms 后重载，新图到达前保留旧图）
- [x] 主页 / 作品 / 系列等用固定逻辑尺寸取图（`bind_image(.., 170.0)` 等）。核对后：主页卡片都走网格模型；聚焦卡封面改为随宽度变化，尺寸变化后按新尺寸重取
- [x] 客户端缩放：解码后用 Triangle 缩到显示尺寸，再由渲染器按非整数像素位置采样，两次重采样发虚；改为 CatmullRom，卡片尺寸取整到物理像素（`1phx`），图片 1:1 绘制
- [x] 播放条小封面借用专辑网格的尺寸（几百像素缩到 56），渲染器缩小时无 mipmap 会锯齿；单独按播放条尺寸取图
- [x] 缩放比例变化（拖到另一块屏幕）时重载图片
- [ ] 对比验证：同一专辑在 Electron 和 Rust 版截图对比 **[Win]**

#### 2. 主页缩放布局（对照 `features/home/music032/music032-home.module.css` 的容器断点）

- [x] 聚焦卡：封面 `clamp(180px, 17vw, 244px)`；宽度 ≥1440 时右侧显示曲目预览，否则不显示预览、改为"专辑详情"链接
- [x] 双栏：左右 7:5；≥1440 左栏 `minmax(520px, 720px)`；≤1040 改为单栏上下排列
- [x] 最近添加：默认 3 列 6 张；≥1440 4 列 8 张；≥1880 5 列 10 张；≥2240 6 列 12 张；≤1040 6 列 6 张；≤700 3 列（现在固定 12 张、列数不对）
- [x] 按作品漫游：Electron 是横向作品卡（76px 海报 + 类型 / 标题 / 用途 / 系列，宽 268-300px，横向滚动），不是竖版海报（`WorkCardH`，宽 288px，左右翻页）
- [x] 货架（编年 / 歌单）：卡宽 `clamp(150px, 10.5%, 232px)`，随宽度变化
- [x] 主题曲与插曲：≥1440 两列；≤700 隐藏作品列；≤760 隐藏行内时间
- [x] 窄窗口 / 宽窗口快照（ui-snapshot `home-900/1180/1700/2300.png`）

#### 3. 右侧面板：队列和歌词同时显示（对照 `now-playing/components/sidebar-play-queue.tsx`）

Electron：播放条"队列"按钮开关整个右栏；右栏里队列和歌词上下分栏（默认都显示，顺序 queue / lyrics，分隔线可拖动、比例持久化），
歌词面板右上角有上移 / 下移 / 关闭；队列面板顶部有搜索和操作；播放条"歌词"按钮打开全屏播放器的歌词页。

- [x] 右栏容器：队列 + 歌词上下分栏，可拖动分隔线，比例存 `settings.json`（`rightPanel`）；右栏宽度可拖动（260-720px）并保存
- [x] 歌词面板右上角上移 / 下移 / 关闭；队列按钮开关整个右栏；歌词按钮暂时切换右栏歌词（全屏播放器做好后改为打开全屏歌词页）
- [ ] 设置页：右栏显示队列 / 显示歌词开关（现在只能用歌词面板的关闭和歌词按钮）
- [x] 队列面板顶部：搜索（搜索时不能拖动排序）、定位到当前曲、另存为歌单（`TrackSource::Queue`）、清空

#### 4. 区分歌手和幕后人员

- [x] 侧栏两个入口："专辑艺术家"（`role=album`，原来的"艺人"）和"艺术家"（`role=all`，含作曲 / 作词 / 编曲 / 制作），对照 `settings.store.ts` `sidebarItems`（共用一个结果列表，切换时重新查询）
- [x] 专辑详情右侧信息栏（`albums/music032/music032-album-inspector.tsx`；页面宽度 ≥960px 时在右侧，否则"曲目 / 信息"页签切换）：关联作品（含曲目用途，可展开）、发行信息（类型、演唱者、发行 / 原发行日期、厂牌、编号、国家、流派）、
  **参与人员**（`albumPeople`：演唱 / 作曲 / 作词 / 编曲 / 制作，按参与曲目数排序，演唱者优先）、媒体信息（`albumSpecLabel` 区间格式、总大小）
- [x] 专辑页头作品胶囊（`Music032AlbumWorkCapsules`，`GET /albums/{id}/works` + 每部作品的详情 / 专辑 / 曲目，详情显示后再补）；曲目行的作品用途标签"◆ OP"（点击进第一部作品；Electron 是弹出作品列表）；曲目标题下的署名行"作词 · 作曲 · 编曲 · 制作"
- [ ] 曲目右键"查看信息"：标题、艺人、作曲、作词、编曲、制作、音质、路径（对照 Feishin 曲目信息弹窗，只列 032 有的字段）

#### 5. 艺人页（对照 `artists/components/album-artist-detail-*.tsx`、`hooks/use-artist-albums-grouped.ts`）

- [x] 作品按 032 分组（与服务端 `groupArtistDiscography` 一致）：专辑 / 单曲与 EP / 合辑与现场 / 其他发行 / 合作发行 / 参与作品 / 幕后参与；
  个人发行按 `releaseKind || albumType` 归类（ep/single、compilation/live、bootleg/other/soundtrack、其余为专辑）
- [x] 页头：艺人图、名称、类型 / 国家 / 别名、专辑数 / 曲目数；"关于"区块显示简介（`biography`，带来源，超过三行可展开）
- [x] 区块顺序按 Electron 默认：关于 → 作品分组 → 热门歌曲（服务端给的前若干首 + "查看全部"）
- [x] 每组用卡片网格（每组独立网格模型）
- [ ] 每组标题旁的播放 / 下一首 / 末尾按钮（Electron `AlbumSection`），需要"多张专辑"的 `TrackSource`
- [ ] "收藏的歌曲"区块（Electron `FAVORITE_SONGS`）

### P1 - 核对中发现的差异

- [ ] 侧栏：导航图标（对照 `sidebar-icon.tsx`）、侧栏歌单列表（`sidebar-playlist-list.tsx`）、底部当前播放封面（`sidebarImage`）、顶部搜索框和前进 / 后退
- [ ] 曲目表格：标题列带缩略图 + 艺人（Feishin 默认列：序号、标题（含封面和艺人）、时长、专辑、流派、年份、收藏）
- [ ] 专辑详情：多碟分组标题、简介（`review`）、"更多来自该艺人"货架
- [ ] 播放条：第三行专辑名 + 音质；点封面打开全屏播放器
- [ ] 系列详情：成员按类型分组标题（现在用角标代替）
- [ ] 播放记录行封面
- [ ] 各列表页窗口缩放检查：最小窗口宽度、工具栏换行、长标题省略
- [ ] 其余页面逐页截图对照（搜索、歌单、收藏、设置、作品、系列、每日推荐），差异补进本清单

### P2 - 原 M4 范围

- [ ] 032 Light / Dark 主题、跟随系统、强调色（`shared/themes/music032-*`）
- [ ] 全屏播放器（原图封面不带 `size`、元数据区音质、歌词 / 队列页签）
- [ ] 全局快捷键
- [ ] 自动更新（本仓库 GitHub Releases）、Windows 安装包、release 隐藏控制台
- [ ] i18n：zh-Hans / zh-Hant / en
- [ ] M4 验证清单 **[Win]**

## M3 - 032 专属页面

范围对照 Electron 版（`features/home/music032/`、`works/`、`series/`、`daily-mix/`、`playback-history/`、`lyrics/`）。
确定不做 / 有意调整的：

- **逐字卡拉 OK 不做**：032 歌词接口只有逐行时间（`{timeMs, text}`），Electron 版只在歌词带逐字时间时才进卡拉 OK，
  对 032 实际不会出现。
- 聚焦卡"作品聚焦"的"播放系列全部歌曲"：Electron 按卡片上最多 6 部作品逐个取曲目，这里用 `/series/{id}/tracks`（系列全部成员）；
  非系列（单部作品）时用 `/works/{id}/tracks`，结果一致。
- 接着听的续播位置只在该条目第一次开始播放时生效（Electron 每次成为当前曲都会跳转）。
- 歌词翻译目标语言默认 `zh-Hans`（Electron 继承 Feishin 默认 `en`；本客户端界面是中文）。

### 1. api

- [x] 类型：主页（`HomeResponse` 各区块、`RotationPage`、`WorkCard`、`SongRow`、`AlbumPane`、`SeriesPane`、`Chronicle`）、
  `Work` / `WorkDetail` / `AlbumWork` / `WorkTrack`、`Series` / `SeriesDetail` / `SeriesMember` / `SeriesAlbumEntry`、`PlayRef`、
  `PlaybackRecord`、`DailyMix*`、`TrackWork`、`LyricsSearchResult`、`LyricsTranslation`；能力位 `works` / `workPosters` / `workSeriesInfo` / `seriesTracks`
- [x] 端点：`/home`、`/home/works|songs|chronicle|spotlight/random`、`/works`（q、type、sort）及详情 / 专辑 / 曲目、
  `/series` 列表 / 详情 / 专辑 / 曲目、`/tracks/{id}/works`、`/playback/history`（GET / DELETE）、`/daily-mixes` 列表（202 生成中）/ 详情 /
  评价 / 换一个 / 另存 / 不再推荐、`/lyrics/search`、`/lyrics/remote/{source}/{id}`、`PUT|DELETE /tracks/{id}/lyrics/source`、翻译
- [x] 测试（wiremock）：`crates/api/tests/m032.rs` 7 条

### 2. core

- [x] 主页：`LoadHome`（`/home` + 最近播放 `/tracks?sort=recentlyPlayed` + 每日推荐，生成中每 5 秒轮询，最多 24 次）；
  作品轮换类型 / 换一批、主题曲换一批（`generation` 过期 `renewed` 时提示"今日推荐已更新"）、编年切换年份、随机重温换一张；404 / 501 回退（主页不可用时侧栏默认进专辑）
- [x] 作品列表（搜索、类型、排序、分页加载）/ 详情（关联专辑 + 曲目用途、收录于精选集曲目、系列横幅）
- [x] 系列列表（搜索、分页）/ 详情（按类型分组成员、系列专辑）；无 `series` 能力时显示不支持
- [x] 每日推荐：详情（新发现、文件缺失、不再推荐）、评价（赞 / 踩带原因 / 撤销）、换一个（429 次数用完）、另存为歌单、今天的其他推荐
- [x] 播放记录：分页、清空（确认）
- [x] 播放：`TrackSource::Work` / `Series` / `DailyMix`；单曲播放（`PlayTrack`，接着听带续播位置）；
  每日推荐播放带 `playbackContext`（服务端声明能力时，加入队列不带；随队列保存）
- [x] 正在播放标识：`/tracks/{id}/works`（`trackWorks`），旧服务端用会话内的作品 / 系列 -> 曲目映射（最多 200 个）；每日推荐按 context
- [x] 歌词：搜索 / 预览 / 选用 / 清除（`lyricsOnline`）、翻译（`lyricsTranslation`，双语 / 只看译文，自动翻译，`translation_disabled` 时隐藏）、
  偏移（本次会话、按曲目，±50 ms）、点击歌词行跳转
- [x] 接着听：先按新播放从 0 上报 start，文件加载后跳到断点并上报 seek（与 Electron 相同）
- [x] 测试：`crates/core/tests/m032_flow.rs` 6 条（主页 / 接着听 / 轮换 renewed、无主页接口回退、每日推荐 context、作品播放与标识、
  播放记录、歌词翻译 / 偏移 / 搜索），`m032` / `format` / `queue` 单元测试

### 3. lyrics

- [x] 假名注音、罗马音（`lyrics::japanese`）：`lindera` 6.2 + 内嵌 IPADIC（用户选择内嵌，exe 增加约 48 MB，zip 后约 11 MB），
  `wana_kana` 转罗马音。对照 `furigana.ts`：只处理含假名的行，读音去掉送り仮名，罗马音按词加空格；只看译文时不显示。core 按行缓存结果

### 4. app

- [x] 侧栏：搜索、主页（服务端有 `/home` 时）、专辑、曲目、艺人、作品、系列（无系列接口时打开网页）、播放记录、歌单、收藏、设置
- [x] 主页各区块、作品 / 系列 / 每日推荐 / 播放记录页面、作品海报 2:3；数据放在 Slint 全局 `Home` / `Lib032` / `LyricsSearch`
- [x] 歌词面板：工具栏（搜索、偏移、翻译、清除）、翻译显示、点击跳转、手动滚动后暂停自动居中 4 秒
- [x] 设置：歌词翻译（目标语言、显示方式、自动翻译）
- [x] 「在网页中打开」（`os::browser`，`open` crate）
- [x] ui-snapshot 补主页、作品、系列、播放记录、作品详情、系列详情、每日推荐、歌词翻译、歌词搜索
- [x] 注音 / 罗马音开关（设置页），歌词面板读音显示在汉字上方、罗马音在下方
- [ ] M3 验证清单 **[Win]**

简化（与 Electron 不同的地方）：系列详情的成员不分组显示，多种类型时用卡片角标标出类型；系列列表用海报网格而不是行；
播放记录行没有封面；主页货架用左右翻页按钮（不嵌套滚动）。

## M2 - 曲库浏览

### 1. api

- [x] 类型：`Artist` `ArtistDetail`（`releases` 带 `relation`、`topTracks`）`AlbumDetail` `Playlist`（`durationMillis` 可缺）`PlaylistDetail`
  `PlaylistItemResult` `PlaylistArtwork` `SimilarTrack`；`Capabilities::can_transcode`
- [x] `/browse/artists` / `/artists`（`ArtistListQuery`：role、favorite、sort、order、seed）
- [x] `RandomSeeds`：按查询（不含分页）记 seed，offset 0 换新 seed，最多 50 个（对照 `withRandomSeed`）
- [x] 专辑详情（`trackCount` 超过嵌入曲目时用 `/tracks?album=` 补齐）、艺人详情、艺人全部曲目
- [x] 收藏 PUT / DELETE、`/favorites/albums|tracks|artists`
- [x] 歌单：列表、详情、新建、改名、删除、`items/append`（曲目 / `albumId`）、`items/remove`、`order`（带 `expectedRevision`）、
  替换、封面上传（multipart 字段 `image`）/ 恢复
- [x] `/tracks/{id}/similar`（limit 1..100，去掉种子曲目）；转码 URL（mp3 / ogg 码率取不超过请求值的档位，`offsetMs`；flac `maxSampleRate=48000`）
- [x] 测试：`crates/api/tests/library.rs` 7 条，`query.rs` 单元测试 4 条

### 2. core

- [x] `browse` 模块：列表筛选（`AlbumFilter` / `TrackFilter` / `ArtistFilter` / `PlaylistFilter`）、行和详情视图类型；
  收藏专辑 / 曲目在客户端筛选排序（对照 `favoriteAlbums` / `favoriteSongs`，无值的排在最后），歌单排序（`sortPlaylists`）；专辑音质汇总
- [x] `controller/library.rs`：`QueryAlbums|Tracks|Artists|Playlists`、`OpenAlbum|Artist|Playlist|Similar`、`Search`、`SetFavorite`、
  歌单增删改、`ShuffleAll`；每个目标只应用最新一次请求的结果（序号），退出登录时丢弃全部在途结果
- [x] 艺人详情：`releases` + `/albums?artist=` 里只靠署名关联的专辑（`credit`），对照 `artistDiscography`
- [x] 播放 / 入队 / 加入歌单统一用 `TrackSource`（列表行、整个列表、专辑、艺人、歌单）；列表行播放 = 从该行开始播放整个列表；随机播放（不改随机模式）
- [x] 收藏变化同步到所有列表、队列（`queue.json`）和已打开的收藏页
- [x] 歌单移除遇到 `playlist_conflict` 重读 revision 重试一次；排序先取最新详情再提交
- [x] 队列 `move_to`（拖动排序；随机时只改播放顺序）
- [x] 转码设置（`settings.json` `transcode`）；MP3 / Opus 不支持 Range，拖动进度时带 `offsetMs` 重新加载，进度 = 偏移 + 引擎时间
- [x] 无 browse 的服务端：曲目页默认按名称排序（旧接口不支持"最近添加"）
- [x] 修 CI 上偶发失败的 `lossy_transcode_seeks_by_restarting_the_stream`（固定等待 100 ms 改为轮询引擎调用）
- [x] 测试：`crates/core/tests/library_flow.rs` 6 条（随机 seed、收藏、详情 / 搜索 / 相似、歌单编辑与冲突重试、有损转码拖动、队列拖动），
  `browse` / `queue` / `format` 单元测试

### 3. app

- [x] 侧栏：搜索、专辑、曲目、艺人、歌单、收藏、设置；导航历史（详情页左上角返回）
- [x] 列表工具栏：搜索（0.4 秒防抖）、排序下拉（切换时用该排序的默认方向）、升降序、年份区间、艺人筛选标签
- [x] 通用卡片网格（专辑 / 艺人 / 歌单，`cards.rs`）：点击打开详情，右下角按钮播放；右键菜单（播放、随机播放、下一首、末尾、添加到歌单子菜单、收藏、转到艺人、删除歌单）
- [x] 曲目行：心形收藏列；右键菜单（播放、下一首、末尾、添加到歌单、收藏、相似歌曲、转到专辑、转到艺人、从歌单移除）
- [x] 详情页：专辑（碟-曲序号、音质汇总）、艺人（热门歌曲 + 作品角标）、歌单（拖动排序、编辑、封面、删除）、相似歌曲（另存为歌单）
- [x] 收藏页（曲目 / 专辑 / 艺人）、搜索页（曲目 / 专辑 / 艺人）、歌单页（新建）
- [x] 播放条：心形收藏，点标题进专辑、点艺人进艺人页
- [x] 队列面板拖动排序
- [x] 设置页转码选项（只列服务端可用的格式）
- [x] `os::dialog`（`rfd`，Linux 用 xdg-portal，不依赖 GTK）：选择歌单封面图片
- [x] ui-snapshot 增加艺人、歌单、收藏、搜索、专辑详情、艺人详情、歌单详情（含编辑对话框）
- [ ] M2 验证清单 **[Win]**

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
- [x] 拖动排序（M2 完成）
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

### 6. 睡眠定时

- [x] 三种模式对照 `sleep-timer-button.tsx`：本曲结束后（任何换曲都停在新曲上，含手动切歌）、本专辑结束后（放完当前专辑最后一首停在下一首；
  切到别的专辑则取消）、定时 5 分钟到 4 小时（只在播放时倒计时，到点暂停）
- [x] "结束后停"不预加载下一首，mpv 放完即停；core 把队列前进一格并停止（上报为完整播放）
- [x] 播放条月亮按钮 + 弹出菜单，开启后旁边显示剩余时间或"本曲" / "本专辑"
- [x] 测试：`queue_flow.rs` 本曲结束、本专辑结束、定时开启 / 取消
- [ ] 弹出菜单位置、定时到点暂停 **[Win]**

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
