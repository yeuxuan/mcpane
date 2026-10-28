# MCPane 命名与标识冻结

来源：`docs/MCPane_EXECUTION_PLAN.md` §0.1，本文件为工程侧唯一命名基准。

## 冻结标识

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
| 内建 MCP Tool 前缀 | `mcpane__` |
| 默认上游地址 | `http://127.0.0.1:24860/mcp` |
| Tauri Bundle Identifier | `io.mcpane.desktop`（首次签名发布前最终确认，此后不得改变） |

## 数据目录

```text
macOS   ~/Library/Application Support/MCPane/
Windows %APPDATA%\MCPane\
```

## 禁用写法

对外文案统一写作 `MCPane`。以下变体一律禁止，由 `scripts/brand-scan.sh` 在 CI 强制扫描：

- `McPane`、`Mcpane`、`mcPane`、`MCP Pane` <!-- brand-scan: allow-prohibited-spellings -->

需要在文档中引用禁用写法本身时（如本文件），在该行追加注释标记
`brand-scan: allow-prohibited-spellings`。

## 本地目录说明

本仓库在开发机上的本地文件夹名为 `MCPanePro`（用户既有目录，仅为本地别名）。
仓库的规范标识仍为 `mcpane`：所有包名、crate 名、二进制名、配置键、环境变量
均使用冻结标识，与本地文件夹名无关。远端仓库创建时必须命名为 `mcpane`。

## 环境变量（单一事实表）

前缀一律 `MCPANE_*`。Hub `--help`、桌面端快捷配置与本表应对齐。

### Hub 运行时

| 变量 | 用途 |
|---|---|
| `MCPANE_HOME` | 数据目录（默认见上方「数据目录」） |
| `MCPANE_HUB_BIND` | 数据面监听地址（默认 `127.0.0.1:24860`；非 loopback 拒绝启动） |
| `MCPANE_HUB_STATEFUL` | 设为 `0` 关闭 `MCP-Session-Id`，回退无状态基线 |
| `MCPANE_SESSION_TIMEOUT_SECS` | 会话空闲超时（默认 300） |
| `MCPANE_HEALTH_INTERVAL_SECS` | 下游健康探测间隔（默认 30） |
| `MCPANE_SUPERVISED` | 置任意值 = 桌面 sidecar 托管：父进程退出时随之退出 |
| `MCPANE_DOWNSTREAM_CALL_TIMEOUT_SECS` | 下游 `tools/call` 超时（默认 300） |

### Bridge（`mcpane-hub bridge`）

| 变量 | 用途 |
|---|---|
| `MCPANE_TOKEN` | 桥进程 Bearer Token（必填） |
| `MCPANE_HUB_URL` | Hub 数据面端点（默认 `http://127.0.0.1:24860/mcp`） |

### 客户端凭据（P0 互操作约定）

明文只经 envsync 注入进程环境，配置文件写环境变量引用，不落盘。

| 客户端 | 变量 |
|---|---|
| Claude Code | `MCPANE_CLAUDE_TOKEN` |
| Codex | `MCPANE_CODEX_TOKEN` |
| Cursor | `MCPANE_CURSOR_TOKEN` |
| VS Code | `MCPANE_VSCODE_TOKEN` |
| Devin | `MCPANE_DEVIN_TOKEN` |
| Gemini CLI | `MCPANE_GEMINI_TOKEN` |
| Qoder | `MCPANE_QODER_TOKEN` |
| Qwen Code | `MCPANE_QWEN_TOKEN` |
| CodeBuddy | `MCPANE_CODEBUDDY_TOKEN` |

Claude Desktop 走 stdio 桥，凭据写 `env` 块里的 `MCPANE_TOKEN` / `MCPANE_HUB_URL`，无独立 `MCPANE_*_TOKEN`。

Cline、Trae、Antigravity 与 ZCode 的配置不做变量展开，EnvVar 模式只能写占位串由用户手填，同样没有 `MCPANE_*_TOKEN`。

Hub 独立调试时也认 `MCPANE_CLAUDE_TOKEN` / `MCPANE_CODEX_TOKEN`：启动时以固定 ID 入库（只存哈希）。

### 开发 / 测试专用（不进产品文档对外承诺）

| 变量 | 用途 |
|---|---|
| `MCPANE_DEVSERVER_NAME` | 内建 devserver 的 server 标识 |
| `MCPANE_DEVSERVER_MODERN_ONLY` | `1` = 只讲 `2026-07-28` |
| `MCPANE_DEVSERVER_LEGACY_RUDE` | `1` = 模拟粗鲁存量服务器 |
