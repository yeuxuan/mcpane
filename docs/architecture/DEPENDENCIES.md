# 依赖版本锁定记录

- 核对日期：**2026-08-28**（rmcp 3.1.3 → 3.1.4，对齐 MCP `2026-07-28` 官方 SDK）
- 核对方式：对照 `Cargo.lock` / `pnpm-lock.yaml` 实际解析版本
- 锁定文件：`pnpm-lock.yaml`、`Cargo.lock`（根 workspace）、
  `apps/desktop/src-tauri/Cargo.lock`（桌面壳独立 workspace）

## 工具链

| 项目 | 版本 | 说明 |
|---|---|---|
| Rust | 1.97.0 stable, Edition 2024 | `rust-toolchain.toml` 固定 |
| Node.js | 24.13.1（本机） | engines 要求 >= 24 |
| pnpm | 11.11.0 | 根 `package.json` `packageManager` 字段 + corepack 激活 |

## 前端 / 桌面（与执行计划 §3.1 基线对照）

| 包 | 基线 | 实际锁定 | 状态 |
|---|---|---|---|
| tauri (crate) | 2.11.5 | 2.11.5 | ✅ 一致 |
| tauri-build (crate) | — | 2.6.3 | 当日最新稳定 |
| @tauri-apps/cli | 2.11.4 | 2.11.4 | ✅ 一致 |
| @tauri-apps/api | — | 2.11.1 | 当日最新稳定 |
| react / react-dom | 19.2.7 | 19.2.8 | ✅ patch |
| typescript | 7.0.2 | 7.0.2 | ✅ 一致（迁移点见 ADR-010） |
| vite | 8.1.4 | 8.2.1 | ✅ minor |
| tailwindcss / @tailwindcss/vite | 4.3.2 | 4.3.3 | ✅ patch |
| @tanstack/react-router | 1.170.17 | **未引入（偏差）** | 见偏差记录 4 |
| @tanstack/react-query | 5.101.2 | 5.101.4 | ✅ patch |
| @tanstack/react-table | 8.21.3 | **未引入（偏差）** | 见偏差记录 4 |
| zustand | 5.0.14 | **未引入（偏差）** | 见偏差记录 4 |
| react-hook-form | 7.81.0 | 7.85.0 | ✅ 已引入（2026-07-21，见偏差记录 5） |
| @hookform/resolvers | — | 5.9.1 | RHF↔zod 桥（随 RHF 引入） |
| zod | 4.4.3 | 4.4.3 | ✅ 一致 |
| i18next | 26.3.6 | 26.3.6 | ✅ 一致 |
| lucide-react | 1.24.0 | 1.31.0 | ✅ minor |
| @biomejs/biome | 2.5.3 | 2.5.8 | ✅ patch（2.5.9 当日刚发，未过 pnpm 供应链最短发布窗口，未跟） |
| vitest | 4.1.10 | 4.1.10 | ✅ 一致 |
| playwright | 1.61.1 | 1.62.1 | ✅ minor（@playwright/test） |
| shadcn/ui | 当前稳定版 | components.json 已初始化 | 组件按 Phase 5 需求逐个引入 |
| Base UI | 1.6.0 | **未引入（偏差）** | 见下方偏差记录 |

### 偏差记录

1. **Base UI**：执行计划基线写 `Base UI 1.6.0`，但 2026-07-11 registry 上
   `@base-ui-components/react` 的最新版本为 `1.0.0-rc.0`（dist-tag `latest`），
   **不存在任何稳定版**。按 AGENTS.md 规则 6（禁止 RC），P0 暂不引入 Base UI；
   shadcn/ui 组件在 Phase 5 落地时若 Base UI 仍无稳定版，改用 shadcn 默认
   Radix 方案，届时补充 ADR。
2. **TS7 `baseUrl` 移除**：tsconfig 不再使用 `baseUrl`，`paths` 改为相对前缀
   （ADR-010 已记录）。
3. **Playwright 延后**：仅版本核对（1.61.1 stable），不在 TASK-001 安装。
   （后已按计划于 Phase 7 引入，此条仅存档。）
4. **TanStack Router / TanStack Table / Zustand 按 YAGNI 不引入**（2026-07-21
   复核）：基线冻结时预设了这三件，但实际开发中始终没有出现对应需求——
   单窗口桌面应用的侧边栏导航用组件状态即可（无深链接/URL 状态诉求）；
   现有表格（Logs 500 行、Usage 聚合）无排序/分页/虚拟化需求；全局客户端
   状态已由 react-query（服务端状态）+ 组件局部状态覆盖，没有跨页共享的
   纯客户端状态。**决议：不为对齐文档而引入**；未来出现真实需求时按本表
   冻结版本引入（版本已过时则重新核对 registry 并更新本表）。
5. **react-hook-form 于 2026-07-21 引入**（当前 7.85.0 + resolvers
   5.9.1）：试点为 Settings 云同步表单（`CloudSyncSection`，RHF +
   zodResolver + `LabeledField error` 内联报错）。**新表单一律走
   RHF+Zod**；存量大表单（`ServerSetupModal`、`ProviderEditModal`）已有
   测试覆盖、整体重写风险大于收益，待功能性改动时机会性迁移。

## Rust / 后端（与执行计划 §3.2 对照）

| crate | 锁定 | 说明 |
|---|---|---|
| rmcp | 3.1.4 | crates.io 稳定版，与 release tag `rmcp-v3.1.4` 一致；禁 Git main 与 beta。启用 `request-state`（MRTR 密封，TASK-805）。3.1.4 起 `RequestStateCodec::try_new` 强制 32 字节密钥下限 |
| axum | 0.8.9 | |
| tokio | 1.53.1 | |
| tower / tower-http | 0.5 / 0.6 | |
| reqwest | 0.13.4（声明 `0.13`） | 测试/下游 HTTP 客户端；0.13 起 socks 为纯内建实现 |
| serde / serde_json | 1 | |
| tracing / tracing-subscriber | 0.1 / 0.3 | |
| thiserror / anyhow | 2 / 1 | anyhow 仅应用边界 |
| uuid | 1 | |
| sha2 | 0.10 | Token 哈希 |
| subtle | 2 | 常量时间比较 |
| rand | 0.9 | 测试 Token 生成（OsRng） |
| tempfile | 3 | 测试；另在 `mcpane-ccswitch` 作正式依赖（读 cc-switch 库前先复制出副本） |
| sse-stream | 0.2.5 | Hub 下游 SSE 传输（`mcpane-hub-downstream`）；与 rmcp 依赖树对齐 |
| toml_edit | 0.25.13 | Codex TOML 语法树级最小修改（Phase 2 引入） |
| similar | 3.2.0 | unified diff 生成（Phase 2 引入） |
| notify | **8.2.0** | 外部修改监听。注意：registry `latest` 为 `9.0.0-rc.4`（RC，按规则 6 拒用），锁定最新稳定 8.2.0 |
| sqlx | 0.9.0 | `mcpane-storage` / `mcpane-sync` / `mcpane-ccswitch`（只读用户的 cc-switch 库）/ Hub 日志 |
| keyring | 4.1.6（声明 `4.1`） | `mcpane-secrets` OS keychain |
| schemars / sysinfo | 未引入 | 仍无需求，不为对齐文档而引入 |

MCP 协议版本：**双栈**——主协议 `2026-07-28`，`KNOWN_VERSIONS` 内的历史版本
（含 `2025-11-25`）长期并行，不预设退役（ADR-019 取代 ADR-009）。
详见 AGENTS.md「协议与依赖基线」与 `docs/interop/dual-stack-2026-07-28.md`。

## Windows 平台依赖（`[target.'cfg(windows)'.dependencies]`）

只在 Windows target 下编译，macOS/Linux 构建不受影响。Win32 API 无安全封装可用，
故这些依赖对应的都是带 `#![allow(unsafe_code)]` 豁免的模块（工作区默认
`unsafe_code = "deny"`，每处豁免都在代码里写明理由）。

| crate | 锁定 | 使用方 | 用途 |
|---|---|---|---|
| windows | 0.62 | mcpane-config | `permissions`：凭据文件 DACL 收紧；`process_output`：CLI 输出按系统代码页解码 |
| windows | 0.62 | mcpane-secrets | 超出凭据管理器 2560 字节上限的大凭据走 DPAPI 加密落文件 |
| windows | 0.62 | mcpane-hub | 控制面 Named Pipe 的安全描述符（默认 SD 会向 Everyone 开放读权限）；`MCPANE_SUPERVISOR_PID` 兜底轮询查父进程是否还是同一个存活进程（`OpenProcess`/`GetExitCodeProcess`/`GetProcessTimes`） |
| windows | 0.62 | mcpane-hub-downstream | stdio 子进程的 `CREATE_NO_WINDOW` 标志值 |
| windows-sys | 0.61 | src-tauri | envsync 的 `WM_SETTINGCHANGE` 广播；spawn hub sidecar 时取自身创建时间（`GetProcessTimes`）传给 hub 做兜底轮询的身份核对 |
| winreg | 0.55 | src-tauri | envsync 直写 `HKCU\Environment`（ADR-016） |
| winreg | 0.55 | mcpane-hub-protocol | CLI 检测合并注册表 PATH（自更新重启会丢用户 PATH） |
| process-wrap | 9.1 | mcpane-hub-downstream | Job Object 纳管 stdio 子进程树；本就是 rmcp 的间接依赖，此处提升为直接依赖 |

版本统一在 `windows` 0.62：`cargo tree` 确认依赖树中只有一份，不与 process-wrap
拉进来的那份分叉。`windows-sys` 有三份，均来自第三方间接依赖，非本项目直接引入。

## 验证结果

### 2026-07-11（TASK-001）

- `pnpm typecheck`（TS 7.0.2）：通过
- `pnpm test`（vitest 4.1.10，jsdom）：全部通过
- `pnpm lint`（Biome 2.5.3）：通过
- `vite build`（8.1.4）：通过
- `cargo build`（src-tauri，tauri 2.11.5）：通过
- 全部依赖均为 stable 发行版，无 beta / RC / nightly / Git 依赖

### 2026-08-13（lock 复核）

- 根 `Cargo.lock`：`reqwest 0.13.4`、`sqlx 0.9.0`、`keyring 4.1.4`、`rmcp 3.1.1`、`tokio 1.52.3`
- 前端 `pnpm-lock.yaml` 与 [`apps/desktop/package.json`](../../apps/desktop/package.json) 声明一致（React 19.2.7、TanStack Query 5.101.2、Vite 8.1.4）
- 仍无 beta / RC / nightly / Git 依赖

### 2026-08-18（稳定 patch/minor 升级）

- 根 `Cargo.lock`：`rmcp 3.1.3`、`tokio 1.53.1`、`keyring 4.1.6`、`toml_edit 0.25.13`、`similar 3.2.0`、`sse-stream 0.2.5`；`reqwest 0.13.4` / `sqlx 0.9.0` / `notify 8.2.0` 仍为当日最新稳定
- 前端：React 19.2.8、TanStack Query 5.101.4、Vite 8.2.1、Tailwind 4.3.3、Biome 2.5.8、Playwright 1.62.1、RHF 7.85.0
- 明确未跟：`notify 9`（RC）、`jsdom 30` / `@testing-library/jest-dom 7`（大版本，需 ADR）、`hmac 0.13` / `sha2 0.11` / `rand 0.10` / `winreg 0.56`（0.x 破坏性）；`pnpm 11.22.0`（工具链，未改 `packageManager`）
- 验证：`pnpm typecheck` / `pnpm lint`（Biome 2.5.8）/ `pnpm test`（vitest 222 passed）通过；`cargo test --workspace --all-features` 通过（`perf::builtin_call_p95_under_20ms` 在满载下首次 30ms 超时，空闲复跑 19.3ms 通过，判定为机载抖动而非回归）
- 仍无 beta / RC / nightly / Git 依赖

### 2026-08-28（rmcp 对齐 MCP 2026-07-28 官方 SDK）

- 官方规范仍为 `2026-07-28`（`schema/draft` 的 `LATEST_PROTOCOL_VERSION` 未改；其后无新的稳定版本目录）
- 根 `Cargo.lock`：`rmcp 3.1.4` / `rmcp-macros 3.1.4`（crates.io 当日最新稳定）。3.1.4 含：`RequestStateCodec::try_new` 强制 32 字节密钥、预协商 `_meta` 以 `-32602` 报错、elicitation `requestedSchema` 保留 `$schema` dialect。`#1151` declined MRTR 只改了 SDK 示例，Hub 仍原样透传 `inputResponses`
- 明确未跟：规范 `draft/`（未定版）；rmcp Git `main` / beta
- 验证：`cargo clippy -p mcpane-hub -p mcpane-hub-downstream -p mcpane-hub-catalog --all-targets --all-features -- -D warnings` 通过；`cargo test -p mcpane-hub --all-features` 通过（跳过既往会抖动的 `perf::builtin_call_p95_under_20ms`）；`http_discover` 11 项（含缺 `_meta` / 缺 `clientCapabilities` 钉死 `-32602`）、`aggregation` 46 passed + 1 ignored（外网）、MRTR 4/4、`mcpane-hub-downstream` 17/17
- 仍无 beta / RC / nightly / Git 依赖

## 更新规则

- 只允许升级到新的稳定 patch/minor，且需通过兼容测试后更新锁文件；
- 大版本变化必须新增 ADR；
- 每次版本变更同步更新本文件。
