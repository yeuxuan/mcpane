# ADR-021 工具调用率：真机验证记录

- 状态：**协议侧六项 + 核心假设（Codex）+ 注解声明口端到端已验证；
  Cursor 侧与另五项未跑**（末次更新 2026-09-08）
- 环境：隔离 Hub 实例（`MCPANE_HOME=/tmp/mcpane-adr021-verify` + 端口 24999
  + devserver stdio 下游），不触生产实例；验证后已清理。
- 自动化覆盖见 `binaries/mcpane-hub/src/hub.rs`（auto 判定、目录截断预算）、
  `crates/mcpane-hub-catalog/src/lib.rs`（描述增强）、
  `crates/mcpane-config/src/guidance.rs`（区块读写）、
  `apps/desktop/src/pages/Workspaces.test.tsx`（投递与指引 UI）。
  本文件只记录自动化替代不了的部分。

## 已验证

| # | 项目 | 结果 |
|---|---|---|
| 1 | auto 判定：discovery 开 + 4 个工具 | ✅ codex 走 **flat**，下游工具直接可见——改动前此处会是 gateway，只有 meta 工具 |
| 2 | auto 判定：discovery 开 + 44 个工具（11 × devserver） | ✅ codex 切 **gateway**，`tools/list` 只剩 5 个 meta 工具 |
| 3 | Claude Code 的无条件例外 | ✅ 同样 44 个工具下仍 flat，46 个工具全平铺 |
| 4 | `connection_info` 自诊断 | ✅ 回显 `effectiveExposure=flat` / `visibleToolCount=4` / `autoFlatToolLimit=40`（该次验证时的阈值，现为 80），判定自洽可解释 |
| 5 | 描述增强落到 `tools/list` | ✅ 原描述原样打头、触发提示追加在后、未配置的工具描述逐字节未动 |
| 6 | 规则文件投递端到端 | ✅ 用**真实 CLAUDE.md 副本**（113 行 / 6629 字节）跑投递 → 幂等 → 更新 → 撤销；撤销后 `diff` 与原件零差异（测试断言之外的独立复核）。用户真实文件全程未被触碰，无 `.mcpane-tmp` 残留 |
| 7 | ZCode 读不读 `~/.zcode/AGENTS.md`（原 E 项，2026-09-01） | ✅ **会读**，读取链见下节 |
| 8 | Antigravity 有没有可写的**全局**规则文件（2026-09-08） | ✅ **没有**——内核自带的规则文档标题就是 "Workspace Rules"，位置只列目录级 `GEMINI.md` / `AGENTS.md`（不支持 frontmatter、对所在目录及子目录恒生效），全局那一层只有 `~/.gemini/config/` 下的 skills / plugins / mcp_config，没有规则文件。故 Antigravity 归**手动投递**，理由写进 `guidance.rs` 的 MANUAL 表 |

## 真机抓到并已修复的缺陷

**网关目录把触发提示的尾部截掉了。** `compose_instructions` 的目录条目按
`CATALOG_TOOL_DESC_MAX_CHARS`（100 字符）从**拼好的描述整体尾部**截断，而触发
提示恰好拼在尾部——实测 `关键词：搜索、查找、检索、仓库、代码、定位` 被切成
`关键词：搜索…`。关键词正是触发匹配最靠的那部分，等于描述增强在网关形态下被
悄悄削掉大半。自动化测试只测了「总长受上界钉死」，钉住了，却钉错了地方。

修复：`CatalogTool` 另存一份 `description_suffix` 原文（拼完就没法还原边界——
下游描述本身可能带换行），目录渲染改为 `catalog_desc_line`，**预算优先给触发
提示，不够了先截下游自述**；提示自身超预算时自述整个让位。目录的用途是「一行
认得出、知道什么时候用」，用户写的触发条件比下游自述更贴这个用途。

复验：同一条目变为 `开发用检索工具（返回带 … Use when: …关键词：搜索、查找、
检索、仓库、代码、定位`——关键词完整，被牺牲的是下游自述。守卫
`catalog_line_spends_its_budget_on_the_trigger_hint_first` 已反向验证（退回
旧的整体截断行为即转红）。

## 第二次真机复核抓到的缺陷：网关目录把触发提示拼了两遍

上一次修「截断预算优先给触发提示」时，`catalog_desc_line` 的第一个入参传的是
`tool.description`——那是**已经拼好提示的完整描述**，函数再把 `description_suffix`
完整追加一遍。实测目录行长这样：

```
- conv__team_handbook：Returns handbook entries. Use… Use when: 任何涉及…
```

下游自述被截成 `Returns handbook entries. Use…`（残留提示开头），后面又跟一份
完整提示。预算被重复内容吃掉，下游自述反而被无谓截短。

修复：拼接由 `append_suffix` 做，剥离就该由同一处提供——`CatalogTool::
downstream_description()` 按拼接规则摘掉提示，剥不掉时退回完整描述（不比不剥更差）。
守卫 `downstream_description_strips_the_appended_hint` 已反向验证。

复核过其余三个出口（`tool_discovery` 的 matches、`tool_describe`、flat 的
`tools/list`）：它们要的就是拼接后的完整描述，没有截断预算问题，**无同类实例**。

复验后的目录行：`Returns handbook entries. Use when: …关键词：…、约定`——
下游自述完整、提示只出现一次、关键词尾部完整。

## 阈值判定的六种组合（真机）

> **这批数字是在 `AUTO_FLAT_TOOL_LIMIT = 40` 时跑的，原样保留。**
> 阈值后来据此上调到 80（见下），但**判定逻辑是同一套**——边界行为
> （「≤ 上限走 flat，超一个就 gateway」）与上限取值无关，这批记录仍然有效。
> 改这里的数字就是篡改实验记录，那比记录过时更糟。

`AUTO_FLAT_TOOL_LIMIT = 40`，连接为 `auto` + `client_type=cursor`：

| 下游工具数 | workspace discovery | effectiveExposure | tools/list |
|---|---|---|---|
| 40 | 关 | flat | 42 |
| **40** | **开** | **flat** | 42 | ← 本次改动的目标行为（改动前此处为 gateway）|
| **41** | 开 | **gateway** | 5 | ← 边界精确 |
| 42 | 开 | gateway | 5 |
| 80 | 开 | gateway | 5 |
| 80 | 关 | flat | 82 |

**阈值对真实配置没有余量**：本机生产工作区的下游工具数正好是 40
（context7 2 + deepwiki 3 + exa 2 + fast-context 2 + firecrawl 27 +
microsoft-learn 3 + sequential-thinking 1），压线通过——**再接任何一个 server
就掉回 gateway**。firecrawl 一家就占 27 个。

→ **已据此上调到 80**（代码现值）。但要说清楚依据的强度：80 是「覆盖已知重度
配置 + 留一倍余量」拍的，**不是测算结果**——上下文成本与客户端硬上限都没实测。
真要再调，依据应当是「某客户端在 N 个工具时开始截断或报错」这类实测数字。

## 核心假设已验证：触发提示把调用率从 0% 拉到 100%

Codex CLI 0.133.0（gpt-5.5）+ 隔离 Hub + 一个真实能干活、但自述平庸
（`Returns project metadata.`）的实验 server。提示词固定为「这个项目的 Rust
代码注释应该怎么写？简短回答即可」——泛化措辞，模型完全可以凭通用知识作答。
读数取自下游 server 自己记的调用日志，不解析模型的自然语言输出。

| 组 | ToolAnnotations | 触发提示 | 结果 |
|---|---|---|---|
| A | 无 | 无 | 0/5，**连尝试都没有** |
| B | 无 | 有 | 0/5 完成，但 **5/5 主动发起**（被 Codex 审批拦下） |
| A' | 有 | 无 | **0/3** |
| B' | 有 | 有 | **3/3** |

A' 与 B' 的唯一变量是 `description_suffix`。0% → 100%。

### 在真实工具数下复现（40 个工具）

单工具环境的说服力有限——模型没有「选择成本」。把干扰工具做成真实 MCP server
集合的样子（web_search / web_scrape / query_docs / code_search / sql_query /
parse_pdf … 共 39 个）+ 1 个目标工具 `team_handbook`，凑成 **40 个下游工具**，
正好对齐本机生产配置的规模。提问换成模型有强先验的技术选型题：
「这个项目的错误处理应该用 anyhow 还是 thiserror？」

| 组 | 触发提示 | 结果 |
|---|---|---|
| A | 无 | **0/5** |
| B | 有 | **5/5** |

A 组答的是社区共识：「底层库用 thiserror，顶层应用用 anyhow」——凭记忆，没查。
B 组推翻自己的先验、以工具结果为准：「都不用。按团队手册，本项目错误处理统一用
snafu……禁止引入 anyhow」。

工具名 `team_handbook` 与「错误处理」没有任何字面关联，模型得自己想到「团队手册
里可能规定了这个」——触发提示补的正是这一步。

B' 组模型的回答直接引用了埋在工具返回值里、通用知识猜不到的内容（「单条注释
不超过 3 行」「unsafe 块上方必须有 SAFETY:」「决策必须带 ADR 编号」），
证明它不只是发起了调用，而是真的把结果用上了。

样本量小（用户授权的是小规模试水），但 A/A' 组 8 次零调用、B' 组 3 次全调用，
方向没有歧义。要写进结论性材料的话建议再扩到每组 15 次。

## 附带发现：一键接入写了一个 Codex 不认的配置键（已修）

实验中 B 组的调用全部报 `user cancelled MCP tool call`，追下去发现：
`crates/mcpane-adapter-codex/src/patch.rs` 按 ADR-020 决策 8 写入的
`default_tools_approval_mode = "approve"`，在 Codex 0.133 上
**`codex --strict-config` 直接报 `unknown configuration field`**——
非严格模式下静默忽略，严格模式下会让用户的 Codex 起不来。

`approval_policy = "never"` 也无效（实测仍取消）：它管的是 shell 命令审批，
不管 MCP 工具。真正的根因 `docs/interop/codex-direct-http.md` 早有记载——
缺 `ToolAnnotations` 的工具被 Codex 误路由进 app-tool 审批路径，headless 下
stdin EOF 被当成「用户拒绝」。实验 server 补上 annotations 后调用立即通畅
（A'/B' 两组均无取消）。

处置分两步：

1. **不再写入**——删掉该 op，断言改为「必须不存在」；
2. **清理存量**——光不写没用，旧版接入过的用户配置里那个键还躺着，他们不会
   自己去发现它。`hub_entry_patch` 里加一条 `PatchOp::remove`，接入/重连/轮换
   凭据时顺手删掉自己留下的垃圾。`json5::remove` 对不存在的键是 no-op，
   新用户无副作用；范围严格限定在 `mcp_servers.mcpane` 段内，不碰别家 server
   的同名键（守卫 `legacy_approval_key_is_removed_on_reconnect` 两个方向都
   反向验证过：去掉清理转红、扩大范围也转红）。

**残留局限**：清理挂在接入路径上。已接入且从此再不重新接入、不轮换凭据的用户，
配置里的键仍在。危害仅在 `--strict-config` 下才致命（默认静默忽略），
为此另铺一条启动期扫描通道不划算。

### Codex 到底认哪个字段（探针实验，2026-08-31）

「给缺注解的下游自动补一个空壳注解」是否可行，取决于 Codex 看的是「有没有
annotations 字段」还是「有没有具体的行为断言」。四个变体，同一工具、同一提问：

| 变体 | annotations 内容 | 结果 |
|---|---|---|
| none | 不申报 | 发起了但被取消 ⛔ |
| title | `{"title": "..."}` | **发起了但被取消** ⛔ |
| readonly | `{"readOnlyHint": true}` | **调用成功** ✅ |
| full | title + 四项 hint 齐全 | 调用成功 ✅ |

**结论：Codex 认的是 `readOnlyHint`，不是「字段存在」。**

这一条直接否决了「Hub 自动补空壳注解」的方案：`title` 是纯显示名、补它不构成
替下游声明语义，但**它不管用**；管用的 `readOnlyHint: true` 是一句安全断言——
声明「本工具不修改环境」，客户端据此跳过审批。下游那个工具到底会不会写数据，
Hub 无从得知，替它断言等于拿用户的数据赌一个我们没有的知识。

**因此方案只能是让用户断言**：他选的下游、他知道那工具干什么，他有这个知识而
Hub 没有。好消息是 UI 可以极简——**最小必要断言就是一个「只读」开关**，
不必让用户填满四项 hint。

这同时落地了 ADR-002 GATE-1 留下的待办（「下游聚合透传时需标注风险」）：
先标出哪些工具没申报注解、说清后果，再给一个开关。

**由此暴露的产品缺口**：Hub 对下游 annotations 是「缺失时不伪造」
（ADR-002 Gate，原则正确），但后果是**任何不申报 annotations 的下游 server，
在 headless Codex 下都不可用**。内建工具已补全注解治本，下游的没有出口。

→ **已解决并真机验证**（2026-09-01）：ADR-002 Gate 补记开了「用户声明」这个
口子（`tool_declared_annotations`，迁移 0025）。同一条 `codex exec`
在声明前被取消（下游 0 次调用、模型退回凭记忆作答），声明只读后调用通畅
（下游 1 次调用、零取消、模型改用工具返回的答案）；对照组「下游已申报注解的
工具，用户声明不生效」同轮验证通过。详见
`docs/interop/tool-annotations-declaration.md`。上面那组四变体实验正是它的
边界依据——自动补空壳不管用，只能让用户断言。

## 仍未验证

每项都写清**卡在哪、需要什么、判据是什么**——只写「未跑」下次还得从头想一遍。

| # | 项目 | 卡在哪 | 判据 |
|---|---|---|---|
| A | 描述增强在 **Cursor** 上的效果 | Cursor 无 CLI，对照实验的每一轮都得人工发消息 | 下游 server 自记的调用日志，A/B 各 ≥5 次 |
| B | 阈值 **80** 是否合适 | 需要真实 Cursor 报出它的工具数上限与超限表现 | Cursor 的 MCP 面板在 82 个工具时显示几个：全在 / 截断 / 报错 |
| C | 手动指引照着做能不能走通 | 只能真人照着做一遍 | 六家各走一遍，卡在哪一步就改哪一步的措辞 |
| D | Gemini CLI 读不读 `~/.gemini/GEMINI.md` | **本机未安装 Gemini CLI**（`~/.gemini/` 下是 antigravity 的目录，`settings.json` 结构确认属 Gemini CLI 约定，但 CLI 本身不在 PATH） | 装上后投递 → 新会话问一个只有准则里才有的答案 |
| F | 教义「能力重叠」条款的实效 | 提示词工程的效果是边际的，小样本测不出；且要造「Hub 工具与宿主内置工具都能做」的场景 | 每组 ≥15 次才有统计意义，成本高于当前收益 |
| G | ZCode 到底认不认 `~/.agents/mcp.json`（`mcp_clients.rs` 里 ZCode 注释第 1 点的半个理由） | 核对 E 项时顺手在 CLI 内核里搜过：配置落点是硬枚举（用户级 `~/.zcode/cli/config.json`，工作区级 `zcode.json` / `.zcode/config.json`），`.agents` 只用于 skills、custom commands 和 `/init` 的 AGENTS.md 候选，**搜不到这个兼容文件**。但只搜了 CLI 内核，够不着 GUI 主进程，也可能是已下线的旧机制 | 在 Electron 主进程那侧再搜一遍；结论不影响现有行为（我们写的嵌套 `mcp.servers` 已真机验证生效），只是那半句理由可能过期 |

E 项已结，见下方小节；字母不重排，免得别处引用错位。

**D 的现状已反映在 UI 上**：它（连同 Codex）的 `path_verified = false`，
投递清单里带一句「未实测该客户端是否真的读它」。没验就是没验，标出来比假装验过强。

## E 项已结（ZCode 读 `~/.zcode/AGENTS.md`）

原判据是「在 GUI 里手动开新会话」，卡在 ZCode 没有非交互入口。但**读取侧不必靠黑盒**：
ZCode.app 里就带着它的 agent 内核（`Contents/Resources/glm/zcode.cjs`，GUI 的会话走的
就是这份），链路可以逐段核对——比问一句、看它答什么更硬，那种做法答对了也可能是模型瞎猜。

链路（函数名取自内核里保留的 `a(fn, "name")` 映射）：

1. `resolveContextSources` 里 `userInstructions` 一路默认到 `{workingDirectory}`——是真值对象，
   不是 `undefined`，所以那个三元恒走加载分支，**没有开关能默认关掉它**；
2. `resolveUserInstructions` 的 `priorityFiles` 默认 `["AGENTS.md"]`，`maxBytes` 默认 `100 * 1024`；
3. `findDefaultUserInstructionFile` 的用户级候选恒为 `join(HOME || USERPROFILE || homedir(), ".zcode", "AGENTS.md")`
   ——**没有 ZCode 专属的 home 覆盖变量**（不同于 `CLAUDE_CONFIG_DIR` / `CODEX_HOME`），只认通用的两个；
4. 用户级与工作区级两份**合并**（`scope: "user"` 在前，`\n\n` 拼接），不是互相覆盖——
   写用户级不会被项目那份顶掉。工作区级是 `findInstructionFile` 从 cwd 逐级向上找
   **项目根的 `AGENTS.md`**（不是 `.zcode/AGENTS.md`——那个是 `/init` 命令的落点，
   两套机制别混）。

**顺带记下的边界**：超过 100 KB 会**截断而不是报错**（`truncated: true`）。MCPane 的
BEGIN/END 区块若被截在中间，用户拿到的是半截准则且无任何提示。当前投递量（6 KB 级）离
这条线很远，故不加限制，只把数记在这里——真要逼近了，得在投递前按剩余预算卡一刀。

## A 项的现成环境（Cursor 对照实验）

Codex 侧那套可以直接搬，只有「发消息」这一步换成人工。完整脚本见下节，要点：

1. 下游 server 提供**一个真实能干活、但自述平庸**的工具，答案要是通用知识猜不到的
   （本轮用的是「团队手册规定错误处理统一用 snafu」——与模型的先验相反，
   一眼看得出它查没查）；
2. 干扰工具凑到与生产规模相当（本轮 39 个，做成真实 MCP server 集合的样子），
   否则模型没有「选择成本」，A 组会直接满分而测不出增量——**这是本轮在 Cursor 上
   栽过的坑：单工具环境两轮各 3/3，看着漂亮，其实什么也没说明**；
3. 提问用模型有强先验的题目，工具名与提问**不要有字面关联**；
4. 读数取下游 server 自记的调用日志，不解析模型的自然语言输出；
5. 每次必须新开对话——同一对话里连发，第二次会被第一次的上下文带着走。

## 复现方法

先跑一次 Hub 建库，再往 `mcp_servers` 插一行指向 `mcpane-devserver` 的 stdio 定义：

```bash
ISO=/tmp/mcpane-verify && mkdir -p $ISO
MCPANE_HOME=$ISO MCPANE_HUB_BIND=127.0.0.1:24999 MCPANE_CODEX_TOKEN=<tok> ./target/debug/mcpane-hub
```

`tools/list` 需先 `initialize` 拿 `mcp-session-id`，响应是 SSE（`data:` 前缀）。
超阈值场景靠注册多个 devserver 实例凑数（每个 4 个工具）。改
`workspaces.tool_discovery` / `client_connections.tool_exposure_mode` 后需重启
Hub 才生效（或走控制面 reload）。

### 给非 claude/codex 客户端发 Token

Hub 只认两个 dev token 环境变量（`MCPANE_CLAUDE_TOKEN` / `MCPANE_CODEX_TOKEN`，
见 `main.rs` 的 `bootstrap_connections`）。要以别的 `client_type` 连接
——比如验 Cursor 的 auto 判定——直接往库里插一行，`token_hash` 是明文的
SHA-256 十六进制：

```bash
TOK=$(python3 -c "import secrets;print(secrets.token_hex(32))")
HASH=$(python3 -c "import hashlib,sys;print(hashlib.sha256(sys.argv[1].encode()).hexdigest())" "$TOK")
sqlite3 $ISO/mcpane.db "INSERT INTO client_connections
  (id,name,client_type,token_hash,token_prefix,default_workspace_id,enabled,created_at,tool_exposure_mode)
  VALUES ('conn_v','Cursor (verify)','cursor','$HASH','${TOK:0:8}','ws_default',1,strftime('%s','now'),'auto');"
```

### 隔离客户端自己的配置

别改用户真实配置。Codex 用 `CODEX_HOME` 指向临时目录，凭据用**符号链接**
指回原位（`ln -s ~/.codex/auth.json $TMP/auth.json`）——复制凭据到 /tmp 是
没必要的暴露。Cursor 没有等价开关，只能往 `~/.cursor/mcp.json` 加一个条目、
测完删掉，并在测试期间临时禁用其他 MCP server（否则它们的 instructions
与工具会污染对照）。

### 让实验读数可信

下游 server 自己往文件里记调用（`tools/call` 时追加一行），**不要解析模型的
自然语言输出**。模型说「我查了手册」不等于它真调了工具；反过来它调了但没提
也是常事。工具返回的内容要埋一条通用知识猜不到、且与模型先验相反的事实，
这样人工复核时一眼能看出它到底有没有用上工具的结果。
