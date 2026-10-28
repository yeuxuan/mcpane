# TASK-101：Codex 原生 Streamable HTTP 直连验证报告

> **协议已升级**：本报告记录的是 2026-07-11 单栈 `2025-11-25` 时期的验证事实，保留作历史记录。当前 Hub 为双栈（主协议 `2026-07-28`），见 [ADR-019](../adr/ADR-019-mcp-2026-07-28-adoption.md) 与 [dual-stack-2026-07-28.md](dual-stack-2026-07-28.md)。


- 日期：2026-07-11
- 结论：**通过**（真实发布版 Codex，非 SDK 模拟）

## 环境

| 项目 | 值 |
|---|---|
| Codex | codex-cli **0.133.0**（npm 发布版） |
| OS | macOS（Darwin 24.6.0，Apple Silicon） |
| Hub | mcpane-hub 0.0.1（无状态模式，json_response=true） |
| MCP 协议 | 2025-11-25 |
| URL | `http://127.0.0.1:24860/mcp` |
| 认证 | `bearer_token_env_var = "MCPANE_CODEX_TOKEN"`（环境变量，Token 不落盘） |

验证使用隔离 `CODEX_HOME`（临时目录），未触碰用户真实 `~/.codex/config.toml`。

## Codex 侧配置（脱敏）

```toml
[mcp_servers.mcpane]
url = "http://127.0.0.1:24860/mcp"
bearer_token_env_var = "MCPANE_CODEX_TOKEN"
enabled = true
required = true
startup_timeout_sec = 10
tool_timeout_sec = 60
default_tools_approval_mode = "auto"
```

## 请求顺序（Hub 侧脱敏日志证实，每次 Codex 会话）

```text
POST /mcp initialize                      → 200 application/json
POST /mcp notifications/initialized       → 202 (无 body)
POST /mcp tools/list                      → 200 application/json
POST /mcp tools/call mcpane__echo         → 200 application/json
POST /mcp tools/call mcpane__connection_info → 200 application/json
```

所有请求 Bearer 认证通过，Hub 识别为 `client_codex_dev`。

## 工具调用结果（codex exec --json 事件流摘录）

```json
{"item":{"type":"mcp_tool_call","server":"mcpane","tool":"mcpane__echo",
 "arguments":{"message":"codex-interop-101"},
 "result":{"structured_content":{"message":"codex-interop-101"}},"status":"completed"}}
{"item":{"type":"mcp_tool_call","server":"mcpane","tool":"mcpane__connection_info",
 "result":{"structured_content":{"clientConnectionId":"client_codex_dev",
 "clientType":"codex","protocolVersion":"2025-11-25",
 "requestId":"req_457ba1334d97409fa71b93ed57a2fe4a","serverVersion":"0.0.1",
 "sessionId":null}},"status":"completed"}}
```

## 过程中发现并解决的互操作问题（重要）

**现象**：`codex exec`（非交互）下所有 MCP 工具调用被立即取消，
错误 `user cancelled MCP tool call`；交互式 TUI 不受影响。
`approval_policy="never"`、`default_tools_approval_mode="auto"` 均无效。

**根因**（上游已知缺陷）：无可用 `ToolAnnotations` 的自定义 MCP 工具被 Codex
误路由进 app-tool 审批路径，非交互模式 stdin EOF 被解释为"用户拒绝"。
参见 openai/codex#16685、#29857、#24135；修复 PR #16632 截至 0.133.0 未合入。

**MCPane 侧治本方案**：为全部内建 Tool 补充完整 `ToolAnnotations`
（`readOnlyHint=true`、`destructiveHint=false`、`idempotentHint=true`、
`openWorldHint=false`）。这本身即 MCP 规范最佳实践，同时使带注解的只读工具
在 Codex `auto` 审批模式下直接放行。**修复后一次通过。**

**对后续阶段的约束**：Hub 聚合下游工具时（TASK-405），若下游工具缺失
annotations，需保留字段透传原样，并在文档中标注 Codex exec 兼容性风险；
不得伪造下游工具的注解语义。

**2026-08-31 补记**：上面这条约束照旧成立（Hub 自己仍然永不代填），但下游
工具已经有了出口——用户可为单个工具**署名声明**一份注解，只在下游缺席时补位、
绝不覆盖下游申报。见 ADR-002「Gate 补记」与
`docs/interop/tool-annotations-declaration.md`。

## 证据文件

- Codex 事件流：scratchpad `codex-101b.jsonl`（本地，含完整往返）
- Hub 脱敏日志：`hub-interop.log`（Token 扫描 0 命中）
