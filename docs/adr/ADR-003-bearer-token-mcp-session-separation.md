# ADR-003：Bearer Token 与 MCP Session 分离

- 状态：Accepted
- 日期：2026-07-11

## 背景

Bearer Token 若与协议会话混用（例如把 Token 当 Session 标识，或把 Token 作为
MCP 参数传递），会导致撤销、并发与隔离语义纠缠不清。

## 决策

1. **Bearer Token 标识 ClientConnection**（Claude / Codex / 自定义客户端），
   在 HTTP 中间件层验证；**`MCP-Session-Id` 单独标识协议会话**（McpSession）。
2. Token 规则：
   - 系统安全随机源生成 ≥ 256 bit；
   - 完整 Token 只在创建时展示一次；
   - 持久层只保存哈希（SHA-256）与可识别前缀；
   - 比较使用常量时间算法（`subtle`）。
3. 一个 Token 可产生多个 MCP Session；Session 404 只影响该 Session；
   撤销某客户端 Token 不影响其他客户端；Session 不得以 URL 作为唯一标识。

## 理由

- 认证（谁在连接）与会话（这次对话的协议状态）是不同关注点；
- 独立撤销 / 轮换 Token 是多客户端控制面的核心能力；
- 常量时间比较与只存哈希是凭据处理的基线安全实践。

## 后果

- 数据模型需要 `client_connections` 与 `mcp_sessions` 两张表及关联；
- 中间件与 Session Manager 分层实现（TASK-100 起）。
