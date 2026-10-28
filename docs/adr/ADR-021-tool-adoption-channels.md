# ADR-021：工具调用率——收窄网关、描述增强层与规则文件投递

- 状态：Accepted
- 日期：2026-08-31
- 关联：ADR-020（工具暴露三轴）、ADR-005（最小 patch 原子写）、
  ADR-002 Gate 补记（下游注解的用户声明口，本 ADR 的实验为其定了边界）、
  migrations/0024（`workspace_tools.description_suffix`）

## 背景

用户实测反馈：Cursor、Codex 接入 Hub 后仍存在「懒调用」——模型倾向凭记忆作答
或使用宿主内置工具。Claude Code 因 ADR-020 的 flat 分流已专门优化，其余客户端
没有对等待遇。

把一次调用拆成五道闸：①在场 ②想起 ③选中 ④敢用 ⑤复用。盘点 ADR-020 的覆盖：
① 由 flat/gateway 分流解决，② 由 instructions 教义解决一半，③④⑤ 基本空白。

**③ 是最大缺口，且已有实证**：`docs/interop/tool-exposure-adr020.md` 测试项 5
记录了模型在任务措辞泛化时选择 harness 自带 WebSearch。当时判定为「测试设计
噪音，不改代码」。本次重新评估该判定：下游工具描述写的是**能力**（what），而
模型选工具时匹配的是**触发条件**（when）；宿主内置工具的描述由 harness 精调，
下游 server 的没有——这场竞争默认不公平，而 Hub 正好站在能补位的位置上。

## 决策

### 1. auto 判定引入工具数维度（修订 ADR-020 决策 1）

ADR-020 的 auto 只看 `client_type`。但网关是 `discovery → describe → execute`
**三跳漏斗**，每一跳都是一次模型放弃的机会；ADR-020 自己也承认「可达率依赖
它肯调用 tool_discovery，无协议保证」。而选择网关的唯一收益是上下文成本，
成本 = O(工具数 × schema)——**工具少时，这笔交易不成立**。

`resolve_auto_exposure(client_type, workspace_discovery, visible_tools)`
（`binaries/mcpane-hub/src/hub.rs`，抽为自由函数以便直接单测）：

- Claude Code 无条件 flat（自带 ToolSearch，上不封顶）；
- 发现开关关闭 → flat（同改动前）；
- 发现开关开启但**本连接可见工具数** ≤ `AUTO_FLAT_TOOL_LIMIT`（80）→ flat；
- 超出、或数不出来（Revision 取不到）→ gateway。回退无条件，不拿未知数翻配置。

阈值取编译期常量而非配置项：想强制网关的用户走 per-connection 显式 `gateway`，
不为一个内部启发式再铺一条设置管道（YAGNI）。**真机校准后定为 80**：初值 40
撞上了本机生产工作区的实际规模（正好 40 个下游工具，firecrawl 一家占 27），
压线通过、再接一个 server 就失效，对重度用户形同虚设。80 给一倍余量；
超过 80 时上下文成本才真正压过三跳漏斗的代价。

`mcpane__connection_info` 增回显 `visibleToolCount` / `autoFlatToolLimit`——
只报 `effectiveExposure` 说不出「为什么是 flat」，用户无从自诊断。

### 2. 工具描述增强层（闸③的确定性通道）

flat 形态下 `description` 是**协议保证必然进模型上下文**的字段（不注入模型就
无法调用），而 instructions 是否注入取决于 host。此前 `catalog_tool_to_rmcp`
原样透传下游描述——Hub 占着聚合位却没用上唯一的强通道。

- `workspace_tools` 加列 `description_suffix`（0024，纯 ADD COLUMN 不重建表）；
- `ToolOverrides` 的 value 由元组 `(bool, Option<String>)` 升为具名
  `ItemOverride { enabled, alias, description_suffix }`——元组到第三维就没人
  记得住谁是谁；
- `build_revision` 内 `append_suffix` **追加不覆盖**：下游改了自己的描述，
  用户写的触发条件依然在；后缀为空则描述逐字节不变；下游无描述时后缀独立成立
  （没有描述的工具恰恰最需要一句「何时该用」）；
- **prompts 不套后缀**：prompt 由用户主动触发（slash command），描述影响的是
  人的挑选而非模型的自主选择；
- 描述不参与 `schema_hash`，加后缀不改变路由与 Revision 同一性（有守卫）；
- `set_item_override` 的 `alias` / `description_suffix` 为 `None` 表示
  **不改动**而非清空（清空走空串）。调用方各自只关心一个字段——服务器弹窗切
  开关、工作区页存提示——要求每个调用方都把其余字段原样带上，迟早会漏，
  漏一次就是用户配好的东西被静默抹掉。COALESCE 在数据层一次兜住；
- **网关目录的截断预算优先给触发提示**（两轮真机各抓到一个缺陷）：
  第一轮——按拼好的描述整体从尾部截，而提示恰在尾部，`关键词：搜索、查找、检索`
  被切成 `关键词：搜索…`，关键词正是触发匹配最靠的部分。改为 `CatalogTool` 另存
  一份 `description_suffix` 原文、`catalog_desc_line` 先保提示。
  第二轮——渲染时把**已拼好提示的** `description` 当成下游自述传进去，提示被拼了
  两遍（截断后残留 `Use…` 开头，后面又完整追加）。改为先经
  `CatalogTool::downstream_description()` 剥离：拼接由 `append_suffix` 做，剥离
  就该由同一处提供，分散到调用方必然各写各的、改了一边对不上另一边。

推荐内容形态是触发式 + 中文关键词（下游描述几乎全英文而用户任务全中文，
跨语言匹配不是零损耗）：
`Use when: 需要实时网页内容时（优先于内置搜索）。关键词：抓取、爬取、网页`

不内置硬编码规则——哪个工具该在什么时候用，只有用户自己知道。

### 3. usage_guidance 投递到宿主规则文件（闸③的强通道）

`workspaces.usage_guidance` 此前只进 instructions（建议性、host 可选、权重低于
系统提示词）。宿主自己的规则文件则进系统提示词层。同一份准则两路投递。

`crates/mcpane-config/src/guidance.rs`，复用既有基建（`atomic_write` +
`FileFormatting::detect/restore` 行尾与 BOM 保真），markdown 是纯文本不走
document 模型：

- 托管区块 `<!-- MCPANE:BEGIN … -->` … `<!-- MCPANE:END -->`，存在则整块替换
  （幂等），不存在则追加；撤销时连同留下的空行清理，**区块外一字不动**；
- 区块残缺（有 BEGIN 无 END）**一律报错而不是猜边界**——猜错就是吞掉用户内容，
  与 ConfigEngine「绝不静默覆盖」同一条原则；
- 空准则等于取消投递，不留空壳区块。

**默认关闭，逐客户端勾选**，投递前明示写哪个文件、写什么内容、怎么撤销。
写的是用户自己的文件，一键铺全部不是效率是越权。

落点。**两件事必须分开记**——「路径对不对」和「客户端会不会读」是两个问题，
早先版本用同一个 ✅ 表示，把「文件存在」当成了「验证过」：

| 客户端 | 落点 | 路径存在 | **实测读取** |
|---|---|---|---|
| Claude Code | `~/.claude/CLAUDE.md`（尊重 `CLAUDE_CONFIG_DIR`） | ✅ | ✅ 它会把该文件注入系统提示词，本仓开发过程中反复可见 |
| Codex | `~/.codex/AGENTS.md`（尊重 `CODEX_HOME`） | ✅ | ❌ 社区通行约定，未实测 |
| ZCode | `~/.zcode/AGENTS.md` | ✅ | ✅ 读取链在它自带的 agent 内核里逐段核对（见 interop 记录第 7 项） |
| Gemini CLI | `~/.gemini/GEMINI.md` | ❌ 本机无此文件 | ❌ 按文档推定 |

**实测读取那一列直接驱动 UI**（`GuidanceTarget.path_verified`）：为 ❌ 的两家
在投递清单里带一句「未实测该客户端是否真的读它」。判据卡死在读取侧——
本轮跑通的投递端到端属于**写入侧**（文件写对、幂等、可撤销），说明不了客户端
会不会理它。不标的话，用户勾完开关就以为准则生效了，而这类「看着成功」的失败
最难自查。

**GUI 编辑器类客户端够不着**：规则要么存在应用内部存储（Cursor 的 User Rules、
Trae、Qoder），要么按项目存放（VS Code Copilot、Cline）而 MCPane 不管理项目路径。
不做 `~/.cursor/projects` 之类的内部状态反推：读客户端私有状态既脆弱又越界。

Antigravity 是另一种够不着：它的规则只按目录层级发现（`GEMINI.md` / `AGENTS.md`，
内核自带的文档里就叫 Workspace Rules），**没有用户级的全局规则文件**可写，同样走手动。

这些客户端走**手动投递**——但「不支持」不是终点，只给一句原因等于把用户丢在
半路。每家给出：为什么得手动、可照做的步骤、一键复制的可粘贴文本
（带 markdown 小标题、**不带** MCPANE 托管标记——手动路径要么独占文件、要么
进设置文本框，边界标记在两种情况下都只是噪音）。

**步骤刻意不写死菜单层级**：各家 UI 随版本漂移，写死的路径过期得比谁都快，
而用户照着一条过期路径找不到东西比没有指引更糟。写「在设置里搜什么词」这类
抗漂移的表述，只对多方一致且格式稳定的文件路径给精确值
（`.cursor/rules/*.mdc`、`.clinerules/`、`.github/copilot-instructions.md`）。
Cline 的全局规则目录在官方文档里就有三种说法，因此只给项目级路径。

手动投递**不报「已完成」状态**：规则可能落在 MCPane 读不到的应用内部存储里，
自报一个会撒谎的状态不如不报。

对 Cursor 而言，决策 1 与 2 才是主力抓手——这也正是它们排在前面的原因。

### 3b. 两种形态都要自报家门

用户的准则是静态文本，形态却按客户端与工具数自适应（决策 1）——一份为网关写的
准则（「先 `tool_discovery` 检索再执行」）碰上平铺，就是在指挥模型去调一个根本
不存在的工具。此前 `compose_instructions` 只在网关时说明形态，平铺什么都不说，
等于放任那份过时准则误导模型；阈值上调到 80 后切平铺的连接更多，这个缺口会扩大。

平铺形态补一句说明，并显式指出「若上面的准则提到检索/两步调用，那是网关形态的
说法，此处不适用」。

**只在用户确实写了准则时才说**：这句话是给「准则与形态对不上」兜底的，没写准则
的用户不存在这个问题，对他们纯属噪音——而 instructions 每长一分，真正重要的
那几句就被稀释一分（这也是本 ADR 决策 4 拒绝 per-client 教义分支的同一条理由）。
无可见 Server 时同样不说，那时整段能力总览本就不展开。

### 4. 教义补「能力重叠」条款

instructions 的内置教义补一句：与宿主自带的同类工具能力重叠时优先用专用工具，
理由落在可验证的事实上（用户按需接入、配好凭证与数据源）而非空喊优先级。

**不做 per-client 教义分支**：实际差异点要么已通用处理（工具名前缀映射），
要么需要点名宿主内置工具——而那份清单随版本变化，点名既易过期又可能在内置
工具确实更合适时误导模型。

### 5. 观测（先决条件）

此前无法回答「模型该调没调」，所有优化都是盲改。`LogStats` 复用既有分组模式
增加 `by_tool`（按 `method = 'tools/call'` 过滤后取 `exposed_name`——只判该列
非空不够，`prompts/get` 与 `resources/read` 也写它，后者写的是 URI）与
`by_connection`——后者是「哪个客户端在懒调用」目前唯一的客观读数。

## 明确否决

- **平铺路径的 execute 自愈**：`hub.rs` 注释已写明既定理由（直连客户端手里
  本就有真实 schema），`aggregation.rs` 有守卫锁住失败响应形状。flat 下缺参是
  生成问题而非信息缺失，不值得为边际收益推翻双栈保真原则。
- **缩短工具名 / 去 server 前缀**：ADR-006 的唯一性是硬约束，宿主还会再套自己
  的前缀，收益小风险大。alias 是现成出口。
- **在工具返回结果里塞引导文本**：污染数据面，模型会当成内容读。

## 后果

- 既有用户的行为变化：开着发现开关且工具数 ≤ 80 的非 Claude 连接，
  从网关转为平铺。这是目标改进，但 tools/list 形状变化对活跃会话可见。
- 新增控制面命令 `guidance_targets` / `guidance_deliver` / `guidance_revoke`
  / `guidance_preview`。`guidance_targets` 按工作区解释状态：区块内容与当前
  工作区准则不一致时回报 `stale`——多工作区下「已投递」可能说的是另一个
  工作区的内容，不区分就是在误导。
- 守卫（全部经反向验证——改坏源码确认转红）：auto 判定四种组合、描述追加不
  覆盖且不动 schema_hash、空后缀逐字节不变、区块幂等/撤销还原/残缺报错/
  CRLF+BOM 往返、不支持的客户端不被静默隐藏、suffix 确实随请求下发。
- **真机验证：协议侧六项已过，模型行为侧六项未跑**，见
  `docs/interop/tool-adoption-adr021.md`。已验证 auto 判定两侧行为（4 个工具走
  flat、44 个工具切 gateway）、Claude Code 的无条件例外、`connection_info`
  自诊断、描述增强落到 `tools/list`、规则文件投递用真实 CLAUDE.md 副本跑通
  投递/幂等/更新/撤销且 diff 零差异。
  真机同时抓到并修复了网关目录截断吃掉触发提示的缺陷。
- **核心假设已验证，且在真实工具数下复现**：Codex 0.133 + 隔离 Hub。
  先在单工具环境验证（0/3 → 3/3），再把环境做成 **40 个真实形态的工具**
  （对齐本机生产配置：搜索/抓取/文档/数据库/文件等 39 个干扰工具 + 1 个目标
  工具），唯一变量仍是 `description_suffix`：**A 组 0/5，B 组 5/5**。
  A 组答的是社区共识（「底层库用 thiserror、顶层应用用 anyhow」），B 组推翻
  自己的先验、以工具结果为准（「都不用，本项目统一用 snafu」）。
- **Cursor 侧测不出增量**：两轮各 3/3，A 组已满分（换过一次场景：工具名与提问
  无字面匹配、模型有强先验，仍 3/3）。Cursor 在单工具干净环境下不懒，与 Codex
  形成鲜明对比；但那不是真实场景，它在 40 工具环境下的行为仍未测（无 CLI）。
- **阈值已由真机校准从 40 上调到 80**：本机生产工作区正好 40 个下游工具
  （firecrawl 一家占 27），在初值下压线通过、再接一个 server 就掉回 gateway。
  **80 是「覆盖已知重度配置 + 留一倍余量」拍的，不是测算结果**——上下文成本
  与客户端硬上限都未实测。真要再调，依据应当是「某客户端在 N 个工具时开始
  截断或报错」这类实测数字。
- 实验附带修掉了 ADR-020 决策 8 的无效配置键（Codex 不认 `default_tools_
  approval_mode`，代码里不再写入 + `PatchOp::remove` 清理存量），并暴露一个
  待处理缺口：**不申报 `ToolAnnotations` 的下游 server 在 headless Codex 下
  不可用**，而 Hub 按 ADR-002 不伪造注解，下游没有出口。
- **该缺口已由 ADR-002 的 Gate 补记解决**（`tool_declared_annotations` 表，
  迁移 0025）：用户可为单个下游工具声明一份注解，键不含 workspace。
  方案的边界由本轮的四变体探针实验定死——Codex 认的是 `readOnlyHint` 而非
  「annotations 字段存在」，只有 `title` 的空壳照样被取消。这否决了「Hub 自动
  补空壳」：管用的那句是安全断言（声明本工具不修改环境，客户端据此跳过审批），
  而下游会不会写数据 Hub 无从得知，替它断言等于拿用户数据赌一个我们没有的知识。
  **只能让用户断言**，且最小必要断言就是一个「只读」开关。
  实现见 `docs/interop/tool-annotations-declaration.md`。
