# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

> 品牌命名冻结、协议双栈规则、Phase/Task 执行流程、密钥红线等**强制工程规则**见根目录
> `AGENTS.md`——冲突时以 `AGENTS.md` 及其引用的 `docs/MCPane_EXECUTION_PLAN.md` 为准，
> 本文件只补充「怎么跑起来、代码长什么样」。

## 项目是什么

MCPane：Claude Code / Codex 与 MCP 工具的统一控制面（macOS/Windows 桌面应用）。
核心是一个本地 MCP Hub，把多个下游 MCP Server（stdio/HTTP/SSE）聚合到单一
Streamable HTTP 端点，供多个客户端（Claude Code、Codex、Claude Desktop 等）复用。

```text
Claude Code ────┐
Codex ──────────┼─ Streamable HTTP ─→ mcpane-hub ─→ stdio / HTTP / SSE MCP Servers
兼容客户端 ─────┘      (127.0.0.1:24860/mcp)
Claude Desktop ── stdio ─→ mcpane-hub bridge ──┘
```

## 常用命令

```bash
# 前端 + 桌面（monorepo 用 pnpm workspace，仅 apps/*）
pnpm install
pnpm dev                                 # Tauri 桌面：Vite + sidecar Hub 一起拉起
pnpm --filter @mcpane/desktop dev        # 仅 Vite（不开桌面窗口，调前端用）
pnpm typecheck && pnpm lint && pnpm test && pnpm build
pnpm lint:fix                            # biome 自动修复
pnpm e2e                                 # Playwright e2e（chromium+webkit，独立于 vitest）

# 单个前端测试
pnpm --filter @mcpane/desktop test -- src/lib/clients.test.ts   # vitest 单文件
pnpm --filter @mcpane/desktop test -- -t "某个 test 名"          # vitest 按名筛选
pnpm --filter @mcpane/desktop e2e -- e2e/servers.spec.ts        # playwright 单个 spec

# Rust：根 workspace（Hub + 领域 crates，不含桌面壳）
cargo build --workspace
cargo test --workspace
cargo test -p mcpane-storage                    # 单 crate
cargo test -p mcpane-storage --test backup_restore   # 单个集成测试文件
cargo test -p mcpane-hub-downstream some_test_name   # 按名筛选
cargo run -p mcpane-hub                          # 单独起 Hub（仅调试 Hub 本体需要；
                                                  # tauri dev/build 会自动构建并托管 sidecar）

# Rust：桌面壳是独立的第二个 Cargo workspace（apps/desktop/src-tauri/Cargo.toml
# 里 [workspace] 为空表，与根 workspace 完全隔离），日常由 `pnpm tauri dev/build` 驱动，
# 需要单独 fmt/clippy 时：
(cd apps/desktop/src-tauri && cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings)

# 提交前建议全跑一遍（见 CONTRIBUTING.md）
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
pnpm typecheck && pnpm lint && pnpm test && pnpm build && pnpm e2e && pnpm brand-scan

# target/ 清理（两个 workspace 共享依赖各编译一份，容易堆到十几 GB）
pnpm clean:target          # 清 7 天前陈旧产物
pnpm clean:target --deep   # 连增量缓存一起清
```

依赖漏洞扫描（CI 单独 job 负责，日常不必本地跑，红了才复现）：
`cargo audit`（根 workspace + `apps/desktop/src-tauri`）、`pnpm audit --prod --audit-level high`。

## 代码架构

### 两个独立的 Cargo workspace

- **根 workspace**（`Cargo.toml`）：`binaries/mcpane-hub` + `crates/mcpane-*`，Hub 进程与所有领域库。
  `cargo fmt/clippy/test --workspace` 只覆盖这里。
- **`apps/desktop/src-tauri`**：桌面壳自己的 `[workspace]`，故意与根 workspace 隔离
  （依赖树差异巨大：Tauri 全家桶 vs. Hub 的 Axum/rmcp）。由 `pnpm tauri dev/build` 驱动，
  不会被根目录 `cargo test --workspace` 捎带跑到。

### 运行时拓扑：谁启动谁

桌面应用（`apps/desktop/src-tauri`）作为宿主，把 `mcpane-hub` 当 **sidecar** 托管：
自动拉起、崩溃退避重启、应用退出时优雅关停、发现已有实例则 adopt（不重复拉起）。
逻辑落在 `src-tauri/src/hub_process.rs`。Hub 对外暴露两个平面：

- **数据面**：`http://127.0.0.1:24860/mcp`（Streamable HTTP，MCP 协议本体，多客户端复用）；
  常量与路径拼接逻辑收敛在 `crates/mcpane-hub-protocol`（双端唯一事实源，避免 Hub 与
  桌面各自拼 `MCPANE_HOME`/socket 路径而漂移，历史上出过 UDS `SUN_LEN` 超限的坑）。
- **控制面**：UDS（macOS/Linux）/ Named Pipe（Windows），桌面 App 用它管理 Hub
  生命周期、下发配置变更，是应用内 IPC 而非 MCP 协议本身（ADR-007）。

只支持 stdio 的客户端（Claude Desktop 等）不直连数据面，而是经
`mcpane-hub bridge` 子命令（`binaries/mcpane-hub/src/bridge.rs`）：桥进程把 stdio 上的
JSON-RPC 原样转发到数据面，客户端侧不需要 node/npx。

### 领域 crates 怎么分工（`crates/mcpane-*`）

- `mcpane-config`：配置安全引擎——最小 Patch、原子写、快照、失败恢复/回滚。是所有
  「写用户配置文件」操作的必经层，不要绕过它直接改 JSON/TOML。
- `mcpane-adapter-{claude,codex,gemini,grok,hermes,openclaw,opencode}`：每个客户端一个
  adapter，负责把 MCPane 的领域模型翻译成该客户端原生配置文件格式（`~/.claude/settings.json`、
  `~/.codex/config.toml` 等），写入时走 `mcpane-config`。新增客户端支持从这里下手。
- `mcpane-storage`：SQLite 持久化（连接、Server、Workspace、Catalog、日志、用量；用量成本在读取时按
  `model_pricing` + 用户的 `model_pricing_override` 计算，见 ADR-026）。
  迁移文件在 `crates/mcpane-storage/migrations/*.sql`，靠 `build.rs` + sqlx `migrate!`
  嵌入二进制；跨 workspace 改了迁移文件后 `src-tauri` 侧可能读到旧的嵌入产物，
  发布前需要一次干净重编（`scripts/verify-embedded-migrations.sh` 校验）。
- `mcpane-hub-downstream`：下游 MCP Server 连接管理（stdio 子进程 + Streamable HTTP +
  SSE legacy），健康监控与自动恢复；`oauth` 模块负责远程 MCP 的 OAuth（令牌存
  `$MCPANE_HOME/mcp-oauth/` 文件，协议交给 rmcp 的 `auth`，ADR-024）。
- `mcpane-hub-catalog`：工具/Prompt/Resource 目录——命名空间、Revision、冲突检测，
  支撑 ADR-020 的工具暴露模式（平铺/网关/自动）。
- `mcpane-hub-protocol`：Hub 与桌面共享的控制面协议（路径、常量、wire 命令类型）。
- `mcpane-secrets`：OS 钥匙串/Credential Manager 封装，SQLite 里只存引用不存明文。
- `mcpane-provider`：Provider Profile（官方订阅 OAuth / 第三方中转站）的领域模型、存储、服务。
  另含 `mcpane://` 深链解析（`import_link`，ADR-023）与额度「是否用尽」判定（`quota`）。
- `mcpane-ccswitch`：从 cc-switch 安装读取 Provider 与 MCP Server，供一键迁移用。
- `mcpane-net`：出站网络——系统代理探测（Clash 等）、SOCKS、bypass 规则（ADR-017）。
- `mcpane-registry`：官方 MCP Registry 发现（ADR-025）。地址 → 检索词 → 候选（npm/pypi/远程），
  只读不执行；候选只用于预填「添加服务器」表单。
- `mcpane-sync`：WebDAV / S3 兼容的云同步，全量快照协议。
- `mcpane-usage`：纯函数，从 Claude/Codex 本地会话文件解析 token 用量，不依赖 DB。
- `mcpane-desktop-proxy`：本地路由（ADR-022），单端口按路径分命名空间：`/claude` `/codex` `/gemini`
  `/grok` 做路由接管与故障转移/熔断/临时接替/额度用尽后置/请求整流/流结果记账/超限错误归一
  （`overflow.rs`）/Codex→Chat 工具回合配对，`/claude-desktop` 是 Claude Desktop 网关；
  `codex_chat` / `codex_anthropic` / `claude_chat` / `claude_responses` 做跨协议转换。真实上游回归
  `tests/real_provider.rs`（`#[ignore]`，`MCPANE_REAL_KEY` 环境变量，须 `--test-threads=1`）。
- 前端：React 19 + Vite + Tailwind 4 + shadcn/ui（组件在 `src/components`，走
  `components.json` 生成配置）+ TanStack Query（数据获取/缓存）+ i18next（`src/lib/i18n.ts`）。
  页面按业务域拆在 `src/pages/`（Dashboard/Servers/Clients/Workspaces/Providers/Usage/
  Logs/Settings/Projects），领域无关的工具函数在 `src/lib/`。
- 前后端通信走 Tauri `invoke`（`src/lib/ipc.ts` 封装），后端命令面在
  `src-tauri/src/commands/*.rs`，按业务域一文件一模块（`servers.rs`/`workspace.rs`/
  `provider.rs`/`sync.rs`/`usage.rs` 等），`commands/mod.rs` 汇总注册。
- `src-tauri/src/envsync/`：环境变量凭据持久化（ADR-016），按平台落地方式不同
  （Unix 写 `$MCPANE_HOME/env.sh` + rc 注入；macOS `launchctl setenv` + LaunchAgent；
  Windows 写 `HKCU\Environment`；Linux `~/.config/environment.d`），失败分级明确
  （存储失败致命，rc/会话同步失败降级为 warning，凭据本身不丢）。
- `src-tauri/src/tray.rs`：系统托盘（含 Provider 快捷切换）；`window.rs` 控制主窗口显隐
  （注意：`show()` 必须放在 `RunEvent::Ready` 里调用，`.setup()` 里调用会被丢弃）。
- `src-tauri/src/hub_process.rs`：sidecar 生命周期；`config_watch.rs`：外部配置文件变更监听；
  `auto_sync.rs`：云同步的自动触发。

### 测试分层

- Rust：单测/集成测试随各 crate 放在 `crates/<name>/tests/`，跑 `cargo test -p <crate>`。
- 前端单测：`vitest`，与源文件同目录 `*.test.ts(x)`，跑 `pnpm --filter @mcpane/desktop test`。
- E2E：Playwright，`apps/desktop/e2e/*.spec.ts`，通过 mock Tauri IPC 驱动真实构建的前端
  （不起真实 Hub 进程），跑 `pnpm e2e`；mock 数据量要接近真实规模才能现形出分页/虚拟滚动类问题。
- `fixtures/`：Claude/Codex 配置样例（含故意损坏的配置），供 adapter 与迁移逻辑的测试复用。

## 深入前必读

- 协议双栈（新旧 MCP 版本并行）与「新字段绝不能漏进 legacy 响应」的三处门控先例——
  见 `AGENTS.md` 第 3 节，改协议面代码前必看，改完必跑 `tests/http_discover.rs`。
- 架构决策记录在 `docs/adr/ADR-XXX-*.md`，按 ADR 编号线性叠加，改动涉及既有 ADR 范围的
  设计问题时先读对应 ADR 里的取舍理由，别重新论证一遍已经拍板的结论。
- 依赖版本基线与偏差记录：`docs/architecture/DEPENDENCIES.md`。
