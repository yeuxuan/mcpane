# MCPane

**Claude Code、Codex 与 MCP 工具的统一控制面。**

[![CI](https://github.com/yeuxuan/mcpane/actions/workflows/ci.yml/badge.svg)](https://github.com/yeuxuan/mcpane/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/yeuxuan/mcpane)](https://github.com/yeuxuan/mcpane/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

MCPane（MCP + Pane / MCP Control Plane）是一个 macOS / Windows 桌面应用，提供：

- 供应商管理与一键切换（官方订阅 OAuth / 第三方中转站，Claude Code 与 Codex 各自独立切换）
- 从 cc-switch 一键迁移：供应商（含 API Key）与 MCP 服务整批导入，Key 转存系统钥匙串
- MCP Server 管理（stdio + Streamable HTTP + SSE legacy），可批量导出 / 导入，导出时凭据自动换成占位符
- 统一 MCP Hub：多客户端通过同一个 `http://127.0.0.1:24860/mcp` 接入
- Project / Workspace 级 Server 与 Tool/Prompt/Resource 逐条启停
  （tool/prompt 另支持别名），按连接可选工具暴露模式（平铺 / 网关 / 自动，
  ADR-020：网关形态目录直给 + 按需取 schema + 发现即升格，
  避免大量工具挤爆模型上下文）
- Hub 请求日志与聚合统计仪表、配置 Diff、备份与回滚
- 应用内自动更新（签名校验；发布端点走 GitHub Releases latest.json）
- 开机自启：登录后静默驻留托盘，不弹窗口

## 安装

到 [Releases](https://github.com/yeuxuan/mcpane/releases/latest) 下载对应平台的安装包：

| 平台 | 安装包 | 系统要求 |
|---|---|---|
| macOS · Apple Silicon（M 系列） | `MCPane_x.y.z_macos-aarch64.dmg` | macOS 12.0+ |
| macOS · Intel | `MCPane_x.y.z_macos-x86_64.dmg` | macOS 12.0+ |
| Windows · x64 | `MCPane_x.y.z_windows-x86_64-setup.exe` | Windows 10 1803+ |
| Linux · x64 | `MCPane_x.y.z_linux-x86_64.AppImage` | glibc 2.31+（Ubuntu 20.04+） |

不确定 Mac 是哪种芯片：左上角  → 关于本机，看「芯片」一行。

当前安装包**未经代码签名**，首次打开会被系统拦下，放行一次即可
（此后升级由应用内自动更新接管，不会再触发）：

- **macOS**：Gatekeeper 提示无法验证开发者。打开一次（会失败）后进入
  「系统设置 → 隐私与安全性」，在底部找到 MCPane 被拦截的提示，点「仍要打开」
- **Windows**：SmartScreen 提示「已保护你的电脑」，点「更多信息 → 仍要运行」
- **Linux**：AppImage 下载后没有执行权限，先 `chmod +x` 再运行

## 架构

```text
Claude Code ────┐
Codex ──────────┼─ Streamable HTTP ─→ mcpane-hub ─→ stdio / HTTP / SSE MCP Servers
兼容客户端 ─────┘      (127.0.0.1:24860/mcp)
Claude Desktop ── stdio ─→ mcpane-hub bridge ──┘
```

只支持 stdio 的客户端（Claude Desktop 等）经 `mcpane-hub bridge` 接入：
桥进程把 stdio 上的 JSON-RPC 原样转发到数据面，无需 node/npx。

- `apps/desktop` — Tauri 2 + React 19 桌面端
- `binaries/mcpane-hub` — Rust 独立 Hub 进程（Axum + 官方 rmcp SDK）
- `crates/mcpane-*` — 领域、存储、适配器等库

## 开发

```bash
# 前端与桌面
pnpm install
pnpm dev                                 # Tauri 桌面（Vite + sidecar Hub）
pnpm typecheck && pnpm lint && pnpm test && pnpm build
pnpm e2e                                 # Playwright（chromium+webkit）
pnpm --filter @mcpane/desktop dev        # 仅 Vite（不启桌面窗口）

# Rust
cargo build --workspace
cargo test --workspace

# 单独启动 Hub（仅调试 Hub 本体时需要；`tauri dev/build` 会自动构建并托管
# sidecar Hub——拉起/崩溃重启/退出关停，已有实例则 adopt 不重复拉起）
cargo run -p mcpane-hub

# 从源码构建安装包（产物在 apps/desktop/src-tauri/target/release/bundle/）
# 因开启自动更新签名，需先设置 TAURI_SIGNING_PRIVATE_KEY（本地自用可自行
# 生成 minisign 密钥对，演练步骤见 docs/RELEASE_CHECKLIST.md）
pnpm --filter @mcpane/desktop tauri build
```

详细执行计划见 `docs/MCPane_EXECUTION_PLAN.md`，工程规则见 `AGENTS.md`。

## 状态

P0 开发中，产品主环已可用（Phase 0~4 完成，GATE-1/2/3 全部通过；
Phase 5/6 桌面 UI 与一键接入已达可用状态）：

- ✅ 真实 Claude Code / Codex 直连同一 `/mcp`（不同 Token、并发、撤销隔离）
- ✅ 配置安全引擎（最小 Patch / 快照 / 原子写 / 失败恢复 / 回滚）
- ✅ 聚合 Hub（stdio + Streamable HTTP + SSE legacy 下游；Tools/Prompts/
  Resources 三类聚合、命名空间、Session-Pinned、二次权限校验、
  健康监控自动恢复、下游 list_changed 自动重建 Catalog、请求日志脱敏）
  - 下游运行策略只实现 Shared 单实例；DB 的 `runtime_policy` 还放行
    `per-client-connection` / `per-upstream-session`，遇到这两个值 Hub 会
    告警并按 Shared 运行
- ✅ 桌面端全页 UI + 系统托盘（含 Provider 快捷切换）+ 开机自启 + 一键接入
  （Claude Code / Codex / Claude Desktop / Cursor / VS Code / Cline / Devin /
  Gemini CLI / Antigravity / Trae / Qoder / Qwen Code / CodeBuddy / ZCode）
  + 多 Workspace
- ✅ Hub sidecar 托管（应用自动拉起 / 崩溃退避重启 / 退出优雅关停 / adopt 外部实例）
- ✅ 自动化测试（Rust 单测/集成 + vitest + Playwright E2E）+ 性能基线
  + chaos 覆盖（进程即退 / 非法输出 / 调用超时 / 端口占用 / 认证限速 /
  健康恢复 / 下游 HTTP 4xx-5xx / SSE 断流 / DB 写锁竞争 / 注入类畸形输入）
- 🟡 Windows 适配（控制面 Named Pipe + DACL 收紧、配置行尾/BOM 保真、stdio 子进程
  Job Object 与黑窗抑制、Claude Desktop MSIX 路径、大凭据 DPAPI 降级）：
  两个 workspace 均经 `x86_64-pc-windows-gnu` 目标 clippy 核验，**尚未真机运行时验证**
- ✅ 发布流水线（打 tag 即构建 macOS 双架构、签更新包、生成 `latest.json`、
  产出草稿 Release），变更记录见 `CHANGELOG.md`
- ⏳ 待办：Apple 公证凭据、Windows 真机验证与代码签名、SBOM，
  已收敛到 `docs/RELEASE_CHECKLIST.md`

详细进度与 Gate 报告见 `docs/interop/`。

## 许可证

[MIT](LICENSE)

## 致谢

- [cc-switch](https://github.com/farion1231/cc-switch)（MIT）：MCPane 在功能探索阶段以它为对标参考；部分供应商预设数值与本地路由行为语义与其公开数据同源，均为独立实现。MCPane 同时提供「从 cc-switch 一键迁移」，直接读取其本机数据完成导入。
- [models.dev](https://models.dev)：模型价目数据来源。
- 各客户端与供应商的品牌图标仅用于产品内的识别用途，归各自权利人所有；来源记录见 `apps/desktop/src/icons/brand/README.md`。
