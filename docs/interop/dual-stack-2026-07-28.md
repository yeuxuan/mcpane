# TASK-806：MCP 2026-07-28 双栈互操作验证报告

- 日期：2026-08-07
- 依据：[ADR-019](../adr/ADR-019-mcp-2026-07-28-adoption.md)
- 结论：**Claude Code 双路径通过；Codex 未能真机验证（见「缺口」）**

## 环境

| 项目 | 值 |
|---|---|
| Hub | mcpane-hub 0.2.1（rmcp 3.1.1，有状态 legacy + 无状态 modern 并行） |
| Claude Code | **2.1.224**（npm 发布版） |
| Codex CLI | 0.133.0（**未能完成验证**） |
| OS | macOS（Darwin 25.6.0，Apple Silicon） |
| URL | `http://127.0.0.1:24880/mcp` |

Hub 以隔离 `MCPANE_HOME`（临时目录）启动，未触碰用户真实数据库；
Claude 用 `--mcp-config <file> --strict-mcp-config`，未触碰用户真实配置。

## 1. legacy 路径（真实 Claude Code 2.1.224）

`mcpane__connection_info` 原样返回：

```json
{"clientConnectionId":"conn_dev_claude","clientType":"claude",
 "requestId":"req_85dfe36aba19413687757324d84119aa",
 "sessionId":"15ea2b05-040b-417b-a72a-ce544db482d8",
 "workspaceId":"ws_default","catalogRevision":"rev_d5c5e93504044e8a998ce016ee1712d0",
 "protocolVersion":"2025-11-25","serverVersion":"0.2.1"}
```

- 协商版本仍是 `2025-11-25`，**升级未改变旧客户端看到的协议**；
- `sessionId` 非空——有状态 Session 语义未受影响；
- 请求顺序与升级前一致（initialize → notifications/initialized → GET SSE → tools/list → tools/call）。

## 2. 新协议路径（`2026-07-28`，无 initialize）

Claude Code 2.1.224 虽内置了 2026-07-28 客户端实现，但其 CLI 默认不走该路径
（实测见 ADR-019），故用 curl 以新协议语义直接驱动。

**`server/discover`**：

```json
{"resultType":"complete",
 "supportedVersions":["2024-11-05","2025-03-26","2025-06-18","2025-11-25","2026-07-28"],
 "capabilities":{"prompts":{"listChanged":true},"resources":{"listChanged":true},
                 "tools":{"listChanged":true}}}
```

（快照为设计审查补齐三类 listChanged 后重抓，与当前代码一致。）

**无 initialize、无 Session 直接 `tools/call`**：

```json
{"clientConnectionId":"conn_dev_claude","clientType":"claude",
 "requestId":"req_c56bf617b65d47f2b66a6c0a23c428dd","sessionId":null,
 "workspaceId":"ws_default","catalogRevision":"rev_d5c5e93504044e8a998ce016ee1712d0",
 "protocolVersion":"2026-07-28","serverVersion":"0.2.1"}
```

- 五个历史版本一个不少，新版在列；
- 三类 `listChanged` 只在新协议侧宣告（legacy 的 initialize 响应里三类都没有，见测试）；
- `sessionId` 为 `null`、`protocolVersion` 为 `2026-07-28`——无状态语义生效。

## 3. 自动化覆盖

`cargo test --workspace`：**627 passed / 0 failed**，`cargo clippy --workspace --all-targets`：0 告警。
（本报告首版记录 622；此后两轮设计审查补了改绑通知、访问控制通知、三类 listChanged、
`resources/read` 必填缓存字段与发现模式 MRTR，测试数随之增长。）

协议相关的守卫（`tests/http_discover.rs`、`tests/aggregation.rs`）：

| 守卫 | 反向验证 |
|---|---|
| 四个历史版本 initialize 逐一通过、协商结果不被改写 | 收窄 `supported_protocol_versions()` → 转红 |
| legacy 响应不含 `resultType`/`ttlMs`/`cacheScope` | 把已存在的键加进黑名单 → 转红（确认遍历真的穿透嵌套） |
| Catalog 重建经 `subscriptions/listen` 推到订阅者 | 掐掉广播 → 超时转红 |
| 能聚合只讲 2026-07-28 的下游 | lifecycle 退回 `Initialize` → 转红 |
| MRTR `requestState` 重新密封、跨下游不可重放 | 去掉重新密封 → 精确捕获 `pending:delete` 原文泄漏 |
| Workspace 改绑即时通知订阅者 | 改绑不广播（首版实现）→ 超时转红 |
| 访问控制启停即时通知（含隔离与幂等断言） | 掐掉 `set_server_denied` 的广播 → 转红 |
| 四个 list + `resources/read` 的缓存契约只发新协议 | 三个 list 端点 legacy 泄漏断言 + read 双栈断言 |

## 缺口（不要把本报告读成全覆盖）

**Codex 0.133.0 未能真机验证，legacy 与新协议两条路径都没验。**
`codex exec` 在本机隔离 `CODEX_HOME` 下运行 5 分钟无响应后被终止，
Hub 侧日志显示**未收到任何请求**——它在自己那侧就卡住了（本环境很可能缺少
登录态），与协议改动无关，但这条验证确实没做成。

现有的间接证据：Codex 走 `2025-06-18` 生命周期，而该版本的 initialize 与
`tools/list`/`tools/call` 已由 `every_legacy_version_still_initializes_unchanged`
逐字节覆盖（含"响应里不得掺入新协议字段"）。这不能替代真机验证。

**待办**：Codex 可用后补跑 legacy 路径；其新协议路径需等 Codex 稳定版支持
`2026-07-28`（openai/codex#33952 仍 open）。
