# TASK-102：Claude Code 原生 HTTP MCP 直连验证报告

> **协议已升级**：本报告记录的是 2026-07-11 单栈 `2025-11-25` 时期的验证事实，保留作历史记录。当前 Hub 为双栈（主协议 `2026-07-28`），见 [ADR-019](../adr/ADR-019-mcp-2026-07-28-adoption.md) 与 [dual-stack-2026-07-28.md](dual-stack-2026-07-28.md)。


- 日期：2026-07-11
- 结论：**通过**（真实发布版 Claude Code，非 SDK 模拟）

## 环境

| 项目 | 值 |
|---|---|
| Claude Code | **2.1.207**（npm 发布版） |
| OS | macOS（Darwin 24.6.0，Apple Silicon） |
| Hub | mcpane-hub 0.0.1（无状态模式，json_response=true） |
| MCP 协议 | 2025-11-25 |
| URL | `http://127.0.0.1:24860/mcp`（与 Codex 完全相同的 Endpoint） |
| Token | 独立 Claude Token，经 `${MCPANE_CLAUDE_TOKEN}` 环境变量展开，不落盘 |

验证使用 `--mcp-config <file> --strict-mcp-config`（隔离配置），
未触碰用户真实 `~/.claude.json` / `.mcp.json`。

## Claude Code 侧配置（脱敏）

```json
{
  "mcpServers": {
    "mcpane": {
      "type": "http",
      "url": "http://127.0.0.1:24860/mcp",
      "headers": { "Authorization": "Bearer ${MCPANE_CLAUDE_TOKEN}" }
    }
  }
}
```

环境变量展开语法 `${VAR}` 由 Claude Code 原生支持，验证有效——
Hub 侧收到的是解析后的真实 Token，配置文件中无明文。

## 请求顺序（Hub 侧脱敏日志证实）

```text
POST /mcp initialize                          → 200 application/json
POST /mcp notifications/initialized           → 202
POST /mcp tools/list                          → 200
POST /mcp tools/call mcpane__echo             → 200
POST /mcp tools/call mcpane__connection_info  → 200
```

全部识别为 `client_claude_dev`，与 Codex 的 `client_codex_dev` 完全区分。

## 工具调用结果（claude -p --output-format json，is_error=false）

```json
{"message":"claude-interop-102"}
```

```json
{"clientConnectionId":"client_claude_dev","clientType":"claude",
 "protocolVersion":"2025-11-25","requestId":"req_196ff6848fad4e35874695989dfcc4cb",
 "serverVersion":"0.0.1","sessionId":null}
```

## 备注

- 调用命令：`claude -p "<prompt>" --mcp-config mcpane-mcp.json --strict-mcp-config
  --allowedTools "mcp__mcpane__mcpane__echo,mcp__mcpane__mcpane__connection_info"`；
- Claude Code 对 MCP 工具的命名是 `mcp__{server}__{tool}`，与 Hub 的
  `mcpane__` 工具前缀叠加后为 `mcp__mcpane__mcpane__echo`，无冲突；
- 与 Codex 不同，Claude Code 对带 `readOnlyHint` 注解工具的非交互放行
  通过 `--allowedTools` 显式控制，行为符合预期。
