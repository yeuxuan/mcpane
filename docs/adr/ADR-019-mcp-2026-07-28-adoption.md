# ADR-019：采用 MCP 2026-07-28 为主协议，历史版本长期并行

- 状态：Accepted
- 日期：2026-08-07
- 取代：ADR-009（P0 锁定 2025-11-25）
- 修订：ADR-004（Session-Pinned Workspace）——见「Workspace 语义的无状态等价物」

## 背景

ADR-009 在 2026-07-11 把协议锁在 `2025-11-25`，并写明两个重启评估的信号：
rmcp 3.0.0 stable 发布，或 Claude Code / Codex 任一正式支持 `2026-07-28`。

2026-08-07 复核，信号状态：

| 项目 | 状态 | 依据 |
|---|---|---|
| MCP 规范 | `2026-07-28` 已是当前正式版 | spec 仓库 `docs/specification/` 下最新目录，其后仅 `draft` |
| rmcp | ✅ 3.0.0 于 2026-07-28 随规范同日发布，现 **3.1.1**（2026-08-05） | crates.io `max_stable_version` |
| Claude Code | ⚠️ 客户端能力已内置，CLI 默认仍走 legacy | 见下「实测」 |
| Codex | ❌ 稳定版 0.133.0 未带；main 分支 HTTP 侧已实现 | 二进制字符串；openai/codex#37080 引用的 `ProtocolVersion::V_2026_07_28` |

### 实测（本机 claude 2.1.224，记录型假 MCP server）

1. 二进制内含 `2026-07-28` 43 处、`server/discover` 55 处，以及
   `versionNegotiation: { mode: 'auto' }`、`prior: { kind: 'legacy' }`、
   `subscriptions/listen`、MRTR `input_required` 的完整实现字符串；
2. 但 HTTP 与 stdio **两种传输实际都走 legacy 握手**
   （`initialize(2025-11-25)` → `notifications/initialized` → GET SSE → `tools/list`），
   从不主动探 `server/discover`；
3. **关键**：当服务端在 legacy `initialize` 响应里回 `protocolVersion: "2026-07-28"` 时，
   Claude Code **直接断连**，后续请求一律不发。

第 3 条是本 ADR 全部形态选择的硬约束：协议不能整体切换，只能双栈并行。

### 双栈对旧客户端无影响（同法实测）

把假 server 改成双栈形态（`server/discover` 宣告 `["2026-07-28", "2025-11-25"]`，
同时 legacy `initialize` 仍回 `2025-11-25`），再让同一个 Claude Code 2.1.224 连接：
请求序列与纯 legacy 服务端**一字不差**（`initialize` → `notifications/initialized`
→ GET SSE → `tools/list` → `tools/call`），工具调用正常返回。旧客户端根本不会看到
`server/discover` 的存在——它不问，服务端就不说。

## 决策

1. **rmcp 升到 3.x（基线 3.1.1）**，继续只用 crates.io 稳定发行版，不用 Git main 或 beta。
2. **`2026-07-28` 为 Hub 的主协议**：`server/discover` 宣告 rmcp
   `KNOWN_VERSIONS` 全集（五个版本），**不收窄**。
   实施期反向验证发现：一旦收窄 `supported_protocol_versions()`，
   `negotiate_protocol_version` 对集合外的请求版本会**静默回退**到
   `get_info().protocol_version` 而不是报错——请求 `2024-11-05` 的客户端
   会收到它没请求过的 `2025-11-25`，正是让 Claude Code 断连的那个场景。
   保持 SDK 默认的全集既最宽容，也避免了这个陷阱。
3. **历史版本长期并行，不预设退役**。rmcp 3.1.1 的 `KNOWN_VERSIONS` 含
   `2024-11-05` / `2025-03-26` / `2025-06-18` / `2025-11-25` / `2026-07-28`
   五个版本，与 2.2.0 **完全相同**——升级不移除任何一个历史版本。
   （2.2.0 里 `V_2026_07_28` 就已存在，但只是个能被识别的版本常量，
   没有对应的协议语义实现；3.x 才补上实现。）具体保障：
   - legacy `initialize` 响应**必须**继续回 `2025-11-25`（依据：上述实测第 3 条）；
   - `legacy_session_mode` 保持开启，但它此后只作用于 `<2026-07-28` 的请求；
     rmcp 3.x 对 `2026-07-28` 请求强制无状态，不受该开关影响；
   - 新协议引入的字段（`resultType` / `ttlMs` / `cacheScope`）**只发给协商到
     `2026-07-28` 及以上的 peer**，legacy 的 wire 形状逐字节不变；
   - `capabilities.tools.listChanged` 同理**只对新协议宣告**：它靠
     `subscriptions/listen` 兑现，而那是 2026-07-28 才有的方法。Hub 在 legacy
     路径上从不主动推 `tools/list_changed`（ADR-004 说的 best-effort 从未实现），
     宣告了却永远不推比不宣告更坏。实现上覆写 `initialize` 按协商版本裁掉该项。
4. **不设兼容层退役条件**。双栈的维护成本由 rmcp 承担（两条路径都是 SDK 内建），
   我们只是配置，没有需要通过砍旧版来收回的成本。将来若真要移除任何历史版本，
   须另写 ADR 并以锚定客户端的实际迁移情况为唯一依据，不看日历。
5. **不采纳**：Tasks 扩展（`io.modelcontextprotocol/tasks`）、Roots / Sampling /
   Logging（本就未实现，新规范下已进入弃用期，不补）。
6. **下游探测回退无条件**（2026-08-07 补，实测驱动）。Hub 作为客户端连下游时
   先探 `server/discover`；rmcp 的 `Auto` 只在收到标准 `-32601` 时才自动回退
   legacy `initialize`，而真实服务器的拒绝方式无法穷举——官方 TS SDK 在 HTTP
   层校验 `MCP-Protocol-Version` 头回 `400 + -32600`；learn.microsoft.com 对
   discover 回 `200 + SSE 内嵌 -32601`（rmcp 探测路径解析不了，落成
   ConnectionClosed）。两例都曾造成用户可见的「连接失败」。故 connect 层在
   探测**以任何方式失败**时一律回退 legacy 重试一次（stdio 会重新 spawn），
   两轮都败以 legacy 错误为主报告、附探测错误。如实记录代价：stdio 回退是
   **完整重跑一次进程生命周期**——启动脚本若有副作用（锁文件、遥测、抢占
   固定端口）会执行两遍，且首个进程的 kill 异步于第二次 spawn，存在短暂
   并存窗口。此类下游若因此偶发回退失败，日志里两轮错误都在，可据此定位。按错误类别设允许清单的方案
   已被否决：每漏一种形态就是一个新故障。回归测试按三种真实 wire 形态
   固化（HTTP 400、SSE 内嵌错误、stdio 粗鲁 -32600），另有 `#[ignore]`
   真机联网测试对 learn.microsoft.com。

## Workspace 语义的无状态等价物（修订 ADR-004）

ADR-004 的 Session-Pinned 依赖 `Mcp-Session-Id`，`2026-07-28` 下不存在。但 ADR-004
要防的东西（客户端持有过期 Catalog 却被路由到新 Revision，撞 unknown tool 或同名工具
静默串味）依然真实，只是新规范给了更好的工具：

| ADR-004 的手段 | 无状态等价物 |
|---|---|
| initialize 时 pin Revision 到 Session | 按 `connection_id`（Bearer Token 派生，ADR-003 已把 Token 与 Session 分离）每请求解析；控制面不改绑时结果恒定，等价于 pin |
| pin 时存 `connection_id` 防 Session-Id 借用 | **不再需要**——无 Session-Id 可借用，身份每请求由 Token 直接证明 |
| best-effort `tools/list_changed` | `subscriptions/listen`（SEP-2575）：客户端显式 opt-in、服务端 ack、通知带 `subscriptionId`，是可靠通道而非 best-effort |
| 「切换对新 Session 生效」 | `ttlMs` / `cacheScope`（SEP-2549）显式告知客户端缓存有效期，到期自然重拉 |
| Revision 绑定后不得悄悄改路由 | 工具名在当前 Revision 不存在 → `unknown tool` 明确报错（已有行为，不静默）。**但服务端无法校验客户端手里是哪一版 Revision**——见下方「残留窗口」 |

### 残留窗口（如实记录）

无状态协议下**客户端不回传任何 Revision 标识**，`_meta` 由客户端自行填写，服务端
无权要求它带上服务端的版本戳（`requestState` 只在 MRTR 往返内有效）。因此存在一个
ADR-004 的 Session-Pin 能挡、而新协议挡不住的场景：

> 同一个工具名在新旧 Workspace 里被 alias 到**不同下游**。客户端在改绑前拿到工具名，
> 改绑后调用，会打到新 Workspace 的那个下游。

约束这个窗口的三件事：`ttlMs`（默认 60s，缓存兜底）、`subscriptions/listen` 的
listChanged（即时通知，TASK-803）、工具名不存在时的 `unknown tool` 明确报错。
窗口 = 改绑发生到客户端收到通知或缓存过期之间。这是无状态协议的固有代价，不是本
实现的缺陷；规范正是用 SEP-2549 缓存契约来替代会话级 pin 的。

**这里有个坑值得写下来**：上面这段话成立的前提是「改绑真的会发通知」。第一版
实现里它不会——广播只在 Catalog 重建时发，而改绑走的是
`set_connection_workspaces`，只改映射不重建任何 Catalog。于是最典型的场景
（ADR-004 通篇讲的那个）恰恰是唯一收不到 listChanged 的，客户端只能干等 60s。
订阅端也只在建立时解析一次 Workspace，改绑后会一直按旧 Workspace 过滤。
两处都已修，且第二轮审查又发现**访问控制启停是同一类病**（撤销某连接对某
Server 的访问 → 该连接列表立即变，但不触发任何重建）。最终形态：广播载荷为
`CatalogChange::{Rebuilt(ws), ConnectionsChanged(受影响连接集合)}`——改绑与
访问控制的 setter 都自己算 diff（新旧映射都在它手里，成本最低），订阅端只查
自己在不在集合里，命中则重算 Workspace 并推送。幂等操作（重复拒绝同一个
Server）不产生变化、不广播。守卫：`rebinding_a_connection_notifies_its_subscriber`、
`access_control_change_notifies_only_the_affected_connection`。

通知按客户端接受的类别逐个推（tools/prompts/resources 三类）：Catalog 重建
同时改变三张列表，只推 tools 违反「三类治理行为必须一致」的既有原则。

净效果：listChanged 从 best-effort 变成协议级可靠通道，`ttlMs` 让缓存有了明确契约；
代价是上述同名跨下游窗口。ADR-004 的条款在 legacy 兼容层内继续原样有效。

## 理由

- rmcp 3.0 里 `ProtocolVersion::LATEST` **仍是** `V_2025_11_25`，升 SDK 本身不改变
  对外行为——这让「升 SDK」与「开新协议」可以拆成两步独立验证，是最低风险的路径；
- Claude Code 的 `versionNegotiation: 'auto'` 一旦被 CLI 默认开启，先探 `server/discover`
  的客户端立刻出现。届时兼容层能兜住，但只有提前实现 `server/discover` 才能真正吃到
  无状态带来的收益；
- 市面已出现只讲 `2026-07-28` 的服务端（如 googleapis/mcp-toolbox 的 Go SDK 默认即新版），
  下游侧不升级会直接聚合不了这类上游；
- 无状态核心与 Hub 的定位天然契合：Hub 本就按 Token 而非 Session 判定身份，
  去掉 Session 后少一层可被借用的状态（ADR-004 里那段防借用逻辑随之消失）。

## 后果

- 需要为 `2026-07-28` 路径新建一整套互操作与协议测试；现有断言 `2025-11-25` 的测试
  **一行不改**地保留，作为兼容层的守卫；
- `HubService` 不再依赖 `OnceLock` pin，`session_workspace` 的双重校验在新协议路径下
  被 Token 直证身份取代；
- `mcpane__connection_info` 的 `sessionId` 在新协议下恒为 `null`，UI 与文档需同步；
- rmcp 3.x 的 `request-state` 是 opt-in feature，MRTR 落地前须先确认 feature 开关
  （历史教训：feature 裁剪会静默砍能力）；
- 两代协议并行期间，任何协议层改动都要在双栈上各验一次。
