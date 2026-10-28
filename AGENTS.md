# AGENTS.md

## 通用约定

- 不使用长破折号 "—"，用普通 "-"。
- 文档用中文，代码标识符和注释用英文。
- Conventional Commits（`feat(api): ...`、`fix(player): ...`）。scope 用 crate 名：`api`、`session`、`player`、`core`、`lyrics`、`os`、`app`，文档用 `docs`，构建用 `build` / `ci`。
- 只在用户明确要求时提交。
- YAGNI：先做 `docs/FEATURES.md` 里列出的功能，不为其他服务端留抽象。

## 先读

1. `docs/PLAN.md` - 已定决策和当前里程碑；`docs/TASKS.md` - 当前任务清单，完成后打勾。
2. `docs/ARCHITECTURE.md` - crate 边界。
3. 做某个功能前，在 `docs/FEATURES.md` 找到对应条目和 Electron 版源码位置，**以 Electron 版行为为准**。

## 参考来源（只读，不要修改）

| 路径 | 用途 |
|------|------|
| `/workspace/032musicplayer` | 现有 Electron 客户端，行为基准 |
| `/workspace/032musicplayer/MUSIC032_CHANGES.md` | 032 相关改动、能力位、已知限制的总表 |
| `/workspace/032musicplayer/src/shared/api/music032/music032-types.ts` | API 响应类型 |
| `/workspace/032musicplayer/src/renderer/api/music032/` | HTTP 客户端、controller、播放上报状态机 |
| `/workspace/032musicplayer/scripts/music032/*.test.mjs` | 上报状态机测试，移植成 Rust 测试 |
| `/workspace/032music-server` | 服务端（Go）。`internal/storage` 结构体、`docs/api/*.md` 是接口权威 |
| `/workspace/032music-server/internal/http/assets/playback-reporter.js` | 上报协议参考实现 |

## 开发环境说明

- 代理所在容器：aarch64 Linux，无 sudo，无 libmpv，无显示器。可用 rustup 装到 `~`。
  - 工具链已装在 `~/.cargo`，新 shell 先 `source ~/.cargo/env`。
  - 能做：`cargo check`、`cargo test`（无界面、无音频的 crate）、`cargo clippy`。
  - 已装 `x86_64-pc-windows-msvc` 标准库：`cargo check --target x86_64-pc-windows-msvc` 可检查 Windows 专属代码（不链接）。
  - 不能做：运行 UI、实际播放。`player` crate 只做编译检查（`cargo check` 不链接 libmpv）。
- 用户的主力平台是 Windows，UI 和播放在用户机器上 `cargo run` 验证，结果（截图 / 日志）由用户回传。
- 无界面 crate 必须能在没有 libmpv 和显示环境的机器上 `cargo test`。依赖 libmpv / UI 的代码只放在 `player` / `os` / `app`；
  `core`（包名 `client-core`）不依赖它们，需要时在 `core` 定义 trait，由 `app` 注入。

## 检查

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude player --exclude app
```
