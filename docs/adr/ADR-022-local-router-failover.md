# ADR-022：本地路由——客户端接管、故障转移与协议转换

- 状态：Accepted（2026-09-24 二期修订：协议矩阵补全、整流、临时接替、请求日志；
  2026-09-30 三期：额度优先排序，见 §9；2026-10-04 四期：流结果记账、超限归一、工具回合配对，见 §10）
- 日期：2026-09-23
- 关联：ADR-014（本 ADR 推翻其 §1「不建代理」与 §7「不移植自动故障转移」）、
  ADR-013、ADR-012（loopback 网关 token）、ADR-017（出站代理，上游请求照走 `mcpane-net`）

## 背景

cc-switch 的「路由」是本地代理接管：客户端配置指向 `127.0.0.1`，由代理转发给供应商，
并在转发循环里做故障转移、熔断、协议转换。ADR-014 当时判断「无代理架构下不存在等价
移植路径」，因而不做故障转移；Codex 2026-02 移除 `wire_api = "chat"` 后，只有 Chat
端点的厂商（千帆、硅基流动、DeepInfra、Mistral…）也随之无法接 Codex。

用户明确要求补齐路由能力，并要求比 cc-switch 更成熟。MCPane 已有 Claude Desktop 用的
loopback 网关（`mcpane-desktop-proxy`，进程内托管），缺的只是客户端命名空间与转发策略。

规模上不追求对齐 cc-switch 的约 5 万行：其中约 1.3 万行是 Copilot / xAI / Codex OAuth 与
Copilot 优化（官方登录不经路由，不做），另有大量内联测试与重复的 SSE 信封代码。目标是
每条协议路径都有、每个真实故障场景都有兜底、每项都有真实上游回归。

## 决策

### 1. 单端口、按路径分命名空间

复用 Claude Desktop 网关的端口与 token（`DesktopGateway`）：`/claude-desktop`（原有）、
`/claude`、`/codex`、`/gemini`、`/grok`。网关 token 接受 Bearer / `x-api-key` /
`x-goog-api-key`（各客户端的原生携带方式）；上游凭据按供应商形态注入。crate 名保持
`mcpane-desktop-proxy`，避免纯改名的连锁改动。

### 2. 接管 = 改写 live 的地址与凭据，其余照旧

`ProviderState.routing[app] = { enabled, failover_queue }`。开启且当前为中转站时，
切换写 live 的地址为 `http://127.0.0.1:{port}/{命名空间}`、凭据为网关 token；
模型、附加片段等仍取当前供应商。真实地址与 Key 只进路由内存快照。

- **回填防污染**：live 地址是路由地址时，回填跳过 base_url / Key / Codex wire_api；
- **官方登录不经路由**（账号风控风险）：配置照存，live 不动；
- **退出还原直连、启动重新接管**；依赖协议转换的供应商例外（离开路由本就不可用）；
- **不变量「live 指向路由 ⇒ 路由在跑」**：每次同步后若仍有接管需求却没在监听（端口被占等），
  立即把接管中的客户端回退直连并在界面报原因；开启路由失败时连开关一并恢复。
- 运行中改端口：新端口绑定成功后才停旧实例，并重写所有指向路由的客户端配置；失败回滚端口。

### 3. 故障转移、熔断与临时接替

候选 = 当前供应商 + 队列（同 app 中转站）。只在拿到响应头之前转移，流开始后不中途切换。
可重试判定对齐 cc-switch：连接失败、首字节超时、除 400/405/406/413/414/415/422/501 以外的 4xx/5xx。

- **熔断**：连续 4 次软故障，或最近 20 次里失败率 ≥60%（至少 10 次样本；滑动窗口，比
  cc-switch 的累计计数更能抓住间歇抖动的「半坏」站点）→ 暂停 60s → 半开探测（同一时刻只放
  一个探测，其余请求先走备用，探测被取消时名额自动释放），成功 2 次恢复；**连接失败 / 首字节超时
  立即熔断**（挂起型故障不该让每个请求白等 60s）。连接层错误先对同一候选换条连接重试一次
  （多为对端刚关闭的 keep-alive 连接），仍失败才计入。
- **临时接替**（比 cc-switch 更好）：cc-switch 在转移成功后静默改掉用户的当前供应商；我们
  不改用户选择，而是让成功接手的备用在 60s 窗口内优先服务，主站到期经半开探测自动回切，
  界面显示「临时由 X 接替」，手动恢复主站即结束接替。
- **超时分层**：首字节 60s 只管流式（非流式整轮生成才回头，改用 600s 总时限，实测曾误伤长回答）；
  流开始后两道看门——120s 无任何字节，或 300s 只有保活心跳（`: PING` / `event: ping`）无真实数据，
  即中止（实测网关心跳会让单一空闲看门永不触发，流挂 25 分钟）。
- **按 app 调参**：首字节超时（10–600s）、无数据中止（30–1800s）、连续失败次数（1–20）、暂停时长
  （10–3600s），存于 `RoutingConfig.tuning`，旧数据缺省取默认；托盘每个 app 子菜单有本地路由开关。
- **供应商级请求覆盖**（作用于协议转换后的上游请求）：请求头复用 `headers`（鉴权、编码、连接类
  受保护头不可覆盖），请求体用 `route_body_override`（JSON Merge Patch，null 删键，`stream` 不可改），
  补客户端协议表达不了的厂商参数（如 `enable_thinking`）。
- 转移到备用时改写请求模型：Claude 按档位映射，Codex / Grok 换成备用的主模型，Gemini 改写
  URL 路径里的 `models/{model}` 段。

### 4. 请求整流（对同一候选修正后重试一次，每类至多一次）

- **thinking 签名**：转移到另一家 Anthropic 端点后，历史里前一家签名的 thinking 块被拒 →
  剥离 thinking / redacted_thinking 与残留 signature 后重试。识别规则比 cc-switch 保守：
  不以泛化的 "invalid request" 触发。
- **thinking budget**：budget 过小或 max_tokens 不大于 budget → 最小修正后重试。
- **纯文本模型收到图片**：上游报不支持图片 → 图片换文字占位后重试。
- **输出上限超出**：从报错里解析上限（各家措辞不同）→ 按上限重写 max_tokens / max_output_tokens，
  thinking 预算随之压到上限以下后重试。转移到输出上限更小的模型时不再直接失败。
- **思考强度不被支持**：xhigh / max 降为 high，常规档仍被拒则去掉强度后重试。
- **严格 Responses 网关拒收 Codex namespace 工具**：拍平后重试，响应侧（JSON / SSE）还原
  name + namespace。
- 所有上游请求一律 `accept-encoding: identity`（reqwest 未开解压，转换与用量旁路都读明文）；
  上游无视它仍压缩时，转换路径直接报明原因，而非 JSON 解析失败。
- 接口地址已是完整端点或已带 `/v1` 时不重复拼接；Codex → Anthropic 剥离 Codex 指纹头
  （originator / session_id / x-codex-* 等）并把 `Accept` 规范为 `application/json`（严格网关回 406）。

### 5. 协议转换矩阵

均只在路由开启时可用（关闭路由时切换被拦下；当前供应商依赖转换时禁止关闭路由）：

| 客户端 ＼ 上游 | Anthropic Messages | OpenAI Chat | OpenAI Responses |
|---|---|---|---|
| Claude Code / Desktop | 直通 | `claude_chat` | `claude_responses` |
| Codex | `codex_anthropic` | `codex_chat` | 直通 |

对照 cc-switch 对应模块移植核心语义（工具 namespace / custom 拍平与还原、reasoning 往返、
内联 `<think>`、usage、`message_delta` 全流一次、上游报错只发错误事件、`count_tokens` 本地估算）。
reasoning 的不透明载荷以带版本前缀的 JSON 文本借 signature / encrypted_content 往返。
Chat 转换中工具结果里的图片挪到紧随 tool 消息之后的合成 user 消息。
思考强度：Claude 侧按 `output_config.effort`（`/effort`）优先、其次 `thinking`（adaptive 取 high——xhigh
只有少数模型支持）；只对 o 系列 / GPT-5+ / Grok 4.5+ 写 reasoning effort，其中 OpenAI 推理模型在 Chat
上用 `max_completion_tokens`（cc-switch 只认 o 系列）。Codex → Anthropic 清洗压缩 / 中断留下的不完整
工具回合、注入提示词缓存断点（否则长会话每轮全价重发 system / tools / 历史）、流被截断时未完成的
工具调用标为 incomplete 而非交给 Codex 执行半截参数。Responses 终态里内嵌的 failed / cancelled /
error 按失败处理，被截断的回合报 max_tokens 而非 tool_use。
Claude → Gemini 原生**不做**：Google 官方提供 OpenAI 兼容端点，已被 Claude → Chat 覆盖。

### 6. 用量与请求日志

路由不计费：token 由客户端照常写进会话文件（转换路径同样回填 usage），再计会双重计数。
路由记录会话文件缺失的维度：每请求的服务方、故障转移 / 整流经过、首字节与总耗时、用量
（在客户端侧响应上旁路解析，不改字节），内存保留最近 500 条，并按供应商累计，界面展示。
客户端在收到响应前断开（如用户按 Esc 中断）时，记录由守卫收尾为失败，不会永远停在「进行中」。
日志可按「仅失败」筛选、可清空（清空只删记录，供应商累计用量保留）。

### 7. 迁移

从 cc-switch 迁移时带上故障转移队列（`in_failover_queue` + `sort_index`，旧 schema 视为不在队列），
只追加不改用户已有顺序，**不自动开启路由**（开启会改写客户端配置，需用户显式操作）。
供应商 `meta.localProxyRequestOverrides` 迁为 `headers` / `route_body_override`；`proxy_config` 里调过
（非默认）的超时 / 熔断参数截到允许范围后迁入，MCPane 里已调过的不覆盖。迁移后刷新路由快照。
云同步与请求头同口径摘除 `routeBodyOverride`（可能含鉴权参数）。

### 不做

Copilot / xAI OAuth、代理侧 token 计费（见 §6）、Gemini 协议转换、跨轮 reasoning 缓存、
Copilot 空白死循环检测、Moonshot `$ref` schema 改写（仅 Codex Desktop 触发）与其余厂商特化分支。
cc-switch 的 thinking_optimizer（按模型主动把 effort 拉到 max、强加 1M 上下文 beta）也不做：它改写
用户的显式意图并抬高费用。cc-switch 可调 9 项参数，我们只开放 4 项（首字节超时、无数据中止、
连续失败次数、暂停时长），错误率窗口、半开成功次数、非流式总时限保持内置——多数用户碰不到，
多一个旋钮多一处误配。

### 8. 接管的崩溃自愈

启动时先按 **live 内容**（而非开关）检查：未开路由的 app 其 live 仍指向本地路由（关路由时开关
已落盘、live 未改回就被强杀 / 断电）→ 按直连重写当前项。随后再对开了路由的 app 重新接管；
接管失败时客户端保持直连，原因经 `last_error` 显示在界面上，直到用户重新开关路由或接管成功。

### 9. 三期：额度优先排序（2026-09-30）

调研 magpie（见 `docs/competitors/REFERENCES.md`）后只吸收其中站得住的一项：让**已有的额度数据**参与候选排序。

- `RoutingConfig.strategy`：`failover`（默认，行为零变化）/ `quota-aware`。后者在熔断过滤之后对「额度已用尽」
  的候选做**稳定后置**；全部用尽时顺序不变，不因额度信号锁死用户；临时接替者已用尽则不提前。
- 「用尽」的判定（`ProviderQuota::exhausted_until`）只认确凿数据：任一窗口 ≥100% 且重置时间未过，
  或全部余额 ≤0。查询失败、无数据、重置时间已过（陈旧）一律视为无信号，宁可多试一跳也不误伤可用的供应商。
  无重置时间的用尽态信号寿命 15 分钟（后台刷新周期 5 分钟 × 3），刷新不到就自行过期。
- 信号由 `quota_cache::record`（额度写入的唯一收口）推给路由，请求路径上只读内存，不发额度查询。
  后台刷新范围扩到开启了额度优先的路由候选（仅中转站；官方订阅额度已整体停用，任何路径都不请求官方接口）。
- **不做**（评估后否决，勿翻案）：
  - 多 Key / 多账号数据模型与会话粘性——「多个 Provider 各配一把 Key 放进故障队列」已能表达，会话粘性
    只在多 Key 轮转时才有意义；
  - 订阅账号的额度调度——官方登录不经路由是本 ADR §2 的风控红线；
  - magpie 的 Claude 订阅桥接（驱动本地 `claude` 二进制并伪造 billing header / 设备指纹以绕过第三方流量识别）。

### 10. 四期：流结果记账、上下文超限归一、工具回合配对（2026-10-04）

二次调研 magpie（见 `docs/competitors/REFERENCES.md`）后，对照我们的代码核实确认并补齐三处真实缺口。

**流结果记账与首事件转移**（修订 §3 的「只在响应头之前转移」）

- **问题**：旧实现在响应头 2xx 时就记成功。中转站「先回 200、再在流里报错或断流」时，失败只进请求日志，
  熔断永远不触发，也不会转移。
- **转移窗口 =「响应头 + 首个 SSE 事件」**：还有后备候选时，在向客户端写出任何字节之前窥探首个事件
  （最多 `FIRST_EVENT_WINDOW`=2 秒）；首事件是换家可能成功的错误（限流、过载、服务端错误、鉴权失败）则按
  软故障转下一个；窗口内没有事件就照常放行，慢不是失败。窥探到的字节原样接回流前。只对
  `text/event-stream` 窥探；最后一个候选不窥探（没有后备，白等只增延迟）。**流开始后仍不中途切换。**
- **识别在「客户端看到的那条流」上做**（`stream_error.rs`）：它已是客户端协议的形态，转换路径与直通路径同一套逻辑，
  不改任何转换器。认 Anthropic `type:"error"`、Responses `type:"error"` / `response.failed`、Chat 与 Gemini 的
  顶层 `error` 对象（且无 `choices`）。请求本身的错误（参数无效、对话装不下）换哪家都一样，不算供应商的失败。
- **记账**：`Tap` 在流结束时给出结局——读完记成功；读流出错、被空闲看门中止、流中出现可重试错误事件记软故障；
  流中请求类错误不记；**客户端提前断开（如按 Esc）既不记成功也不记失败**。流式成功改为流读完才记，
  这是必须同时做的：否则「响应头成功重置连续失败计数」会让交替出现的头部成功把计数永远清零，熔断形同虚设。
  半开探测沿用响应头记成功（探测名额在函数返回时释放，不能悬到流结束），其流后续失败仍会重新熔断。
  非流式仍在响应头记成功。

**上下文超限错误归一**（`overflow.rs`）

- Claude Code 靠错误里的「prompt is too long」、Codex 靠错误码 `context_length_exceeded` 判定对话装不下并自动压缩。
  各家中转站措辞五花八门，客户端认不出就不压缩，会话卡在这条错误上。
- 在唯一回写点 `error_response` 把确凿的超限错误改写成客户端自己的 API 会说的形态：Claude → 400
  `invalid_request_error` 且 message 带「prompt is too long」前缀；Codex / Grok → 带
  `code:"context_length_exceeded"` 的 OpenAI 形态；Gemini 与已经良构的错误不动。413、422 一并归成 400。
- **宁可漏判不误判**：只说 `max_tokens` 的（回复额度，压缩无益）和限速类措辞一律不判——把限速当超限，
  Claude Code 会压缩了再压缩，直到判定空转而放弃。排在 `try_rectify` 之后，输出上限修正优先。
- 措辞表按各厂商公开文档整理，不是 magpie 代码的移植。**未用真实客户端验证**自动压缩确实被触发。

**Codex→Chat 工具回合配对**（`codex_chat.rs::pair_tool_messages`）

- 压缩与中断会留下孤立的工具输出和没应答的调用。严格的 Chat 上游（Kimi：`tool_call_id is not found`）直接 400，
  而 Codex 每次重试都重发同一段历史，会话永久卡住。
- 发出前修补：工具消息按调用顺序排列；同一调用的重复应答只留第一条；应答不存在 / 已关闭 / 空 id 调用的输出
  降级为 user 文字（不凭空造调用、不重复用 id）；没应答的调用补占位结果。良构回合逐字节不变。
- 只修 Codex→Chat：Claude Code 的 Anthropic 消息自带成对约束，Anthropic API 本身就会拒绝不成对的历史。

## 验证

- 单元 / 集成测试覆盖每个转换器、整流、熔断、临时接替、日志、陈旧连接重试，关键守卫逐项反向验证。
- 真实上游回归套件 `crates/mcpane-desktop-proxy/tests/real_provider.rs`（`#[ignore]`，Key 只从
  `MCPANE_REAL_KEY` 读取，须串行）：每条「客户端协议 × 上游协议」× 流式 / 非流式 × 工具调用往返 × 故障转移。

## 影响

- `crates/mcpane-provider`：`RoutingConfig`、`ClaudeApiFormat`、`CodexWireApi::Anthropic`；
  `set_routing` / `restore_direct` / `reapply_routing` / `append_failover_candidates`。
- `crates/mcpane-desktop-proxy`：`router`、`breaker`、`rectify`、`media`、`namespace`、`request_log`、
  `codex_chat`、`codex_anthropic`、`claude_chat`、`claude_responses`、`effort`、`sse`。
- `crates/mcpane-ccswitch`：迁移故障转移名次。
- 桌面：`desktop_proxy.rs`（同步不变量、端口热切换）；供应商页「本地路由」区块（临时接替、
  启动失败提示、用量、最近请求）；编辑器「接口协议」；Codex 预设恢复 8 条仅 Chat 端点的厂商。
