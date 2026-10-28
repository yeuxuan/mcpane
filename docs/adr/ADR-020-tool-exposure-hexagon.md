# ADR-020：按客户端自适应的工具暴露（目录直给 + 发现即升格 + 兜底自愈）

- 状态：Accepted（决策 1 的 auto 判定已由 ADR-021 修订：增加工具数维度）
- 日期：2026-08-08
- 关联：ADR-019（2026-07-28 双栈）、migrations/0008（workspace 级 tool_discovery）

## 背景

对比实测网关模式（tool_discovery + tool_execute）与平铺模式（Claude Code 自带
ToolSearch 延迟加载）暴露了网关模式的两个结构性缺陷：

1. **调用准确率**：`tool_execute` 的入参是通用 `{name, arguments}`，目标工具的
   真实 schema 不参与模型的结构化生成——实测缺必填字段照样发出，靠下游事后拦截；
2. **检索可达率**：BM25 关键词检索对中文查询假阴性（「网页抓取」对
   firecrawl_scrape 返回空 matches 且不报错），模型误判「没这个工具」直接放弃。

调研结论（SEP-2636、2026-07-28 Client Best Practices、Anthropic Tool Search
Tool 的准确率数据、Claude Code #66084）收敛为三个可分离的轴：

- **准确率来源** = 调用生成那一刻模型手里有真实 schema。分级：
  ① 原生 typed 调用（生成期被约束）＞ ② execute 但 schema 已在上下文 ＞ ③ 裸奔。
- **可达率来源** = 模型对完整工具名单有视野、自己挑（与平铺同构）。任何服务端
  检索器都是概率性的，放在关键路径上可达率就从确定性退化为概率。
- **成本结构** = 目录（名字+一行描述）是 O(n) 小常数，schema 是 O(k)（实际用到
  的工具数）。贵的是 schema，不是名字。

## 决策

1. **per-connection 暴露模式** `tool_exposure_mode: flat | gateway | auto`
   （默认 auto，`client_connections` 加列，无 CHECK，枚举收敛应用层）。
   有效模式解析：显式 flat/gateway 直接生效；auto 下 **仅精确 `client_type ==
   "claude"`（Claude Code）走 flat**——它是唯一实测确认自带延迟加载的客户端；
   其余客户端沿用 workspace 级 `tool_discovery` 开关。既有用户行为不变，唯一
   变化是发现模式工作区里的 claude 连接改走平铺（正是目标改进）。
   `claude-desktop` / `gemini-cli` 等在 `ClientType::parse` 落 `Custom`，
   不享受 auto→flat：未实测的能力不预设。
2. **目录直给替代检索**（检索移出关键路径）：
   - 网关模式 instructions 附全量目录（server 分组，工具名 + 100 字符截断描述）。
     instructions 是否注入取决于 host，属尽力而为通道；
   - `tool_discovery` 的 `query` 改可选：无参/空串 = 浏览模式返回全量目录；
     **检索零命中时无条件降级为全量目录，永不返回空**（确定性通道）；
   - 目录条目不带 inputSchema——全量带 schema 就退化成平铺，目录经济性归零。
     检索命中路径保留 schema 内联。
3. **`mcpane__tool_describe(name)` 精确取 schema + 发现即升格**：精确匹配返回
   完整定义；网关形态下命中即把该工具加入本连接的 tools/list（内存态升格集，
   单连接上限 256），并经既有 `CatalogChange::ConnectionsChanged` →
   `subscriptions/listen` 推 `tools/list_changed`（仅 2026-07-28 路径；legacy
   按 ADR-019 不推）。支持刷新的客户端此后原生 typed 调用（准确率 ①）。
   升格集不落库：重启即清，模型重新 describe 一次即恢复；catalog 重建后失效的
   名字由 list_tools 读取时对当前 Revision 惰性过滤。打错名字返回 BM25 最近似
   候选，不是死胡同。
4. **execute 兜底自愈**（准确率 ② 的下限保障，对不刷新工具列表的客户端这是
   服务端能给的理论上限）：Hub 对 execute 路径做本地轻量校验（仅 required
   缺失 + 顶层类型不符；不碰嵌套/组合子/$ref/enum）。**校验失败不拦截、照常
   转发**——校验器可能因 schema 方言/漂移误杀，下游才是最终裁判（回退无条件
   原则，同 ADR-019 决策 6）。仅当下游返回失败且本地校验有发现时，在响应里
   附校验明细 + 完整 inputSchema，模型一轮自愈。直连调用路径逐字节不变
   （双栈守卫约束）。
5. **workspace 级 `tool_discovery` 开关保留**，语义收窄为「auto 模式下非 claude
   客户端的默认形态」。
6. **主动性轴（2026-08-08 追加）**：可达率/准确率之外的第三轴是「模型肯不肯用」。
   Hub 的杠杆是 instructions（建议性通道，host 系统提示词权重更高）：
   - 内置教义：两种形态的 instructions 都注入「能力范围内优先调用工具、
     不凭记忆作答」的使用准则；
   - 用户自定义准则：`workspaces.usage_guidance`（0020 迁移），Workspace 页
     编辑，Hub 原样注入 instructions——把各客户端 CLAUDE.md/AGENTS.md 里的
     MCP 路由规则下沉到 Hub，一处配置全客户端共享。改动对新会话生效；
     state 的 setter 本身不广播 listChanged（三类列表未变），但桌面保存
     复用既有的 ReloadServers 路径（与 tool_discovery 开关同路），随例行
     Catalog 重建会广播 Rebuilt——代价是订阅客户端多一次无实质变化的重拉，
     换的是不为低频操作另铺一条控制面通道。
   主动性只能用提示词工程逼近、用行为评测验证（interop 清单第 5 项），
   无协议级保证——这是该轴与前两轴的本质区别，如实记录。

7. **下游 schema 入库规范化（2026-08-08 真机测试后追加）**。生态实证三类
   「规范合法但客户端吃不下」的 wire 形态会造成灾难（claude-code#25081 静默
   丢弃整张工具表、#45106 顶层组合子让会话永久 400、#10031 顶层缺 type）；
   社区通行解法是在客户端与 server 间手写 stdio 代理做规范化——Hub 站在聚合
   位上内建它（`mcpane-hub-catalog/src/sanitize.rs`，`build_revision` 入口
   一次处理、全出口受益）：布尔 schema 对象化与顶层补 type 无损；顶层
   anyOf/oneOf 降级（properties 并集 + required 交集）**有损**，必发 warn。
   `additionalProperties` 的布尔形态是生态惯例，刻意不动。
8. ~~**Codex 一键接入写入 `default_tools_approval_mode = "approve"`**~~
   **（2026-08-31 撤销：该键 Codex 不认）**。真机复核 Codex 0.133 的
   `--strict-config` 报 `unknown configuration field`——非严格模式静默忽略、
   严格模式会让用户的 Codex 起不来。`approval_policy = "never"` 同样无效
   （它管 shell 审批，不管 MCP 工具）。headless 取消的真正根因是缺
   `ToolAnnotations`（`docs/interop/codex-direct-http.md` 早有记载），已由
   内建工具补全注解治本。**存量由 `hub_entry_patch` 的 `PatchOp::remove` 在
   接入路径上清理**——只停止写入不清理，等于把垃圾留在用户配置里。
   原决策记录如下：
   tool_execute 如实申报 destructive 会进 Codex 审批路径，headless
   `codex exec` 无法弹审批一律自动取消（openai/codex#24135，真机复现）——
   不预放行等于网关模式对 headless Codex 不可用。信任边界在「用户自己的
   Hub + 用户自己挑的下游」，逐次审批是安全剧场。

## 理由

- 「平铺 vs 网关」是伪二选一：Hub 是链路上唯一知道对面客户端是谁的位置
  （Bearer Token 派生的 `ClientIdentity`，比 wire clientInfo 可靠——无状态
  协议下没有 initialize，`peer_info()` 为 None）。给有延迟加载的客户端让路
  （平铺），给没有的客户端把 Tool Search Tool 的体验复刻到协议层（网关）。
- 检索假阴性的根因不是「检索不够好」，而是用服务端关键词匹配替换了模型自身
  的语义理解。目录直给让「模型自己挑」这一步与平铺完全同构，可达率上限相同。
- Claude Code 实测（#66084，2.1.165 仍复现）：`list_changed` 只刷新 /mcp 能力
  列表，不刷新对话派发用的 deferred-tool 索引——升格对今天的 Claude Code 交互
  会话无效。因此升格不能是唯一通道，execute 兜底必须无条件保留。

## 后果

- 控制面新增 `SetConnectionToolExposure`；`ConnectionSummary` /
  `mcpane__connection_info` 回显配置模式与生效形态（自诊断）。
- 桌面端凭证弹窗per-connection 三态切换（自动/平铺/网关）。
- 守卫：browse 永不为空、describe 升格幂等且只通知本连接、升格名字失效惰性
  过滤、execute 自愈只在 execute 路径加料、gateway 目录截断上界、auto 分流、
  显式覆盖。真机验证项见 `docs/interop/tool-exposure-adr020.md`（未跑前
  如实标未验证）。
- 已知限制：对既不刷新工具列表也不读 instructions 的客户端，网关模式的可达率
  依赖它肯调用 tool_discovery——这是提示词工程层面的引导，无协议保证。
