# MCP 2026-07-28 迁移实施方案

- 依据：[ADR-019](../adr/ADR-019-mcp-2026-07-28-adoption.md)
- 日期：2026-08-07
- 目标：`2026-07-28` 成为 Hub 主协议，历史版本长期并行服务，旧客户端零影响

## 影响面盘点（已逐文件核实）

引用 rmcp 的源文件共 12 个，Cargo.toml 4 处。实际需要改的比预想少：

| 类别 | 结论 |
|---|---|
| `ServerResult` / `ClientRequest` 等六个协议 union 的手写 `match` | **零处**——全部走 `#[tool_router]` / `#[tool_handler]` 宏，`#[non_exhaustive]` 破坏面为 0 |
| `ServerHandler::call_tool` 返回类型 | 仅 `hub.rs:414` 一处需改 `CallToolResponse`；`tool_execute`（`hub.rs:270`）是 `#[tool]` 方法由宏处理，`call_downstream_tool`（`hub.rs:648`）是私有辅助 |
| `stateful_mode` → `legacy_session_mode` | 仅 `app.rs:52` 一处 |
| `Meta` 拆分 / `Annotations.last_modified` | **零处**——项目未直接构造 `Meta`，`last_modified` 命中的全是本项目自己的 storage 字段 |
| `structured_content` `JsonObject`→`Value` | 仅 `tests/bridge_e2e.rs:89` 读一次，经 `to_value` 无影响 |
| `sse.rs` 的 `get_stream` | **不受影响**——`SseClient` 是本项目自有 trait（自 rmcp 0.8.5 移植），rmcp 3.x 改的是它自己的 streamable-http SSE 客户端 |
| 依赖树 | `rustc 1.97.0` ≥ MSRV 1.88；rmcp 3.1.1 要 `reqwest ^0.13.2`，项目已在 0.13，不动 |

结论：**TASK-800 是小改动**，真正的工作量在 TASK-802（无状态 Workspace）和 TASK-805（MRTR）。

---

## TASK-800：rmcp 2.2.0 → 3.1.1，对外行为零变化

根 `Cargo.toml:33` 改 `rmcp = "3.1.1"`，然后编译器指哪打哪：

- `app.rs:52` `with_stateful_mode` → `with_legacy_session_mode`（语义已变：此后只作用于 `<2026-07-28` 请求，注释同步）；
- `hub.rs:414` `-> Result<CallToolResult, ErrorData>` → `CallToolResponse`，实现体末尾加 `.into()`；
- `hub.rs:373` `info.protocol_version = ProtocolVersion::V_2025_11_25` **保持不动**（rmcp 3.0 的 `LATEST` 仍是 `V_2025_11_25`，此处显式写死更稳）。

先跑 `cargo tree -e features -p rmcp` 确认 feature 集与 2.2.0 一致，再改代码——历史教训：feature 裁剪会静默砍能力。

**验收**：现有全部集成测试（`http_mcp.rs` / `http_session.rs` / `aggregation.rs` / `chaos.rs` / `perf.rs`）
里断言 `2025-11-25` 的行**一行不改**地通过。这一步不新增任何测试。

## TASK-801：`server/discover` 与双栈广告

- `supported_protocol_versions()` 与 `discover()` **都用 SDK 默认实现，不覆写**：
  默认已宣告 `KNOWN_VERSIONS` 全集，且默认 `discover()` 就是
  `DiscoverResult::from_server_info(versions, get_info())`，身份自动进
  `_meta["io.modelcontextprotocol/serverInfo"]`。收窄反而有害——见 ADR-019 决策 2；
- legacy `initialize` 路径完全不动——**这是硬约束**，实测证明回 `2026-07-28` 会让
  Claude Code 直接断连；
- 补 `Mcp-Method` / `Mcp-Name` 请求头与 `_meta` envelope 的校验路径（rmcp 3.x 已内建，
  确认它对缺失 envelope 返回 `-32020`/`-32021`/`-32022` 而非 500）。

**验收**：新增 `tests/http_discover.rs`——`server/discover` 返回两个版本且新版在首位；
带 `MCP-Protocol-Version: 2026-07-28` + 完整 `_meta` 的 `tools/list` 直接成功（无 initialize）；
缺 `_meta` 时返回 `-32020` 系错误码；同一进程上 legacy initialize 流程仍全绿。

**旧客户端回归（每个 TASK 都要过的横向验收）**：rmcp 3.1.1 的 `KNOWN_VERSIONS` 有五个版本，
测试须逐个覆盖 `2024-11-05` / `2025-03-26` / `2025-06-18` / `2025-11-25` 的 `initialize`
握手，并逐字节比对 `tools/list` 响应与升级前一致（新协议字段一个都不能漏进去）。

## TASK-802：无状态 Workspace 语义（ADR-019 修订 ADR-004 的落地）

当前 `HubService` 用 `Arc<OnceLock<PinnedIdentity>>`（`hub.rs:133`）把
`connection_id` + `workspace_id` pin 在 Session 上。新协议无 Session，改为：

1. `session_workspace()` 拆成两条路径——legacy 走原 pin 逻辑（ADR-004 原样有效）；
   新协议按 `connection_id` 每请求解析（Bearer Token 直证身份，无 Session-Id 可借用，
   `hub.rs:123-132` 那段防借用注释所描述的攻击面在新协议下消失）；
2. `tools/list` 等四个 list 结果带 `ttl_ms`(60s) + `cache_scope: private`（Workspace 隔离，
   不可被共享中间层缓存）；`resources/read` 也是 SEP-2549 的必填对象，带
   `ttl_ms: 0`（资源内容属于下游、变化无通知，Hub 无法为新鲜度作保）。
   **这两个字段只在 peer 协商到 `2026-07-28` 及以上时设置**——rmcp 对 `resultType`
   已按版本清理，但 `ttlMs`/`cacheScope` 是我们自己填的新增可选字段，必须自己按版本门控，
   否则 legacy 客户端会收到它不认识的字段（多数客户端忽略额外字段，但不能赌）；
3. ~~`tools/call` 校验工具所属 Revision~~ **无状态下做不到**：客户端不回传 Revision
   标识，服务端无从判断它手里是哪一版。工具名不存在时报 `unknown tool` 是已有行为；
   同名跨下游的残留窗口由 `ttlMs` + listChanged 约束，见 ADR-019「残留窗口」；
4. `mcpane__connection_info` 的 `sessionId` 在新协议下恒为 `null`，同步桌面端 UI 与
   `docs/interop/stateful-session.md`。

**验收**：新增测试覆盖「控制面中途改绑 Workspace → 新协议客户端下一次 `tools/call`
拿到明确错误而非静默串味」。按守卫可靠性要求，**必须反向验证**：故意把第 3 条的
Revision 校验改坏，确认该测试转红。

## TASK-803：`subscriptions/listen`

新协议下 GET SSE 流没了，`tools/list_changed` 必须走 `subscriptions/listen`。
项目已有的链路（下游 `*/list_changed` → Catalog 重建，`state.rs:163` / `state.rs:534`）
只差最后一跳：把重建结果推给上游客户端。

- 实现 `ServerHandler::accepted_subscription_filter` 与 `listen(SubscriptionContext)`：
  接受 `toolsListChanged` / `promptsListChanged` / `resourcesListChanged` 三类
  （Catalog 重建同时改变三张列表，三类治理行为必须一致）；
  per-URI 资源订阅（`resourceSubscriptions`）P0 不支持；
- 触发源不止 Catalog 重建：Workspace 改绑、访问控制启停都会改变单个连接的
  可见列表，`CatalogChange::ConnectionsChanged` 携带受影响连接集合精确通知；
- 通知按 Workspace 过滤——只推给受影响 Workspace 的订阅者；
- legacy 路径的 best-effort `tools/list_changed` 保持原样。

**验收**：新协议客户端 `listen` 后，控制面触发 Catalog 重建 → 收到带
`io.modelcontextprotocol/subscriptionId` 的通知；未订阅的连接收不到；跨 Workspace 不串。

## TASK-804：下游侧支持 2026-07-28 上游

`mcpane-hub-downstream` 建连改用 `serve_with_lifecycle` +
`ClientLifecycleMode::Auto { preferred_versions: [V_2026_07_28], legacy_version: Some(V_2025_11_25) }`：
先探 `server/discover`，证明是 legacy 服务端再回退 initialize，一套代码吃两代上游。

rmcp 3.x 默认开启客户端响应缓存（SEP-2549）。**核实结论：保持默认，不动。**
`default_ttl` 是 `Duration::ZERO`——不返回 `ttlMs` 的 legacy 下游立即过期，行为与
升级前一致；`serve_stale_on_error: true` 对 Hub 反而有益（下游临时故障时健康监控
不会把 Catalog 误清空）。`private_partition` 文档建议网关设置，但 Hub 的每条下游
连接用的是 Hub 自己的固定凭据、不随上游客户端切换 principal，故无需设置。

SSE 传输**不改**，仍走 `.serve()`：HTTP+SSE 自 2025-03-26 起就是弃用传输，
2026-07-28 根本不定义它，对它探测 `server/discover` 只是白费一个来回。

**验收**：`mcpane_devserver` 增加一个只讲 `2026-07-28` 的模式，验证 Hub 能聚合它；
现有只讲 legacy 的下游测试全绿。

## TASK-805：MRTR 透传

Hub 是聚合器：下游返回 `input_required` 时若不透传，任何用 elicitation 的下游工具在
Hub 后面就废了。因此要做，但依赖 TASK-802 的无状态基础，排在最后。

- `call_downstream_tool`（`hub.rs:648`）识别 `CallToolResponse::InputRequired`，
  把 `inputRequests` 透传给上游客户端；
- `requestState` 是客户端原样回传的**不可信值**，必须用 rmcp 的 `request-state` feature
  提供的 `RequestStateCodec`（HMAC-SHA256）密封，并把下游标识绑进 associated data，
  防止跨下游重放；
- 上游是 legacy 客户端时，下游的 `input_required` 无法表达 → 返回明确错误说明原因。

**验收**：`mcpane_devserver` 增加一个返回 `input_required` 的工具，验证新协议客户端能完成
两轮往返；篡改 `requestState` 后被拒；legacy 上游拿到可读错误。

发现模式（经 `mcpane__tool_execute`）同样支持完整往返：续轮字段用 rmcp 的
`RequestState` / `InputResponses` 提取器从外层请求取出后原样转交。发现模式
用在工具很多的场景，需要确认的工具在那里更常见，不能只支持首轮。

## TASK-806：互操作 Gate 与文档

按既有隔离方法重跑双端（不碰用户真实配置）：Codex 用隔离 `CODEX_HOME` +
`codex exec --json`；Claude 用 `claude -p --mcp-config <file> --strict-mcp-config --allowedTools`。
**新旧两条路径各跑一遍**。

Codex 稳定版尚未支持 `2026-07-28`（0.133.0 实测只有 `2025-06-18`/`2025-11-25`），
其新协议路径先用 rmcp 官方 conformance 套件代替，待 Codex 发版后补真机验证——
这条缺口要在验证报告里写明，不能让报告读起来像全覆盖。

文档更新：`AGENTS.md` 协议版本一节、`docs/interop/` 三份报告、
`docs/architecture/DEPENDENCIES.md`、`docs/adr/ADR-009` 标记为被 ADR-019 取代。

---

## 顺序与并行

TASK-800 → 801 → 802 必须串行（后者依赖前者的无状态基础）。
803 / 804 可在 802 完成后并行。805 最后。806 贯穿——每个 TASK 完成即补对应验证。

## 不做

Tasks 扩展、Roots / Sampling / Logging（新规范下已弃用，本就未实现）、
资源订阅（`resourceSubscriptions`）、SSE 跨实例 `EventStore`。
