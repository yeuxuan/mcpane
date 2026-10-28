# MCPane 工程架构与执行清单

- 文档版本：V0.4（MCPane 品牌冻结版）
- 日期：2026-07-10（2026-07-12 状态对账）
- 状态：**规划快照，非进度看板**——Phase 0~6 主环已落地（实时状态见 README「状态」节），
  文内待办勾选停更；Provider 域模型已由 v1 演进为官方订阅/中转站二分（ADR-013），
  并补齐 Codex 预设 / 配置片段 / 通用配置 / 统一供应商（ADR-014），本文 §6.1 等 v1 描述以 ADR 为准
- 产品：MCPane
- 首发平台：macOS、Windows Native
- 首批客户端：Claude Code、Codex
- 一句话定位：Claude Code、Codex 与 MCP 工具的统一控制面
- 核心能力：Provider 配置管理、MCP Server 管理、统一 MCP Hub、Workspace、日志、配置备份与回滚

---


# 0. 执行前约束

## 0.1 品牌与工程标识冻结

后续代码、包名、配置、数据库、环境变量和 UI 统一使用以下标识：

| 项目 | 冻结值 |
|---|---|
| 正式产品名 | `MCPane` |
| 品牌解释 | `MCP + Pane / MCP Control Plane` |
| 桌面应用显示名 | `MCPane` |
| 仓库与根目录 | `mcpane` |
| Desktop 可执行标识 | `mcpane` |
| Hub 名称与二进制 | `MCP Hub` / `mcpane-hub` |
| 兼容 Bridge | `mcpane-bridge` |
| 客户端 MCP Entry | `mcpane` |
| Rust crate 前缀 | `mcpane-*` |
| TypeScript package 前缀 | `@mcpane/*` |
| 环境变量前缀 | `MCPANE_*` |
| 逻辑数据目录变量 | `MCPANE_HOME` |
| SQLite 文件 | `mcpane.db` |
| Keychain/Credential Service | `MCPane` |
| 默认上游地址 | `http://127.0.0.1:24860/mcp` |
| Tauri Bundle Identifier | `io.mcpane.desktop`（首次签名发布前最终确认，此后不得改变） |

默认应用数据目录：

```text
macOS   ~/Library/Application Support/MCPane/
Windows %APPDATA%\\MCPane\\
```

强制规则：

- 新代码不得继续使用任何旧项目代号；
- MCP Tool 的产品自带前缀统一为 `mcpane__`；
- 对外文案统一写作 `MCPane`，不得写成 `McPane`、`Mcpane` 或 `MCP Pane`； <!-- brand-scan: allow-prohibited-spellings -->
- CI 增加品牌标识扫描，防止包名、变量名和配置键发生混用；
- Bundle Identifier 在首次公开签名发布前可以调整，但发布后视为永久兼容标识。

---

## 0.2 给 Codex 的总指令

本文件是 MCPane P0 的工程执行基线。执行时遵守以下规则：

1. 严格按 Phase 和 Task 顺序执行，不得跳过 Gate。
2. 每次只完成一个 Task；不要顺手重构无关模块。
3. 每个 Task 完成后必须输出：
   - 修改文件列表；
   - 实现说明；
   - 执行过的测试及结果；
   - 新增的风险或未决问题；
   - 下一 Task 是否已满足前置条件。
4. 参考竞品的公开功能、交互和架构，不复制竞品源代码、函数、组件、目录或资源。
5. 官方 MCP 规范、Claude Code 官方文档、Codex 官方文档优先级高于竞品实现。
6. 依赖使用“执行当日最新稳定且相互兼容的版本”，不得使用 beta、RC、nightly 或 Git `main` 依赖；确认后锁定 `Cargo.lock` 与 `pnpm-lock.yaml`。
7. 不得静默覆盖 Claude Code 或 Codex 的已有配置。
8. 不得把 API Key、Bearer Token、OAuth Token、Cookie 或密码写入 SQLite、普通日志、测试快照或导出文件。
9. Rust 与 TypeScript 不允许以忽略类型错误、关闭校验或大量 `unwrap()` 的方式赶进度。
10. 任何配置写入功能必须同时提交失败恢复测试。
11. 任何 Hub 路由功能必须同时提交并发、鉴权和会话隔离测试。
12. 无法在当前机器完成真实 Claude Code/Codex 测试时，必须明确记录“未验证”，不得伪造通过结果。

---

# 1. 参考项目与参考边界

## 1.1 CC Switch

开源地址：

https://github.com/farion1231/cc-switch

重点参考：

- Tauri + React + Rust 的桌面分层；
- Provider 列表、添加、编辑、切换与托盘快捷操作；
- Claude Code、Codex 等客户端 Adapter 思路；
- MCP 配置导入、导出和同步流程；
- SQLite 本地状态；
- 配置备份、原子写入和恢复；
- 系统托盘、窗口生命周期、自动更新；
- 多客户端状态展示。

建议阅读位置：

- 根目录 `README.md` 的 Architecture Overview；
- `src-tauri/` 中 Commands、Services、Models、配置与数据库模块；
- Provider、MCP、tray、backup、config 相关模块；
- 前端 Provider/MCP 页面和 TanStack Query 使用方式。

MCPane 的改进原则：

- 客户端原生配置是“实际状态”，数据库不得用过期快照无条件覆盖它；
- 每次写入前重新读取文件并检查内容哈希；
- Codex TOML 使用语法树级最小修改，保留注释和未知字段；
- Windows Native、macOS、WSL Target 不混用路径或命令；
- Provider 管理与 MCP Hub 解耦，P0 不做模型协议转换和代理。

## 1.2 MCP Router

开源地址：

https://github.com/mcp-router/mcp-router

重点参考：

- 一个聚合 MCP Server 连接多个下游 MCP Server；
- Project、Workspace、Server 开关和 Tool 开关；
- 本地与远程 MCP Server 接入；
- `apps/electron/src/main/modules/mcp-server-runtime/aggregator-server.ts` 的聚合入口；
- `apps/cli/src/commands/connect.ts` 的 stdio → Streamable HTTP Bridge；
- `apps/cli/src/commands/serve.ts` 的 HTTP → stdio Bridge；
- `apps/cli/src/mcp-aggregator.ts` 的 Tool/Resource/Prompt 聚合思路；
- workspace、tool-catalog、mcp-server-manager、logger 等模块；
- 一键接入客户端和请求日志体验。

MCPane 的改进原则：

- 默认让 Claude Code 与 Codex 原生直连 Streamable HTTP，不默认依赖 Node CLI Bridge；
- Bridge 仅作为兼容后备；
- Bearer Token 在 HTTP 中间件验证，不把 Token 当普通 MCP 参数传递；
- Bearer Token 只标识客户端连接，`MCP-Session-Id` 单独标识协议会话；
- Tool 使用稳定命名空间，避免不同 Server 的同名 Tool 相互覆盖；
- Workspace 在会话创建时固定为 Catalog Snapshot，不能依赖所有客户端都支持即时工具热刷新；
- 正式 Hub 使用 Rust Sidecar，与桌面 UI 进程隔离。

## 1.3 官方技术依据

- MCP 稳定规范（P0 锁定 2025-11-25）  
  https://modelcontextprotocol.io/specification/2025-11-25
- MCP Streamable HTTP Transport  
  https://modelcontextprotocol.io/specification/2025-11-25/basic/transports
- MCP 官方 Rust SDK  
  https://github.com/modelcontextprotocol/rust-sdk
- Claude Code MCP 文档  
  https://code.claude.com/docs/en/mcp
- Codex MCP 文档  
  https://developers.openai.com/codex/mcp
- Codex 开源仓库  
  https://github.com/openai/codex
- Codex Streamable HTTP 客户端实现  
  https://github.com/openai/codex/blob/main/codex-rs/rmcp-client/src/rmcp_client.rs
- Codex Streamable HTTP 真实回环测试  
  https://github.com/openai/codex/blob/main/codex-rs/rmcp-client/tests/streamable_http_remote.rs
- Codex Session/重试恢复测试  
  https://github.com/openai/codex/blob/main/codex-rs/rmcp-client/tests/streamable_http_recovery.rs

优先级：

```text
MCPane 本文档
→ MCP 稳定规范
→ Claude Code / Codex 官方文档
→ MCP 官方 SDK
→ Codex 源码与测试
→ CC Switch / MCP Router 公开实现
```

---

# 2. 冻结的产品与架构决策

## 2.1 P0 范围

P0 必须实现：

- macOS 与 Windows Native；
- Claude Code 与 Codex 安装检测；
- Provider Profile 增删改查与切换；
- Claude Code JSON 配置安全写入；
- Codex TOML 配置安全写入；
- stdio MCP Server；
- Streamable HTTP MCP Server；
- 一个统一的 `http://127.0.0.1:{port}/mcp` 上游入口；
- Claude Code 与 Codex 同时连接同一 `/mcp`；
- 每个客户端独立 Bearer Token；
- MCP Session 隔离；
- Tool 聚合、命名空间、路由；
- Project、Workspace、Server/Tool 开关；
- Hub 请求日志；
- 配置 Diff、备份、原子写入、冲突检测、回滚；
- 一键把 MCP Hub 配置到 Claude Code 和 Codex；
- 系统托盘与 Hub 启停。

P0 不实现：

- 模型请求代理；
- Anthropic/OpenAI API 协议转换；
- Provider 自动故障转移；
- Token/费用统计；
- 云同步；
- Skills、Prompt、Agent、会话历史管理；
- MCP 商店；
- 团队云端控制台；
- Resources、Prompts、Sampling、Elicitation 的完整透明代理；
- Linux 桌面安装包。

## 2.2 MCP Hub 定位

MCP Hub 是 **Tools-first MCP Aggregation Hub**：

```text
Claude Code ───────────────┐
                           │
Codex ─────────────────────┼── Streamable HTTP
                           │
兼容客户端 ────────────────┘
                           │
                           ▼
              http://127.0.0.1:24860/mcp
                           │
                  mcpane-hub
                           │
        ┌──────────────────┼──────────────────┐
        │                  │                  │
        ▼                  ▼                  ▼
   stdio MCP        Streamable HTTP MCP   未来兼容层
```

P0 上游能力：

- initialize；
- notifications/initialized；
- tools/list；
- tools/call；
- ping；
- progress/cancellation 的必要转发；
- best-effort `notifications/tools/list_changed`。

P1 再扩展：

- Resources；
- Resource Templates；
- Prompts；
- Roots 完整转发；
- Sampling；
- Elicitation；
- Tasks；
- Legacy SSE 完整兼容。

## 2.3 数据面与控制面分离

### 数据面

供 Claude Code、Codex 调用：

```text
POST   /mcp
GET    /mcp
DELETE /mcp
```

约束：

- 仅监听 `127.0.0.1`，可选单独支持 `::1`；
- 不监听 `0.0.0.0`；
- 所有方法都执行认证；
- 校验 `Host`；
- `Origin` 存在时必须校验；
- 普通请求优先返回 `application/json`；
- 仅在通知、进度或多消息响应需要时使用 SSE；
- 支持 MCP 规范要求的 `Accept`、`MCP-Protocol-Version`、`MCP-Session-Id`。

### 控制面

供 Tauri Desktop 控制 Hub：

- macOS：Unix Domain Socket，权限 `0600`；
- Windows：Named Pipe，ACL 仅当前用户；
- 不通过公开 `/mcp` 暴露管理员操作；
- 支持 status、reload、shutdown、rotate-token、diagnostics；
- Hub 状态以事件方式推送给桌面端。

## 2.4 认证与会话模型

Bearer Token 和 MCP Session 必须分开：

```text
ClientConnection
├── id
├── client_type: claude | codex | custom
├── token_hash
├── token_prefix
├── default_workspace_id
└── enabled

McpSession
├── session_id
├── client_connection_id
├── protocol_version
├── workspace_id
├── catalog_revision
├── created_at
├── last_seen_at
└── expires_at
```

规则：

- Token 使用系统安全随机源生成至少 256 bit；
- 完整 Token 只在创建时展示一次；
- SQLite 只保存哈希和可识别前缀；
- Token 比较使用常量时间比较；
- 一个 Token 可以产生多个 MCP Session；
- Session 404 只影响该 Session；
- 撤销 Claude Token 不得影响 Codex；
- Session 不得以 URL 作为唯一标识。

### P0 客户端凭据传递策略

互操作 PoC 使用临时环境变量，不把测试 Token 写入客户端配置：

```text
Claude Code  MCPANE_CLAUDE_TOKEN
Codex        MCPANE_CODEX_TOKEN
```

正式接入规则：

- Claude Code 优先在 `headers.Authorization` 中使用环境变量展开：`Bearer ${MCPANE_CLAUDE_TOKEN}`；
- Claude Code 版本支持 `headersHelper` 时，可增加从系统凭据库动态取 Header 的增强模式；
- Codex 使用 `bearer_token_env_var = "MCPANE_CODEX_TOKEN"`；
- 默认禁止把完整 Bearer Token 直接写入 `~/.claude.json`、`.mcp.json` 或 `config.toml`；
- UI 必须展示环境变量配置状态，并提供终端命令、重新检测和 Token 轮换；
- P1 评估本地 OAuth，使 Codex/Claude 可将凭据交由各自安全凭据机制管理；
- 用户明确选择静态 Header 时，必须显示明文落盘风险并二次确认。

## 2.5 Workspace 生效策略

P0 采用 **Session-Pinned Workspace**：

1. 客户端 initialize；
2. Bearer Token 找到 ClientConnection；
3. 读取默认 Workspace；
4. 生成稳定 Tool Catalog Snapshot；
5. 将 Snapshot Revision 固定到当前 MCP Session；
6. 当前 Session 内保持工具名和 Schema 稳定。

切换 Workspace：

- 默认对“新 Session”生效；
- Hub 可以 best-effort 发送 `tools/list_changed`；
- 不把即时热刷新作为正确性的前提；
- UI 明确显示“当前会话”“下次连接生效”；
- 提供重新连接/重建 Session 操作。

## 2.6 Tool 命名与路由

不同 Server 的同名 Tool 必须避免冲突。

默认暴露名：

```text
{server_slug}__{original_tool_name}
```

示例：

```text
github__search
filesystem__search
slack__search
```

规则：

- 名称规范化后必须符合 MCP Tool name 约束；
- 超长时截断并增加稳定短哈希；
- 当前 Workspace 内必须唯一；
- 用户可设置 alias，但 alias 冲突时拒绝保存；
- 数据库保留 original_name、exposed_name、server_id、schema_hash；
- Catalog Revision 一旦绑定 Session，不得在该 Session 中悄悄改变路由目标。

## 2.7 下游运行策略

数据模型预留：

```text
Shared
PerClientConnection
PerUpstreamSession
```

P0 默认：

- 远程 Streamable HTTP：`Shared`；
- 本地 stdio：`PerClientConnection`；
- 明确有状态或项目隔离需求的 Server：允许 `PerUpstreamSession`。

每个实例必须有独立：

- process/transport handle；
- initialize 状态；
- Tool cache；
- timeout；
- crash counter；
- logs；
- cancellation map。

## 2.8 配置安全模型

任何客户端配置修改都执行：

```text
重新读取当前文件
→ 解析
→ 计算内容哈希
→ 生成最小 Patch
→ 展示 Diff
→ 创建 Snapshot
→ 写入同目录临时文件
→ fsync
→ 语法与业务验证
→ 原子替换
→ 重新读取验证
→ 写入操作历史
```

强制规则：

- 不允许字符串搜索替换 JSON/TOML；
- 不允许整体重建 Codex `config.toml`；
- JSON 未知字段必须保留；
- TOML 未知字段、注释和非目标格式尽量保留；
- 写入前文件哈希变化时中止并提示冲突；
- 写入失败时恢复；
- 回滚前再次备份当前文件；
- 数据库是期望状态和历史，不是覆盖原生配置的唯一事实来源。

---

# 3. 确认的技术栈

## 3.1 桌面与前端

以下是 **2026-07-10 的稳定版基线快照**。TASK-001 必须再次查询 registry；只允许升级到新的稳定 patch/minor，并在兼容测试通过后更新锁文件。大版本变化必须新增 ADR。

| 类别 | 方案 / 基线版本 |
|---|---|
| 桌面框架 | Tauri 2.11.x（基线：core 2.11.5，CLI 2.11.4） |
| 前端 | React 19.2.7 |
| 类型检查 | TypeScript 7.0.2 |
| 构建 | Vite 8.1.4 |
| 包管理 | pnpm 11.11.0 |
| CSS | Tailwind CSS 4.3.2 |
| UI | shadcn/ui 当前稳定版 + Base UI 1.6.0 |
| 路由 | TanStack Router 1.170.17 |
| 异步状态 | TanStack Query 5.101.2 |
| 表格 | TanStack Table 8.21.3 |
| 本地 UI 状态 | Zustand 5.0.14 |
| 表单 | React Hook Form 7.81.0 |
| Schema | Zod 4.4.3 |
| 国际化 | i18next 26.3.6 |
| 图标 | Lucide React 1.24.0 |
| 格式与 Lint | Biome 2.5.3 |
| 测试 | Vitest 4.1.10 + Testing Library + Playwright 1.61.1 |

TypeScript 7 兼容策略：

- 使用 TypeScript 7 完成类型检查；
- 默认使用 Biome，不把依赖旧 Compiler API 的 Lint 工具设为启动阻塞项；
- 某个必需工具仍依赖旧 Compiler API 时，按 TypeScript 官方兼容方案隔离使用，不允许污染主类型检查链路；
- 不得为了兼容插件而静默关闭 `strict`、`noUncheckedIndexedAccess` 等核心检查；
- 版本偏离本表时，必须在 `docs/architecture/DEPENDENCIES.md` 写明原因和验证结果。

## 3.2 Rust 与后端

| 类别 | 方案 |
|---|---|
| Rust | 1.97.0 stable，Edition 2024 |
| 异步运行时 | Tokio |
| HTTP Server | Axum |
| HTTP Client | reqwest |
| 中间件 | Tower / tower-http |
| MCP SDK | 官方 `rmcp` 最新稳定 crates.io 版本（基线 release tag：2.2.0） |
| SQLite | SQLx |
| JSON | serde / serde_json |
| TOML | toml_edit |
| Secret | keyring |
| 文件监听 | notify |
| 日志 | tracing / tracing-subscriber |
| 错误 | thiserror + anyhow（仅应用边界） |
| Schema | schemars |
| ID | uuid |
| 时间 | time |
| Hash | sha2 或 blake3 |
| 常量时间比较 | subtle |
| 进程 | tokio::process |
| 系统信息 | sysinfo |
| 测试 | cargo test；可选 cargo-nextest |

依赖规则：

- `rmcp` 必须来自 crates.io 稳定发行版；基线参考 release tag 为 `rmcp-v2.2.0`；
- 禁止依赖 MCP SDK Git `main`；
- MCP P0 锁定协议版本 `2025-11-25`；
- 对未来 `2026-07-28` 规范只做 Adapter 预留，不在 P0 追随 beta SDK。

## 3.3 数据与密钥

- SQLite 开启 WAL；
- 所有 Schema 变更通过 Migration；
- macOS 使用 Keychain；
- Windows 使用 Credential Manager；
- 数据库仅保存 Secret Reference；
- 配置快照在保存前脱敏；
- 日志默认不保存完整 Tool 输入和输出；
- 支持按字段级规则脱敏。

---

# 4. 进程与模块架构

```text
┌──────────────────────────────────────────────────────────┐
│ mcpane-desktop                                      │
│ Tauri 2 + React                                         │
│                                                          │
│ UI                                                       │
│ ├── Dashboard                                            │
│ ├── Clients / Targets                                    │
│ ├── Providers                                            │
│ ├── MCP Servers                                          │
│ ├── Projects / Workspaces                                │
│ ├── Connections                                          │
│ ├── Logs / History                                       │
│ └── Settings                                             │
│                                                          │
│ Rust App Core                                            │
│ ├── Config Adapters                                      │
│ ├── Provider Service                                     │
│ ├── Storage / Secret Store                               │
│ ├── Target Detection                                     │
│ └── Hub Supervisor                                       │
└───────────────────────┬──────────────────────────────────┘
                        │ UDS / Named Pipe
┌───────────────────────▼──────────────────────────────────┐
│ mcpane-hub                                          │
│                                                          │
│ Upstream Streamable HTTP Server                          │
│ ├── Auth Middleware                                      │
│ ├── Session Manager                                      │
│ └── Capability Negotiation                               │
│                                                          │
│ Aggregation Core                                         │
│ ├── Server Registry                                      │
│ ├── Tool Catalog                                         │
│ ├── Workspace Router                                     │
│ ├── Policy Engine                                        │
│ └── Request Router                                       │
│                                                          │
│ Downstream Clients                                       │
│ ├── stdio                                                │
│ └── Streamable HTTP                                      │
│                                                          │
│ Runtime / Observability                                  │
│ ├── Process Supervisor                                   │
│ ├── Health Check                                         │
│ ├── Retry / Timeout / Cancellation                       │
│ ├── Request Log                                          │
│ └── Redaction                                            │
└──────────────────────────────────────────────────────────┘

可选：
mcpane-bridge
客户端 stdio ↔ MCP Hub Streamable HTTP
```

为什么 Hub 必须是独立进程：

- Hub 崩溃不拖垮 UI；
- UI 窗口关闭后 Hub 仍可运行；
- 可以单独重启、升级和诊断；
- MCP 子进程生命周期与 WebView 生命周期隔离；
- 未来可独立开机启动。

---

# 5. Monorepo 目录

> **实现说明（2026-07-19）**：下方为**规划期**的目标目录树，非最终布局。P0 落地时
> crate 数量从这里的 ~16 个收敛到 **10 个**——`mcpane-domain` / `mcpane-contracts` /
> `mcpane-platform` / `mcpane-hub-upstream` / `mcpane-hub-router` / `mcpane-hub-runtime` /
> `mcpane-control-ipc` / `mcpane-observability` 均未作为独立 crate 落地，其职责或内联、
> 或折叠进现有 crate；`binaries/` 下只有 `mcpane-hub`（含测试用 `mcpane-devserver`）。
> **以 `docs/architecture/DEPENDENCIES.md` 和实际 `crates/` 目录为准。**

```text
mcpane/
├── apps/
│   └── desktop/
│       ├── src/                         # React UI
│       ├── src-tauri/                   # Tauri app shell
│       └── package.json
│
├── binaries/
│   ├── mcpane-hub/
│   ├── mcpane-cli/                    # 状态、doctor、auth/header helper
│   └── mcpane-bridge/                 # 仅兼容不支持 HTTP 的客户端
│
├── crates/
│   ├── mcpane-domain/
│   ├── mcpane-contracts/
│   ├── mcpane-storage/
│   ├── mcpane-secrets/
│   ├── mcpane-config/
│   ├── mcpane-adapter-claude/
│   ├── mcpane-adapter-codex/
│   ├── mcpane-platform/
│   ├── mcpane-provider/
│   ├── mcpane-hub-upstream/
│   ├── mcpane-hub-downstream/
│   ├── mcpane-hub-catalog/
│   ├── mcpane-hub-router/
│   ├── mcpane-hub-runtime/
│   ├── mcpane-control-ipc/
│   └── mcpane-observability/
│
├── packages/
│   ├── ui/
│   └── generated-contracts/
│
├── tests/
│   ├── interop/
│   ├── integration/
│   ├── chaos/
│   └── security/
│
├── fixtures/
│   ├── claude-configs/
│   ├── codex-configs/
│   ├── mcp-servers/
│   └── corrupt-configs/
│
├── experiments/
│   └── streamable-http-oracle-ts/      # 仅在排查 SDK 差异时使用
│
├── docs/
│   ├── adr/
│   ├── architecture/
│   ├── product/
│   ├── interop/
│   ├── competitors/
│   └── agent-tasks/
│
├── Cargo.toml
├── Cargo.lock
├── pnpm-workspace.yaml
├── pnpm-lock.yaml
├── rust-toolchain.toml
├── package.json
├── biome.json
└── README.md
```

---

# 6. 核心领域模型与数据表

## 6.1 领域对象

- `Target`：MacNative、WindowsNative、WslDistro；
- `ClientInstallation`：Claude Code/Codex 安装及版本；
- `ProviderProfile`：Provider 配置（v2 起按客户端归属、官方订阅/中转站二分，见 ADR-013）；
- `McpServerDefinition`：stdio/Streamable HTTP 配置；
- `McpRuntimeInstance`：某个下游运行实例；
- `McpToolDefinition`：原始 Tool Schema；
- `ToolRoute`：暴露名到下游 Server/Tool 的稳定映射；
- `Project`：Server 长期分组；
- `Workspace`：启用的 Server、Tool 和权限集合；
- `ClientConnection`：Claude/Codex/自定义客户端连接与 Token；
- `McpSession`：单次协议会话；
- `CatalogRevision`：某 Workspace 的稳定工具快照；
- `ConfigSnapshot`：配置文件备份；
- `ConfigOperation`：一次写入、验证和回滚记录。

## 6.2 P0 SQLite 表

```text
app_settings
targets
client_installations
provider_profiles
provider_client_mappings
secret_references
mcp_servers
mcp_server_secret_refs
mcp_tools
projects
project_servers
workspaces
workspace_servers
workspace_tools
client_connections
mcp_sessions
catalog_revisions
tool_routes
runtime_instances
config_snapshots
config_operations
hub_request_logs
schema_migrations
```

索引至少覆盖：

- `client_connections(token_hash)` unique；
- `mcp_sessions(session_id)` unique；
- `tool_routes(catalog_revision_id, exposed_name)` unique；
- `mcp_tools(server_id, original_name)`；
- `hub_request_logs(timestamp, client_connection_id, server_id, tool_id)`；
- `config_operations(created_at, target_id, client_type)`。

---

# 7. 执行阶段与任务清单

# Phase 0：工程初始化与决策冻结

## TASK-000：建立仓库和开发规则

- [ ] 初始化 Git 仓库；
- [ ] 创建 Cargo Workspace；
- [ ] 创建 pnpm Workspace；
- [ ] 创建 `AGENTS.md`，写入本文件第 0 节规则；
- [ ] 创建 `CONTRIBUTING.md`；
- [ ] 创建 `SECURITY.md`；
- [ ] 创建 `docs/branding/NAMING.md`，记录第 0.1 节冻结标识；
- [ ] 创建 `docs/competitors/REFERENCES.md`，记录两个参考仓库与指定文件；
- [ ] 配置 CI 品牌标识扫描；
- [ ] 配置 `.editorconfig`、`.gitignore`；
- [ ] 配置 Rust fmt/clippy；
- [ ] 配置 Biome；
- [ ] 配置 GitHub Actions macOS/Windows 基础矩阵；
- [ ] 禁止提交 `.env`、Token、用户配置和本地数据库。

验收：

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
pnpm install --frozen-lockfile
pnpm typecheck
pnpm lint
```

## TASK-001：依赖兼容性与版本锁定

- [ ] 安装 Rust 1.97.0 stable、Edition 2024；
- [ ] 以 Tauri 2.11.x 为基线，查询并选择最新兼容 patch；
- [ ] 以 React 19.2.7、TypeScript 7.0.2、Vite 8.1.4 为基线；
- [ ] 以 pnpm 11.11.0、Tailwind 4.3.2 为基线；
- [ ] 初始化 shadcn/ui，并确认 Base UI 1.6.x；
- [ ] 验证 TanStack、Zod、RHF、Biome 与 TypeScript 7；
- [ ] 查询 `rmcp` crates.io 最新稳定版本，并与 `rmcp-v2.2.0` release tag 对照；
- [ ] 检查所有新版本是否为 stable，不接受 beta、RC、nightly 或 Git branch；
- [ ] 记录 `docs/architecture/DEPENDENCIES.md`；
- [ ] 提交 lockfiles。

验收：

- React 页面可启动；
- Tauri dev 可启动；
- TS7 类型检查通过；
- UI 测试通过；
- Rust MCP 最小示例编译通过；
- 无 beta/RC/nightly/Git 依赖。

## TASK-002：创建 ADR

必须创建：

- [ ] ADR-001：Tauri Desktop + Rust Hub Sidecar；
- [ ] ADR-002：统一 Streamable HTTP `/mcp`；
- [ ] ADR-003：Bearer Token 与 MCP Session 分离；
- [ ] ADR-004：Session-Pinned Workspace；
- [ ] ADR-005：配置最小 Patch 与原子写入；
- [ ] ADR-006：Tool 命名空间；
- [ ] ADR-007：控制面使用 UDS/Named Pipe；
- [ ] ADR-008：竞品仅作功能和架构参考；
- [ ] ADR-009：P0 锁定 MCP 2025-11-25；
- [ ] ADR-010：TypeScript 7 与 Compiler API 兼容策略；
- [ ] ADR-011：MCPane 品牌、包名、环境变量与持久化标识；
- [ ] ADR-012：Claude/Codex Bearer Token 凭据传递与明文落盘禁止策略（Superseded）；
- [x] ADR-015：客户端凭据双模式投递——EnvVar 默认 / Inline 直写可选（取代 ADR-012）；
- [x] ADR-016：envsync 代管环境变量持久化——env 模式一键化（修订 ADR-015 §1/§3）。
- [x] ADR-017：全局出网代理——MCPane 自己联网走用户的代理。
- [x] ADR-018：客户端矩阵扩到八家——官方项落库播种、additive 语义、
      JSON5/YAML 文档模型、配置目录覆盖、可选维度按字段存在性门控
      （修订 ADR-013 的官方项模型；YAML 不保注释是显式例外）。

---

# Phase 1：最高优先级互操作 Gate

> 在本阶段完成前，不开发完整 Provider 页面和完整 MCP 管理 UI。

## TASK-100：最小 Streamable HTTP Hub

创建最小 `mcpane-hub`：

- [ ] 监听 `127.0.0.1:24860`；
- [ ] 单一 `/mcp` Endpoint；
- [ ] 支持 POST、GET、DELETE；
- [ ] 校验 Host；
- [ ] Origin 存在时校验；
- [ ] Bearer Token 中间件；
- [ ] 注册两个测试 Token：Claude、Codex；
- [ ] 首轮使用无状态模式，作为 MCP Router 行为基线；
- [ ] 暴露 `mcpane__echo`；
- [ ] 暴露 `mcpane__connection_info`；
- [ ] 验证所有内建 Tool 均使用稳定 `mcpane__` 前缀；
- [ ] 记录脱敏协议日志；
- [ ] 提供仅控制面可见的健康状态。

`connection_info` 返回：

```json
{
  "clientConnectionId": "client_xxx",
  "clientType": "claude|codex",
  "requestId": "req_xxx",
  "sessionId": null,
  "protocolVersion": "2025-11-25",
  "serverVersion": "0.0.1"
}
```

## TASK-101：Codex 直连验证

- [ ] 使用 Codex 原生 Streamable HTTP；
- [ ] URL 为 `http://127.0.0.1:24860/mcp`；
- [ ] Bearer Token 从环境变量读取；
- [ ] 完成 initialize；
- [ ] 完成 tools/list；
- [ ] 调用 echo；
- [ ] 调用 connection_info；
- [ ] 记录 Codex 版本、OS、完整脱敏请求顺序；
- [ ] 保存报告到 `docs/interop/codex-direct-http.md`。

不得仅使用 SDK 模拟 Codex，必须运行真实 Codex 发布版本。

## TASK-102：Claude Code 直连验证

- [ ] 使用 Claude Code 原生 HTTP MCP；
- [ ] URL 与 Codex 完全相同；
- [ ] 使用独立 Claude Token；
- [ ] 完成 initialize；
- [ ] 完成 tools/list；
- [ ] 调用 echo；
- [ ] 调用 connection_info；
- [ ] 记录 Claude Code 版本、OS、完整脱敏请求顺序；
- [ ] 保存报告到 `docs/interop/claude-direct-http.md`。

不得仅使用 SDK 模拟 Claude Code，必须运行真实 Claude Code 发布版本。

## TASK-103：双客户端同时连接

必须在 Claude Code 与 Codex 同时运行时验证：

- [ ] 两者连接同一 `/mcp`；
- [ ] 两者使用不同 Token；
- [ ] 同时 initialize；
- [ ] 同时 tools/list；
- [ ] 各自连续调用 echo 20 次；
- [ ] 并发调用不串线；
- [ ] JSON-RPC ID 不串线；
- [ ] Token 身份不串线；
- [ ] 一个慢请求不阻塞另一个客户端；
- [ ] 关闭 Claude 不影响 Codex；
- [ ] 关闭 Codex 不影响 Claude；
- [ ] 撤销 Claude Token 后 Codex 继续正常；
- [ ] 撤销 Codex Token 后 Claude 继续正常。

报告：

`docs/interop/claude-codex-same-endpoint.md`

## TASK-104：有状态 Session 验证

在无状态 Gate 通过后：

- [ ] 开启 `MCP-Session-Id`；
- [ ] 每次 initialize 产生独立 Session；
- [ ] 同一 Token 可有多个 Session；
- [ ] POST/GET/DELETE 都验证 Session；
- [ ] 无效 Session 返回 404；
- [ ] Codex 在 404 后能重新初始化；
- [ ] Claude Session 失效不影响 Codex；
- [ ] Codex Session 失效不影响 Claude；
- [ ] Session 超时可配置；
- [ ] Session 清理不泄漏 pending request。

## TASK-105：响应形式兼容

覆盖：

- [ ] initialize 返回 `application/json`；
- [ ] tools/list 返回 `application/json`；
- [ ] 短 tools/call 返回 `application/json`；
- [ ] Notification 返回 202 无 body；
- [ ] GET 支持 SSE；
- [ ] 必要时 POST 可返回 SSE；
- [ ] 客户端 Accept Header 不完整时返回明确错误；
- [ ] 协议版本不支持时返回明确错误；
- [ ] `MCP-Protocol-Version` 正确传递和校验。

## GATE-1 验收

Gate 通过条件：

```text
Claude Code 与 Codex
使用不同 Bearer Token
同时通过同一个 /mcp
完成 initialize → tools/list → tools/call
且鉴权、并发、Session 互不干扰
```

若某客户端原生 HTTP 未通过：

1. 保留失败日志；
2. 使用 MCP Router 的 Bridge 思路实现最小 `mcpane-bridge`；
3. 只为失败客户端启用 stdio Bridge；
4. Hub 核心仍保持单一 Streamable HTTP；
5. 在 ADR 中记录失败版本和原因；
6. 不得把 Bridge 变成默认架构，除非两个客户端真实测试均证明需要。

---

# Phase 2：配置安全引擎与客户端 Adapter

## TASK-200：配置安全核心

- [ ] 通用 `ConfigDocument`；
- [ ] 内容哈希；
- [ ] mtime 与文件身份检查；
- [ ] Patch Plan；
- [ ] 结构化 Diff；
- [ ] 临时文件；
- [ ] fsync；
- [ ] 原子 rename；
- [ ] 自动 Snapshot；
- [ ] 写入后重新读取验证；
- [ ] 自动恢复；
- [ ] 操作历史；
- [ ] 外部修改监听；
- [ ] 冲突错误类型。

Fixtures：

- [ ] 空 JSON；
- [ ] 带未知字段 JSON；
- [ ] 损坏 JSON；
- [ ] 空 TOML；
- [ ] 带注释 TOML；
- [ ] 带未知 Table TOML；
- [ ] 损坏 TOML；
- [ ] 只读文件；
- [ ] 写入过程中外部修改；
- [ ] rename 失败；
- [ ] 磁盘写入失败模拟。

## TASK-201：Claude Code Adapter

- [ ] 检测 `claude`；
- [ ] 获取版本；
- [ ] 发现用户配置；
- [ ] 发现项目配置；
- [ ] 识别 MCP scope；
- [ ] 读取 Provider 相关配置；
- [ ] 读取 MCP 配置；
- [ ] 生成 Provider Patch；
- [ ] 生成 Hub MCP Patch；
- [ ] 删除 MCPane 管理的 Hub Entry；
- [ ] 保留其他 MCP Server；
- [ ] 保留未知字段；
- [ ] 支持环境变量 Header；
- [ ] 配置验证与回滚。

## TASK-202：Codex Adapter

- [ ] 检测 `codex`；
- [ ] 获取版本；
- [ ] 发现 `~/.codex/config.toml`；
- [ ] 发现项目 `.codex/config.toml`；
- [ ] 使用 `toml_edit`；
- [ ] 读取 Provider 相关配置；
- [ ] 读取 MCP 配置；
- [ ] 生成 Provider Patch；
- [ ] 生成 `[mcp_servers.mcpane]` Patch；
- [ ] 设置 `bearer_token_env_var`；
- [ ] 保留注释、未知表和非目标顺序；
- [ ] 删除 MCPane Entry 时不影响其他 Server；
- [ ] 配置验证与回滚。

## TASK-203：Target 模型

- [ ] `MacNative`；
- [ ] `WindowsNative`；
- [ ] 数据模型预留 `WslDistro`；
- [ ] 命令必须在目标环境执行；
- [ ] 路径必须绑定 Target；
- [ ] 不做隐式 Windows/Linux 路径转换；
- [ ] P0 UI 只开放 MacNative/WindowsNative；
- [ ] WSL 在独立里程碑启用。

## GATE-2 验收

- 未知 JSON 字段保留率 100%；
- Codex TOML 注释和未知表不丢失；
- 每次写入都有 Diff 和 Snapshot；
- 外部修改后不会静默覆盖；
- 模拟失败后原配置完整；
- 任意历史版本可回滚。

---

# Phase 3：Provider 管理

## TASK-300：Provider Domain

- [ ] Provider Profile；
- [ ] Claude Mapping；
- [ ] Codex Mapping；
- [ ] Secret Reference；
- [ ] Base URL；
- [ ] Model；
- [ ] Headers；
- [ ] Environment Variables；
- [ ] 启用状态；
- [ ] 排序；
- [ ] 导入/导出 Schema Version。

## TASK-301：Secret Store

- [ ] macOS Keychain；
- [ ] Windows Credential Manager；
- [ ] 创建、读取、更新、删除；
- [ ] DB 只保存 Reference；
- [ ] IPC 不返回完整 Secret；
- [ ] UI 默认脱敏；
- [ ] 导出默认排除 Secret；
- [ ] 日志脱敏测试。

## TASK-302：Provider Service

- [ ] 添加；
- [ ] 编辑；
- [ ] 复制；
- [ ] 删除；
- [ ] 排序；
- [ ] 连通性测试；
- [ ] Claude 一键应用；
- [ ] Codex 一键应用；
- [ ] 两者同时应用；
- [ ] 应用前 Diff；
- [ ] 失败回滚；
- [ ] 当前 Provider 识别。

首批模板：

- [ ] Anthropic；
- [ ] OpenAI；
- [ ] OpenRouter；
- [ ] Azure OpenAI；
- [ ] AWS Bedrock；
- [ ] Google Vertex；
- [ ] Custom。

P0 不实现 API 协议转换。

---

# Phase 4：生产级 MCP Hub

## TASK-400：Hub Control Plane

- [ ] macOS UDS；
- [ ] Windows Named Pipe；
- [ ] 当前用户权限限制；
- [ ] Hub status；
- [ ] start/stop/restart；
- [ ] reload config；
- [ ] rotate token；
- [ ] diagnostics；
- [ ] event subscription；
- [ ] protocol versioning；
- [ ] Desktop 与 Hub 版本不兼容提示。

## TASK-401：Client Connection 与 Token

- [ ] 创建 Connection；
- [ ] 生成 256-bit Token；
- [ ] Token Hash；
- [ ] Token Prefix；
- [ ] Token 只显示一次；
- [ ] 撤销；
- [ ] 轮换；
- [ ] 最后使用时间；
- [ ] 绑定默认 Workspace；
- [ ] 独立权限；
- [ ] 认证失败速率限制。

## TASK-402：下游 stdio Client

- [ ] 不通过 shell 启动默认命令；
- [ ] command/args/env/cwd；
- [ ] Runtime 检测；
- [ ] stdin/stdout JSON-RPC；
- [ ] stderr 独立捕获；
- [ ] initialize；
- [ ] tools/list；
- [ ] tools/call；
- [ ] timeout；
- [ ] cancellation；
- [ ] crash detection；
- [ ] 指数退避；
- [ ] crash-loop 熔断；
- [ ] 进程树清理；
- [ ] Windows Job Object 或等价清理；
- [ ] macOS process group 清理；
- [ ] Secret env 注入；
- [ ] stderr 大量输出压力测试。

Runtime：

- node；
- npm；
- npx；
- python；
- python3；
- uv；
- uvx；
- docker；
- 自定义绝对路径。

## TASK-403：下游 Streamable HTTP Client

- [ ] initialize；
- [ ] POST JSON-RPC；
- [ ] GET/SSE；
- [ ] DELETE Session；
- [ ] Session ID；
- [ ] Bearer Token；
- [ ] static headers；
- [ ] Secret headers；
- [ ] OAuth 数据模型预留；
- [ ] timeout；
- [ ] retry；
- [ ] reconnect；
- [ ] 404 Session 重建；
- [ ] TLS 错误分类；
- [ ] tools/list；
- [ ] tools/call；
- [ ] 401/403 明确错误；
- [ ] response size limit；
- [ ] SSE frame limit。

P0 可先支持 Bearer/Header；OAuth 完整交互可列为 P1，但数据模型不得阻塞未来扩展。

## TASK-404：Server Registry 与 Runtime Policy

- [ ] 注册/注销 Server；
- [ ] Shared；
- [ ] PerClientConnection；
- [ ] PerUpstreamSession；
- [ ] 状态机：Stopped/Starting/Ready/Degraded/Failed；
- [ ] 健康检查；
- [ ] Tool Cache；
- [ ] Catalog 更新事件；
- [ ] 单个 Server 崩溃不影响 Hub；
- [ ] Server 启停；
- [ ] 重启；
- [ ] 自动恢复；
- [ ] 并发限制；
- [ ] 每 Server 队列。

## TASK-405：Tool Catalog

- [ ] 并行读取下游 tools/list；
- [ ] Schema 标准化；
- [ ] Schema Hash；
- [ ] 名称规范化；
- [ ] 稳定命名空间；
- [ ] 冲突检测；
- [ ] alias；
- [ ] Catalog Revision；
- [ ] Snapshot；
- [ ] 缓存；
- [ ] Server 下线处理；
- [ ] Catalog Diff；
- [ ] 预热 Workspace。

## TASK-406：Workspace Router

- [ ] Project CRUD；
- [ ] Workspace CRUD；
- [ ] Workspace Server 开关；
- [ ] Workspace Tool 开关；
- [ ] Token → Workspace；
- [ ] Session → Catalog Revision；
- [ ] tools/list 只返回允许项；
- [ ] tools/call 再次执行权限校验；
- [ ] 禁用 Tool 的直接调用返回明确错误；
- [ ] route map；
- [ ] timeout；
- [ ] cancellation；
- [ ] Workspace 切换对新 Session 生效；
- [ ] best-effort list_changed；
- [ ] 切换审计日志。

## TASK-407：日志与诊断

记录：

- request_id；
- 时间；
- client_connection_id；
- session_id；
- workspace_id；
- server_id；
- tool_id；
- exposed_name；
- duration；
- success/failure；
- error category；
- request/response size；
- retry count。

要求：

- [ ] Authorization 脱敏；
- [ ] Token 脱敏；
- [ ] Cookie 脱敏；
- [ ] 常见 Secret 字段脱敏；
- [ ] Tool 参数默认仅保存摘要；
- [ ] 响应截断；
- [ ] 日志保留天数；
- [ ] 最大容量；
- [ ] 导出诊断包；
- [ ] 诊断包二次 Secret 扫描。

## GATE-3 验收

必须通过：

1. 一个 stdio MCP；
2. 一个远程 Streamable HTTP MCP；
3. 两者同时聚合；
4. Claude/Codex 仍连接同一 `/mcp`；
5. 两客户端可并发调用不同下游；
6. 两个 Server 含同名 Tool 时路由正确；
7. 一个 Server 崩溃不影响另一个；
8. Token 撤销和 Workspace 权限隔离正确；
9. Hub 重启后可恢复；
10. 日志不含 Secret。

---

# Phase 5：桌面 UI

## TASK-500：应用壳

- [ ] Tauri 单实例；
- [ ] 主窗口；
- [ ] 窗口位置与尺寸恢复；
- [ ] 左侧导航；
- [ ] 深色/浅色/跟随系统；
- [ ] 简体中文/英文；
- [ ] 键盘导航；
- [ ] Error Boundary；
- [ ] Toast；
- [ ] 空状态；
- [ ] Loading/Skeleton；
- [ ] 系统托盘；
- [ ] 退出行为；
- [ ] Hub 状态栏。

页面：

```text
Dashboard
Clients
Providers
MCP Servers
Projects
Workspaces
Connections
Logs
History
Settings
```

## TASK-501：Dashboard

- [ ] Claude Code 状态；
- [ ] Codex 状态；
- [ ] 当前 Provider；
- [ ] Hub 状态；
- [ ] `/mcp` 地址；
- [ ] 连接客户端；
- [ ] 运行中 Server；
- [ ] 当前 Workspace；
- [ ] 最近错误；
- [ ] 快捷操作。

## TASK-502：Provider UI

- [ ] 列表；
- [ ] 预设；
- [ ] 新建/编辑/复制/删除；
- [ ] Secret 输入；
- [ ] 连通性测试；
- [ ] Claude/Codex 映射；
- [ ] Diff Preview；
- [ ] 一键应用；
- [ ] 回滚；
- [ ] 托盘切换。

## TASK-503：MCP Server UI

- [ ] Server 列表；
- [ ] stdio 表单；
- [ ] HTTP 表单；
- [ ] JSON 导入；
- [ ] 从 Claude 导入；
- [ ] 从 Codex 导入；
- [ ] Runtime 检测；
- [ ] Connection Test；
- [ ] Tool 列表；
- [ ] Server 启停；
- [ ] 状态与错误；
- [ ] 日志；
- [ ] 导入/导出。

## TASK-504：Project/Workspace UI

- [ ] Project 列表；
- [ ] Workspace 列表；
- [ ] Server 开关；
- [ ] Tool 开关；
- [ ] 搜索；
- [ ] 批量启停；
- [ ] Tool 风险信息；
- [ ] Catalog Revision；
- [ ] 当前 Session 状态；
- [ ] 新会话生效提示；
- [ ] 重连操作。

## TASK-505：Connection UI

- [ ] 创建 Claude Connection；
- [ ] 创建 Codex Connection；
- [ ] Token 只显示一次；
- [ ] 一键复制安全配置；
- [ ] 一键写入 Claude；
- [ ] 一键写入 Codex；
- [ ] 测试连接；
- [ ] 撤销；
- [ ] 轮换；
- [ ] 解绑；
- [ ] 恢复原配置。

## TASK-506：Logs/History/Settings

- [ ] 请求日志筛选；
- [ ] Server 日志；
- [ ] 配置操作历史；
- [ ] Diff；
- [ ] 一键回滚；
- [ ] 数据目录；
- [ ] Hub 端口；
- [ ] 自动启动；
- [ ] 日志保留；
- [ ] 更新设置；
- [ ] 诊断包导出。

---

# Phase 6：一键接入与系统托盘

## TASK-600：Claude Code 一键接入

流程（凭据双模式见 ADR-015；两种都一键完成，见 ADR-016）：

1. 检测 Claude Code 版本与可用 MCP scope；
2. 读取最新配置；
3. 凭据模式二选一（apply 均自动轮换/新建连接发放 Token，零手动步骤）：
   EnvVar（默认）写 `MCPANE_CLAUDE_TOKEN` 环境变量引用，明文由 envsync 持久化
   到 MCPane 数据目录并自动注入系统（ADR-016）；Inline（「直接写入 Token」
   可选）把明文写入 `headers.Authorization`；
4. 生成 `mcpane` HTTP MCP Entry；
5. 展示 Diff（Inline 预览用占位串，写入后 diff 对 Token 脱敏为前缀）；
6. Snapshot；
7. 原子写入；
8. 按模式做配置验证（双源互斥）；
9. 失败回滚。

目标配置语义（EnvVar 默认形态）：

```json
{
  "mcpServers": {
    "mcpane": {
      "type": "http",
      "url": "http://127.0.0.1:24860/mcp",
      "headers": {
        "Authorization": "Bearer ${MCPANE_CLAUDE_TOKEN}"
      }
    }
  }
}
```

具体作用域和落盘位置以执行时 Claude Code 官方文档及用户选择为准。

## TASK-601：Codex 一键接入

流程同上。EnvVar 默认形态（Inline 可选形态改写 `http_headers.Authorization`
并移除 `bearer_token_env_var`）：

```toml
[mcp_servers.mcpane]
url = "http://127.0.0.1:24860/mcp"
bearer_token_env_var = "MCPANE_CODEX_TOKEN"
enabled = true
required = false # Hub 停机/变量未设置时不阻塞 Codex 会话（ADR-015）
startup_timeout_sec = 10
tool_timeout_sec = 60
```

要求：

- [x] EnvVar 模式一键化：apply 自动发放 Token 并由 envsync 持久化环境变量
      （env.sh + rc 注入 / launchctl + LaunchAgent / HKCU，ADR-016）；
- [x] 各平台环境变量手动设置指令降级为「使用说明」末尾备用节（平台切换保留）；
- [x] Token 轮换联动：Inline 形态就地改写配置，EnvVar 形态重写环境变量存储
      （connection_rotate_sync）；
- [x] Inline 明文写入为用户在预览弹窗中的显式选择（ADR-015 推翻 ADR-012 的默认禁止）；
- [x] 断开接入即清理环境变量；最后一个客户端断开时全量清理（零残留，ADR-016）；
- [ ] 具体字段必须以执行时 Codex 官方文档为准。

## TASK-602：托盘

- [ ] Hub Start/Stop/Restart；
- [ ] 当前 Claude Provider；
- [ ] 当前 Codex Provider；
- [ ] Provider 快速切换；
- [ ] 当前 Workspace；
- [ ] Workspace 快速切换；
- [ ] 最近错误；
- [ ] 打开日志；
- [ ] 打开主窗口；
- [ ] 完全退出。

---

# Phase 7：测试、加固与发布

## TASK-700：自动测试矩阵

Rust：

- [ ] domain unit tests；
- [ ] config golden tests；
- [ ] adapter tests；
- [ ] SQLite migration tests；
- [ ] auth tests；
- [ ] session tests；
- [ ] router tests；
- [ ] stdio integration tests；
- [ ] HTTP integration tests；
- [ ] control IPC tests。

前端：

- [ ] component tests；
- [ ] form validation；
- [ ] query error states；
- [ ] diff preview；
- [ ] accessibility；
- [ ] Playwright workflows。

跨进程：

- [ ] Desktop ↔ Hub；
- [ ] Hub ↔ stdio MCP；
- [ ] Hub ↔ HTTP MCP；
- [ ] Claude/Codex same endpoint；
- [ ] restart/recovery；
- [ ] installer upgrade。

## TASK-701：Chaos 与安全测试

- [ ] 下游进程启动即退出；
- [ ] 下游输出非法 JSON；
- [ ] stderr flood；
- [ ] tools/list 超时；
- [ ] tools/call 超时；
- [ ] HTTP 401/403/404/429/500/502；
- [ ] SSE 中断；
- [ ] Hub 端口被占用；
- [ ] Hub 崩溃；
- [ ] 数据库锁；
- [ ] 配置文件只读；
- [ ] 写入中外部修改；
- [ ] Token 猜测/暴力请求限速；
- [ ] DNS rebinding/Origin；
- [ ] Host Header；
- [ ] 路径注入；
- [ ] 命令注入；
- [ ] Secret 日志扫描；
- [ ] 诊断包 Secret 扫描。

## TASK-702：性能基线

目标：

- Hub 空闲内存可控；
- 20 个下游 Server 可管理；
- 100 个 Tool Catalog 生成稳定；
- 缓存后 tools/list P95 < 100ms；
- 路由额外开销 P95 < 20ms，不含下游执行；
- 50 个并发 Tool Call 不串线；
- 日志写入不阻塞路由；
- UI 10,000 条日志可虚拟滚动。

具体数字可在首轮基准后调整，但调整必须写 ADR。

## TASK-703：打包发布

- [ ] macOS Apple Silicon；
- [ ] macOS Intel（若仍决定支持）；
- [ ] Windows x64；
- [ ] Hub Sidecar 正确打包；
- [ ] Bridge Sidecar 正确打包；
- [ ] SQLite Migration；
- [ ] Tauri Updater；
- [ ] DMG；
- [ ] NSIS/MSI 二选一并记录 ADR；
- [ ] 签名流程；
- [ ] macOS notarization；
- [ ] Windows code signing；
- [ ] 升级/降级保护；
- [ ] Release Notes；
- [ ] SBOM；
- [ ] 第三方依赖清单。

签名凭据由项目所有者提供；Codex 不得伪造或跳过签名成功记录。

---

# 8. 最终发布 Gate

## GATE-A：统一入口

- Claude Code 与 Codex 同时连接同一 `/mcp`；
- 不同 Token；
- 独立 Session；
- 并发调用不串线；
- 断开一个不影响另一个。

## GATE-B：配置安全

- JSON 未知字段不丢失；
- TOML 注释和未知字段不丢失；
- 所有写入前有 Diff 和 Snapshot；
- 写入失败自动恢复；
- 外部修改不被静默覆盖；
- 任意变更可回滚。

## GATE-C：MCP 聚合

- stdio + Streamable HTTP 混合；
- Tool 名冲突正确处理；
- Workspace 过滤；
- Tool 权限二次校验；
- 单 Server 故障隔离；
- Hub 重启恢复。

## GATE-D：产品闭环

用户可以完成：

```text
安装 MCPane
→ 检测 Claude Code/Codex
→ 导入已有配置
→ 添加 Provider
→ 切换 Provider
→ 添加本地/远程 MCP
→ 创建 Workspace
→ 一键接入两个客户端
→ 通过同一 /mcp 调用工具
→ 查看日志
→ 回滚配置
```

## GATE-E：安全与发布

- Secret 不进入 DB、普通日志、导出、诊断包；
- Hub 只监听 Loopback；
- Origin/Host/Auth 通过测试；
- macOS/Windows 安装包通过；
- CI 全绿；
- 无高危依赖漏洞；
- 无 P0 路径 `TODO`；
- 无被跳过的关键测试；
- 应用显示名、包名、配置键、环境变量和二进制全部符合 MCPane 冻结标识。

---

# 9. 每个 Task 的 Definition of Done

一个 Task 只有同时满足以下条件才算完成：

- 功能代码已实现；
- 单元测试已写；
- 必要集成测试已写；
- macOS/Windows 条件代码均可编译；
- `cargo fmt`、Clippy、Rust Tests 通过；
- Typecheck、Biome、Frontend Tests 通过；
- 无新增 Secret 泄漏；
- 文档和 ADR 已更新；
- 失败路径有明确错误；
- 没有把未验证行为写成已验证；
- PR/提交说明包含验收证据；
- 涉及外部协议、客户端配置或依赖版本时，附官方来源链接和验证日期。

---

# 10. 交给 Codex 的第一条执行指令

```text
你现在负责实现 MCPane。所有新代码和工程标识必须使用 MCPane / mcpane / MCPANE 前缀，不得混入旧项目代号。

先完整阅读：
1. AGENTS.md
2. docs/EXECUTION_PLAN.md
3. docs/adr/
4. 两个参考项目的 README 和指定模块：
   - https://github.com/farion1231/cc-switch
   - https://github.com/mcp-router/mcp-router
   - MCP Router: apps/electron/src/main/modules/mcp-server-runtime/aggregator-server.ts
   - MCP Router: apps/cli/src/commands/connect.ts
   - MCP Router: apps/cli/src/commands/serve.ts
5. Codex 的 Streamable HTTP 实现与测试：
   - https://github.com/openai/codex/blob/main/codex-rs/rmcp-client/src/rmcp_client.rs
   - https://github.com/openai/codex/blob/main/codex-rs/rmcp-client/tests/streamable_http_remote.rs
   - https://github.com/openai/codex/blob/main/codex-rs/rmcp-client/tests/streamable_http_recovery.rs

只参考公开功能、交互和架构，不复制代码。

本轮只执行：
- TASK-000
- TASK-001
- TASK-002
- TASK-100

不要开发完整 UI、Provider 管理或下游 MCP 聚合。

完成后输出：
- 修改文件
- 依赖及锁定版本
- ADR
- 最小 Hub 的启动命令
- /mcp 协议日志示例
- 所有测试结果
- TASK-101/102 是否已具备前置条件

任何无法真实验证的部分必须写“未验证”，不得假设成功。
```

第二轮执行指令：

```text
继续 MCPane，执行 TASK-101 至 TASK-105，并完成 GATE-1。

必须使用真实发布版 Claude Code 和 Codex。
两者必须同时连接同一个 http://127.0.0.1:24860/mcp。
使用不同 Bearer Token。
生成 docs/interop/claude-codex-same-endpoint.md。
未通过时保留完整脱敏日志，并按清单实现最小兼容 Bridge；不得绕过或伪造 Gate。
```

---

# 11. 研究与版本快照

核对日期：2026-07-10。

## 两个竞品参考仓库

1. CC Switch  
   https://github.com/farion1231/cc-switch

2. MCP Router  
   https://github.com/mcp-router/mcp-router

用途仅限：公开功能、交互、模块边界、失败场景和架构思路研究。MCPane 必须独立设计和实现，不复制竞品源代码、组件、文案、视觉资源或独特目录结构。

## 官方依据

- MCP 2025-11-25 稳定规范  
  https://modelcontextprotocol.io/specification/2025-11-25
- Streamable HTTP Transport  
  https://modelcontextprotocol.io/specification/2025-11-25/basic/transports
- MCP 官方 Rust SDK  
  https://github.com/modelcontextprotocol/rust-sdk
- Claude Code MCP 文档  
  https://code.claude.com/docs/en/mcp
- Codex MCP 文档  
  https://developers.openai.com/codex/mcp
- Codex 开源仓库  
  https://github.com/openai/codex

## 基线版本命令

Codex 在 TASK-001 中应重新执行并把结果写入 `docs/architecture/DEPENDENCIES.md`：

```bash
node --version
pnpm --version
npm view react version
npm view typescript version
npm view vite version
npm view tailwindcss version
npm view @base-ui/react version
npm view @biomejs/biome version
rustc --version
cargo search rmcp --limit 1
cargo search tauri --limit 1
```

若 registry、官方 release 与本文版本不一致，以执行当日稳定发行版和实际兼容测试为准，并更新 ADR 与 lockfiles。

---

## 冻结基线之后的增补

本文件是 V0.4 冻结版，不再改写其中的 Phase/Task。冻结之后新增的成套工作
另立方案文档，在此登记指针：

| 段号 | 主题 | 方案 | 决策 |
|---|---|---|---|
| TASK-800~806 | MCP `2026-07-28` 双栈升级（rmcp 3.1.1） | [docs/plans/mcp-2026-07-28-migration.md](plans/mcp-2026-07-28-migration.md) | [ADR-019](adr/ADR-019-mcp-2026-07-28-adoption.md) |
