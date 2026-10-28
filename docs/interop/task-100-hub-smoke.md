# TASK-100 最小 Hub 冒烟记录

- 日期：2026-07-11
- 环境：macOS (Darwin 24.6.0)，Rust 1.97.0，rmcp 2.2.0，无状态模式
- 验证方式：`cargo run -p mcpane-hub` + `curl` / `nc`（真实进程，非 mock）
- 注意：本记录使用 curl 驱动，**不替代** TASK-101/102 要求的真实
  Claude Code / Codex 客户端验证（该两项当前状态：**未验证**）。

## 启动命令

```bash
# 互操作 PoC 约定（ADR-012）：Token 经环境变量注入，不落任何配置文件
export MCPANE_CLAUDE_TOKEN=<token-a>
export MCPANE_CODEX_TOKEN=<token-b>
cargo run -p mcpane-hub
# 环境变量缺失时 Hub 会生成随机 Token 并向 stderr 打印一次（仅开发引导）
```

## 协议往返示例（脱敏）

### initialize → 200 `application/json`

```http
POST /mcp HTTP/1.1
Authorization: Bearer [REDACTED]
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2025-11-25

{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"curl-smoke","version":"0"}}}
```

```json
{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"mcpane-hub","version":"0.0.1"},"instructions":"MCP Hub：Claude Code、Codex 与 MCP 工具的统一控制面。"}}
```

### tools/list → 两个内建 Tool，均带 `mcpane__` 前缀与输入/输出 Schema

```json
{"jsonrpc":"2.0","id":2,"result":{"tools":[
  {"name":"mcpane__connection_info","description":"返回当前客户端连接与协议信息，用于互操作验证","inputSchema":{...},"outputSchema":{...}},
  {"name":"mcpane__echo","description":"回显输入文本，用于连通性验证","inputSchema":{...},"outputSchema":{...}}
]}}
```

### tools/call `mcpane__connection_info`（Codex Token）

```json
{"jsonrpc":"2.0","id":3,"result":{"structuredContent":{
  "clientConnectionId":"client_codex_dev",
  "clientType":"codex",
  "requestId":"req_5e468f724faf49b0996a0777743cd8c2",
  "sessionId":null,
  "protocolVersion":"2025-11-25",
  "serverVersion":"0.0.1"
},"isError":false}}
```

### 无 Token → 401 + `WWW-Authenticate: Bearer`

### 伪造 Host / 非 loopback Origin → 403

## 控制面（macOS UDS）

```bash
nc -U "$MCPANE_HOME/hub-control.sock" < /dev/null
```

```json
{"name":"mcpane-hub","serverVersion":"0.0.1","protocolVersion":"2025-11-25",
 "endpoint":"http://127.0.0.1:24860/mcp","uptimeSecs":0,
 "connections":[
   {"clientConnectionId":"client_claude_dev","clientType":"claude","tokenPrefix":"smoke-cl"},
   {"clientConnectionId":"client_codex_dev","clientType":"codex","tokenPrefix":"smoke-co"}]}
```

- socket 权限 `0600`；`MCPANE_HOME` 路径超过 UDS `SUN_LEN` 时回退到
  `/tmp/mcpane-{uid}/`（目录 `0700`），启动日志有 WARN 提示；
- Windows Named Pipe：**未实现**（TASK-400），启动时输出告警。

## 日志脱敏验证

- 请求日志只含 request_id / method / path / status / duration /
  client_connection_id，不记录任何 Header；
- 对整份运行日志执行完整 Token 字符串扫描：**0 命中**；
- 控制面状态只暴露 8 位 Token 前缀。

## 自动化测试（`cargo test --workspace`，17 通过）

- 单元 9：Token 常量时间验证/邻近值拒绝、无明文存储、256-bit 生成、
  Host/Origin 允许清单、控制面状态脱敏、内建 Tool 前缀；
- 集成 8（真实 TCP + 完整中间件栈）：缺失/错误 Token 401（含 GET/DELETE）、
  双 Token initialize、tools/list 命名空间、echo 回环、connection_info 身份、
  双客户端身份不串线、双客户端各 20 并发 echo（ID 与内容零串线）、
  恶意 Origin 403 + 原始 TCP 伪造 Host 403。
