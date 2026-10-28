# GATE-3：MCP 聚合验收报告

- 日期：2026-07-11
- 结论：**GATE-3 十项验收全部通过**（自动化集成测试
  `binaries/mcpane-hub/tests/aggregation.rs`，97/97 workspace 测试全绿）

## 验收对照

| # | 条件 | 验证方式 |
|---|---|---|
| 1 | 一个 stdio MCP | `mcpane-devserver`（真实子进程，rmcp stdio transport，stderr 独立捕获） |
| 2 | 一个远程 Streamable HTTP MCP | in-process rmcp Streamable HTTP server（有状态模式） |
| 3 | 两者同时聚合 | tools/list 同时暴露两个下游 + 内建工具 |
| 4 | Claude/Codex 仍连接同一 `/mcp` | 上游端点与鉴权栈不变（Phase 1 真实客户端验证依然成立） |
| 5 | 两客户端可并发调用不同下游 | `tokio::join!` 双 Token 并发调用 stdio 与 HTTP 下游，结果互不串线 |
| 6 | 同名 Tool 路由正确 | 两下游同名 `search` → `github__search` / `filesystem__search` 各自命中 |
| 7 | 一个 Server 崩溃不影响另一个 | `github__crash` 使子进程退出 → HTTP 下游继续正常；死亡下游返回明确错误，Hub 本体健康 |
| 8 | Token 撤销和 Workspace 权限隔离 | Token 撤销见 GATE-1；Workspace：禁用 Tool 后新 Session 的 tools/list 不再暴露，**已 pin 的旧 Session 直接调用被二次权限校验明确拒绝**（Session-Pinned + 双重校验，ADR-004/TASK-406） |
| 9 | Hub 重启后可恢复 | 下游定义持久化 SQLite（`mcp_servers`），重启后读回定义 → 重连 → Catalog 重建成功 |
| 10 | 日志不含 Secret | 请求日志入 SQLite（元数据 + 脱敏截断的入参/出参摘要；列表不含摘要正文，详情按 id 取）。全量序列化扫描列表与详情，双 Token 0 命中 |

## 新增构件

- `mcpane-storage`：SQLite（WAL + migrations）。`client_connections`
  （创建/常量时间验证/轮换/撤销/last_used，明文永不入库——有测试）、
  `mcp_servers`、`workspaces`/`workspace_servers`/`workspace_tools`、
  `catalog_revisions`/`tool_routes`、`hub_request_logs`（含保留清理）。
- `mcpane-hub-downstream`：rmcp 客户端封装。stdio（不经 shell、env 注入、
  stderr 限长捕获）、Streamable HTTP（bearer_token 环境变量解析、
  sensitive header、静态 Header）、启动/调用超时、崩溃计数、
  连续 5 次熔断、指数退避（1→64s 封顶）。
- `mcpane-hub-catalog`：`{slug}__{name}` 命名空间、非法字符规范化、
  超长截断+稳定短哈希、alias（冲突/非法拒绝）、禁用过滤、
  不可变 Revision + schema_hash。
- Hub 动态聚合：`HubService` 手写 `ServerHandler`——Session 工厂创建时
  **pin** 当前 Catalog（有状态模式下每 Session 一次），内建工具经宏路由，
  聚合工具查 pin 的 Revision 路由到下游；注解原样透传（不伪造）。
- 控制面新增 `diagnostics` 命令（下游崩溃计数 / Catalog Revision / 工具数）。

## 尚未完成（更新至 2026-08-06）

> 本节初版（2026-07-11 快照）严重滞后于代码。下列多数项在 Phase 5/6 已接线，
> 此处据实更新：

**已补齐（初版记为未完成，现已实现）**：
- TASK-400 控制面：`ReloadServers` / `RotateToken` / `CreateConnection` /
  `DeleteConnection` / `RevokeToken` / `EnableToken` / `Diagnostics` /
  `SetConnectionWorkspace` / `SetConnectionServerAccess` / `ListCatalog` /
  `TestServer` 全部接线（见 `control.rs`）；
- TASK-401 认证失败限速：`AuthRateLimiter` 已实现并接入 `build_router`；
- TASK-404 健康检查/自动重启：`spawn_health_monitor` 已实现（MCP `ping`
  连续失败即按 spec 重连、句柄换新、重建 Catalog；`2026-07-28` 对端对 ping
  回 `-32601` 仍视为活着，超时/传输断开才算挂）；
- **Windows 控制面（Named Pipe）**：0.2.0 已落地（`control.rs` 的 `#[cfg(windows)]`
  分支，含 DACL 显式收紧与关闭时的缓冲区处理），本节此前记的「空实现」已过时。
  仍未做的是**真机运行时验证**——目前只过了 `x86_64-pc-windows-gnu` 目标的编译核验。

**仍未完成**：
- 控制面**事件订阅/推送**：当前 UDS 为一次性 请求→响应→关闭，Desktop 只能轮询
  Status；事件流未实现（功能上可接受）；
- **上游 `notifications/tools/list_changed`（仅 legacy 路径）**：2026-07-28 客户端
  已可经 `subscriptions/listen` 收到三类 listChanged 精确推送（ADR-019）；legacy
  路径 Hub 仍从不主动转发，能力声明也在 initialize 时裁掉。当前后果：legacy 客户端
  会话看不到 Server 增删，需重连或等缓存 TTL——Servers 页已就此加了说明；
- **Runtime Policy 只实现 Shared**：DB 的 `runtime_policy` 放行三档，Hub 侧不读该
  字段，遇到非 `shared` 值只发一条 warn 后按 Shared 运行（TASK-404 的
  PerClientConnection / PerUpstreamSession 未实现）；
- ~~取消/进度未接线~~ 均已于 2026-08-07 接线：上游取消（`notifications/cancelled`）
  与 Hub 自身 `call_timeout` 到期都会向下游发 cancelled（走 send_cancellable_request
  拿请求 ID，协作式取消，不保证下游真的停）；progress 按上游 progressToken 换发。
  progress 归属按「唯一在飞」判定，同一下游并发多路在飞时放弃转发（旧协议线上
  无关联字段，rmcp 还会覆写外发请求的 progressToken，精确归属暂不做）。
  注意这不是罕见边缘：下游连接被所有上游会话共享，任意两路并发命中同一
  下游即触发——progress 丢弃、elicitation 按拒绝处理。依赖 elicitation 做
  二次确认的下游工具在并发期会被自动拒绝，是当前架构的已知限制；
- TASK-402 Windows Job Object 清理：**未验证**（无 Windows 实机）。

**设计变更**：
- 下游 call 级**熔断已移除**（初版记为「已就绪」）。参考同类聚合网关 mcp-router
  的取舍：网关替上游熔断会吞掉本可成功的调用，且此前的实现一旦熔断便永久打开
  （短路时不发起调用，成功复位路径永远到不了）。下游恢复统一由健康监控负责；
  `crash_counter` 保留但仅作诊断遥测。

以上不影响 GATE-3 十项验收结论。
