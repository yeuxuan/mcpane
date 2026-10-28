# MCPane 工程执行规则（AGENTS.md）

本文件是所有 AI Agent 与人类贡献者的强制约束，内容源自
`docs/MCPane_EXECUTION_PLAN.md` 第 0 节，冲突时以执行计划为准。

## 1. 品牌与工程标识冻结

| 项目 | 冻结值 |
|---|---|
| 正式产品名 | `MCPane` |
| 品牌解释 | `MCP + Pane / MCP Control Plane` |
| 桌面应用显示名 | `MCPane` |
| 仓库标识 | `mcpane` |
| Desktop 可执行标识 | `mcpane` |
| Hub 名称与二进制 | `MCP Hub` / `mcpane-hub` |
| 兼容 Bridge | `mcpane-hub bridge` 子命令（stdio 桥；`mcpane-bridge` 为保留标识） |
| 客户端 MCP Entry | `mcpane` |
| Rust crate 前缀 | `mcpane-*` |
| TypeScript package 前缀 | `@mcpane/*` |
| 环境变量前缀 | `MCPANE_*` |
| 逻辑数据目录变量 | `MCPANE_HOME` |
| SQLite 文件 | `mcpane.db` |
| Keychain/Credential Service | `MCPane` |
| 默认上游地址 | `http://127.0.0.1:24860/mcp` |
| Tauri Bundle Identifier | `io.mcpane.desktop`（首次签名发布前最终确认） |

默认应用数据目录：

```text
macOS   ~/Library/Application Support/MCPane/
Windows %APPDATA%\MCPane\
```

强制规则：

- 新代码不得使用任何旧项目代号；
- MCP Tool 的产品自带前缀统一为 `mcpane__`；
- 对外文案统一写作 `MCPane`，禁止其他大小写变体（详见 `docs/branding/NAMING.md`）；
- CI 运行 `scripts/brand-scan.sh` 扫描品牌标识混用；
- Bundle Identifier 发布后视为永久兼容标识。

## 2. 执行总规则

1. 严格按 Phase 和 Task 顺序执行，不得跳过 Gate。
2. 每次只完成一个 Task；不要顺手重构无关模块。
3. 每个 Task 完成后必须输出：修改文件列表、实现说明、执行过的测试及结果、
   新增风险或未决问题、下一 Task 前置条件是否满足。
4. 参考竞品的公开功能、交互和架构，不复制竞品源代码、函数、组件、目录或资源。
5. 官方 MCP 规范、Claude Code 官方文档、Codex 官方文档优先级高于竞品实现。
6. 依赖使用执行当日最新稳定且相互兼容的版本，不得使用 beta、RC、nightly 或
   Git `main` 依赖；确认后锁定 `Cargo.lock` 与 `pnpm-lock.yaml`。
7. 不得静默覆盖 Claude Code 或 Codex 的已有配置。
8. 不得把 API Key、Bearer Token、OAuth Token、Cookie 或密码写入 SQLite、
   普通日志、测试快照或导出文件。
9. Rust 与 TypeScript 不允许以忽略类型错误、关闭校验或大量 `unwrap()` 的方式赶进度。
10. 任何配置写入功能必须同时提交失败恢复测试。
11. 任何 Hub 路由功能必须同时提交并发、鉴权和会话隔离测试。
12. 无法在当前机器完成真实 Claude Code/Codex 测试时，必须明确记录"未验证"，
    不得伪造通过结果。

## 3. 协议与依赖基线

- **双栈协议**（ADR-019）：`2026-07-28` 是主协议，`2025-11-25` 及更早版本长期
  并行。Hub 宣告 `KNOWN_VERSIONS` 全集（`2024-11-05` / `2025-03-26` /
  `2025-06-18` / `2025-11-25` / `2026-07-28`），请求哪个就给哪个。
  `get_info()` 里的 `2025-11-25` 只是**协商回退值**，不是上限；
- 写代码时**不得假设对端版本**。按版本分叉的三处已有先例，照抄它们：
  `HubService::is_modern(context)` 判本次请求；`apply_cache_hints` 门控
  `ttlMs`/`cacheScope`；覆写的 `initialize` 裁掉 legacy 的 `listChanged`；
- **新协议字段绝不能漏进 legacy 响应**。rmcp 只按版本清理 `resultType`，
  我们自己填的字段（`ttlMs`/`cacheScope`/capability 项）SDK 不管，必须自己门控。
  守卫在 `tests/http_discover.rs`，改协议面的东西必跑；
- `rmcp` 必须来自 crates.io 稳定发行版（基线 3.1.4）；
- 版本快照与偏差记录见 `docs/architecture/DEPENDENCIES.md`；
- 架构决策见 `docs/adr/`。

## 4. 常用验收命令

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
pnpm install --frozen-lockfile
pnpm typecheck
pnpm lint
pnpm test
pnpm build
pnpm e2e   # Playwright（首次需 pnpm --filter @mcpane/desktop exec playwright install chromium webkit）
pnpm brand-scan
```
