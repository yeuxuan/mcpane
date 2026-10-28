# TASK-103 / GATE-1：Claude Code 与 Codex 同端点并发验证报告

- 日期：2026-07-11
- 结论：**GATE-1 通过**——真实发布版 Claude Code 与 Codex 使用不同 Bearer Token
  同时通过同一个 `/mcp` 完成 initialize → tools/list → tools/call，
  鉴权、并发、会话互不干扰。**无需启用 mcpane-bridge 兼容后备。**

## 环境

| 项目 | 值 |
|---|---|
| Claude Code | 2.1.207（真实发布版） |
| Codex | codex-cli 0.133.0（真实发布版） |
| Hub | mcpane-hub 0.0.1，`http://127.0.0.1:24860/mcp`（无状态基线模式） |
| OS | macOS（Darwin 24.6.0） |
| Token | 两客户端各自独立 Token，经环境变量注入，配置零明文 |

## 场景 1：双客户端同时各调用 echo 20 次

两个客户端进程同时启动，各自被指示按序调用
`mcpane__echo`（`claude-103-1..20` / `codex-103-1..20`）+ 1 次 `connection_info`。

客户端侧校验（事件流逐条断言）：

```text
Codex : 20/20 echo 全部 completed，回显与入参逐条一致，顺序 1..20 正确
Claude: 20/20 echo 全部成功（最终回复确认逐条回显一致），is_error=false
connection_info：各自返回正确身份（client_codex_dev / client_claude_dev）
```

Hub 侧脱敏日志统计：

```text
claude 请求 25 个（04:30:48 → 04:32:59）
codex  请求 24 个（04:30:44 → 04:31:47）
时间窗重叠：真并发；相邻请求身份交错切换 16 次
鉴权/权限错误（401/403）：0
```

- **JSON-RPC ID 不串线 / 响应不串线**：客户端侧逐条入参↔回显一致；
  另有协议层集成测试（双客户端各 20 并发 echo）断言 `id` 与内容零串线。
- **关闭一个不影响另一个**：Codex 于 04:31:47 完成并断开，Claude 继续正常
  运行至 04:32:59 完成；反方向在撤销场景中由单独运行的 Codex 证实。

## 场景 2：Token 撤销隔离（控制面 UDS 命令，真实客户端验证）

```text
revoke client_claude_dev
  → Claude Code：MCP server 不可用（Hub 返回 401）✅
  → Codex：echo 正常完成 ✅
enable client_claude_dev / revoke client_codex_dev
  → Codex：initialize 即被拒——"Auth required"（收到 401 + WWW-Authenticate，
    因 required=true 会话创建失败，错误明确）✅
  → Claude Code：echo 正常完成 ✅
enable client_codex_dev → 双方恢复正常
```

## GATE-1 检查表

| 条件 | 结果 |
|---|---|
| 两者连接同一 `/mcp` | ✅ |
| 两者使用不同 Token | ✅ |
| 同时 initialize / tools/list | ✅（Hub 日志时间窗重叠） |
| 各连续 20 次 echo | ✅（40/40 零差错） |
| 并发调用不串线 | ✅（客户端断言 + 16 次身份交错 + 集成测试） |
| JSON-RPC ID 不串线 | ✅ |
| Token 身份不串线 | ✅（connection_info 双向确认） |
| 慢请求不阻塞另一客户端 | ✅ 协议层集成测试覆盖；真实客户端场景下双方并发窗口重叠且互不拖慢。未构造人为慢工具（P0 无此内建工具），TASK-402 引入下游后补充长耗时场景 |
| 关闭 Claude 不影响 Codex | ✅ |
| 关闭 Codex 不影响 Claude | ✅ |
| 撤销 Claude Token 后 Codex 正常 | ✅ |
| 撤销 Codex Token 后 Claude 正常 | ✅ |

## 结论与后续

- Claude Code 与 Codex 的**原生 Streamable HTTP 直连均通过**，
  `mcpane-bridge` 保持"仅兼容后备"定位（ADR-002 维持不变）；
- Codex exec 的工具注解审批问题及 MCPane 侧治本方案见
  `docs/interop/codex-direct-http.md`；
- 有状态 Session（`MCP-Session-Id`）验证见 TASK-104。
