# TASK-104 / TASK-105：有状态 Session 与响应形式兼容验证报告

> **协议已升级**：本报告记录的是 2026-07-11 单栈 `2025-11-25` 时期的验证事实，保留作历史记录。当前 Hub 为双栈（主协议 `2026-07-28`），见 [ADR-019](../adr/ADR-019-mcp-2026-07-28-adoption.md) 与 [dual-stack-2026-07-28.md](dual-stack-2026-07-28.md)。


- 日期：2026-07-11
- 结论：**通过**。Hub 默认切换为有状态模式（`MCP-Session-Id`），
  无状态基线保留为 `MCPANE_HUB_STATEFUL=0`。

## 实现

- `HubOptions { stateful, session_keep_alive }`；
- Session 空闲超时经 `MCPANE_SESSION_TIMEOUT_SECS` 配置（默认 300s，
  底层为 rmcp `SessionConfig.keep_alive`）；
- `mcpane__connection_info` 在有状态模式返回真实 `sessionId`
  （取自 `MCP-Session-Id` 请求头），无状态模式为 `null`。

## TASK-104 检查表（协议层集成测试 `tests/http_session.rs`）

| 项目 | 结果 |
|---|---|
| 开启 `MCP-Session-Id` | ✅ initialize 响应携带该头 |
| 每次 initialize 产生独立 Session | ✅ 同 Token 两次 initialize 得到不同 ID，且均可独立工作 |
| 同一 Token 可有多个 Session | ✅ 同上 |
| POST/GET/DELETE 都验证 Session | ✅ POST/GET 无效 Session→404；DELETE 缺失 Session 头→400 |
| 无效 Session 返回 404 | ✅（rmcp 对未知 Session 的 DELETE 为幂等 202，规范允许） |
| 404 后能重新初始化 | ✅ 重新 initialize 得到新 Session 并恢复调用 |
| Claude Session 失效不影响 Codex | ✅ DELETE Claude Session 后 Codex Session 正常调用 |
| Session 超时可配置 | ✅ keep_alive=1s：2.5s 后 404，重新 initialize 恢复 |
| Session 清理不泄漏 pending request | ✅ rmcp keep_alive 机制回收 zombie session；超时后请求得到确定性 404 而非悬挂 |

## TASK-105 检查表

| 项目 | 结果 |
|---|---|
| initialize / tools/list / 短 tools/call 返回 `application/json` | ✅ 无状态基线模式直接 JSON（TASK-101/102 真实客户端日志证实）；有状态模式按规范可为 SSE，两客户端均正常解析 |
| Notification 返回 202 无 body | ✅ 集成测试断言 body 为空 |
| GET 支持 SSE | ✅（GET 要求 `Accept: text/event-stream`，缺失→406） |
| 必要时 POST 可返回 SSE | ✅ 有状态模式 POST 响应为 SSE，测试解析 data 帧 |
| Accept 不完整返回明确错误 | ✅ POST 缺 `text/event-stream`→4xx 带说明 |
| 协议版本不支持返回明确错误 | ✅ `MCP-Protocol-Version: 1999-01-01`→4xx |
| `MCP-Protocol-Version` 正确传递 | ✅ 真实两客户端均发送 `2025-11-25` 并通过校验 |

## 真实客户端有状态验证

- curl initialize → `mcp-session-id: 502b5a17-…` ✅
- Codex（0.133.0）有状态模式下 `connection_info.sessionId` 非空 ✅（见下方记录）
- Claude Code（2.1.207）同上 ✅
- Hub 重启（全部 Session 丢失）后，两客户端重新连接自动建立新 Session ✅

### 输出摘录（脱敏）

Codex（`codex exec --json` 事件流）：

```text
codex sessionId: 9d445feb-e41f-4654-83ac-7d3f4f75c45e
```

Claude Code（`claude -p` 原样输出）：

```json
{"clientConnectionId":"client_claude_dev","clientType":"claude",
 "protocolVersion":"2025-11-25","requestId":"req_abefc913ac584334bb67460069ef996f",
 "serverVersion":"0.0.1","sessionId":"d38de7a8-e1db-44ff-8d6b-9c5a6ff3189f"}
```

两个 Session ID 互不相同，与各自 Bearer 身份正确绑定。
