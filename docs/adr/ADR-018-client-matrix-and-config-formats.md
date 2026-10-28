# ADR-018：客户端矩阵扩到八家——官方项落库、additive 语义、JSON5/YAML 文档模型、配置目录覆盖

- 状态：Accepted
- 日期：2026-07-25
- 修订：ADR-013 §「Official 运行时合成、不落 store、不可删改」

## 背景

cc-switch 上游已支持八个客户端，我们停在四个（Claude Code / Claude Desktop /
Codex / Gemini CLI）。补齐的过程中撞上五件不是"多写一个 adapter"能解决的事，
各自都改变了既有约定，故合并成一份 ADR 记录。

起因是一个用户可见的 bug：**没装 Gemini CLI，供应商页却显示「Google 官方登录 ·
使用中」**。顺着查下去发现根子在 ADR-013 的官方项模型。

## 决策一：官方项从"运行时合成"改为"一次性播种进 store"

ADR-013 规定官方项运行时合成、不落 store、不可删改。实现下来有三处失真：

1. `list()` 无条件为每个 app 合成官方项，从不校验该客户端是否存在——没装的
   客户端照样多出一条"官方订阅"；
2. 前端把「`current` 无记录」当成「官方订阅正在生效」，而那其实只表示
   "MCPane 从没接管过这个 app"；
3. 用户删不掉自己用不到的官方项。

**改为**：首启只为**检测到已安装**的客户端播种一条官方 Profile 落库，之后与
中转站同权——可编辑、可删除、**删了不复活**。播种按 app 记账
（`ProviderState.seeded_officials`），后装的客户端下次启动补种。

这与 cc-switch 的 `providers_seed.rs` 同构（它用全局 `official_providers_seeded`
flag，我们按 app 记，多支持"先装 MCPane 后装客户端"）。它自己的注释写得很直白：
*"启动自动导入只产出 default，从不产出官方条目——否则删掉的官方条目每次重启
都会复活"*。

**连带约束**：首启收编（`import_live_on_startup`）的守卫必须改成"该 app 已有
**非官方** provider"，否则播种一次之后收编永远短路（对齐 cc-switch 的
`has_non_official_seed_provider`）。顺序也固定为**先收编、后播种**。

## 决策二：引入 additive 客户端类型

OpenCode / OpenClaw / Hermes 的配置里**多个供应商并存**，由客户端自己选用，
**没有"当前生效项"**，也**没有官方订阅**（它们自己不提供模型）。

领域层用两个谓词把差异显式化，而不是让 UI 到处 `match app`：

- `ClientApp::has_official()`：播种与「重新添加官方」据此挡掉；
- `ClientApp::is_additive()`：决定 UI 用「启用 / 已启用」还是「切换 / 使用中」。

配套的动作也不同：**「移除」= 从 live 配置删掉但保留列表记录**
（`remove_from_live`，对齐 cc-switch 的 `removeFromLiveConfig`），启用态来自
`live_provider_ids` 而非 `current`。删除记录时会先摘 live 条目，否则客户端配置
里会留下连不上的孤儿。

additive 客户端还要求**显式声明一组模型**（只认列出来的），故 `ProviderProfile`
新增 `models: Vec<String>`；老数据只有单值 `model` 时回退使用，避免升级后原有
条目突然启用不了。

## 决策三：新增 JSON5 与 YAML 文档模型，**YAML 不保注释**

配置格式**不能从文件名或同类语义推断**，必须看参考实现用什么解析器：

| 客户端 | 文件 | 实际格式 |
|---|---|---|
| OpenCode | `~/.config/opencode/opencode.json` | JSON（走 XDG，不在 `~/.opencode`） |
| OpenClaw | `~/.openclaw/openclaw.json` | **JSON5**（名为 .json，实为无引号键 + 单引号 + 注释） |
| Hermes | `~/.hermes/config.yaml` | **YAML**，且供应商容器是**数组** |

- **JSON5**：用 `json-five` 的 round-trip 解析器，地位等同 TOML 侧的
  `toml_edit`——改哪个键只动哪个键，注释与风格逐字保留。
- **YAML**：**这一路不保注释**。Rust 生态没有成熟的 YAML round-trip 编辑器，
  文档在内部走 `serde_yaml_ng` → JSON 值模型，序列化即整份重写。写入仍走引擎，
  **写前快照与历史回滚照旧**，格式被规整时用户能从历史里找回原文。这是对
  "最小改动、保住原貌"这一引擎承诺的**显式例外**，仅限 YAML。

  > 修订（2026-07-25 复核）：本条原写"参考实现（cc-switch）同样是 `serde_yaml`
  > 整份读写"——**这句是错的**。cc-switch `hermes_config.rs` 做的是**顶层段落的
  > 文本级替换**（`find_yaml_section_range` + `replace_yaml_section`）：只把被改
  > 的那一个 section 重新序列化，其余字节原样保留，因此用户在 `agent:`、
  > `mcp_servers:` 等段落里的注释与格式**不会丢**。也就是说这一路我们**弱于**
  > 参考实现，而非与之持平。整份重写仍是当前实现的事实，但它是我们的取舍代价，
  > 不能拿参考实现当依据。补齐路径见审计报告「Hermes YAML 段级写入」条。

两处刻意没有扩展地基：

- YAML 文档在内部直接用 JSON 值模型，路径读写与 JSON 共用一套实现；
- Hermes 的数组容器**不给引擎引入数组寻址**，而是把整个 `custom_providers`
  当一个值改写（adapter 内按 `name` 增/改/删）。代价只是历史 diff 的粒度是
  整块数组。

## 决策四：客户端配置目录可被用户覆盖

默认路径写死时，把配置放在别处的人（多账号隔离、便携盘、公司规范目录）**连"装没
装"都探测不出来**，更别说切换供应商。对齐 cc-switch 的 `*_override_dir`。

- 存 `ProviderState.client_config_dirs`（app → 绝对路径），**不放 SQLite 的
  `app_settings`**：`client_paths()` 是同步函数、启动路径上就要用，settings 仓储是 async。
- 套用方式是"换目录"：各家配置文件都住在同一个目录里，把 `file_name` 接到覆盖目录
  即可，adapter 的 join 逻辑不动。
- **探测必须跟着走**（`is_installed_with_dir` / `client_envs_with_dirs`）：不然用户设完
  目录反而看到「未检测到」，比不给这功能更糟。
- 校验从严：非绝对路径、目录不存在一律拒绝落库——静默生效意味着配置全写去了别处。
- **Claude Desktop 不支持**：四个文件跨 normal/threep 两目录，后端直接挡住。

## 决策五：可选维度只出现在配置里真有该字段的客户端上

additive 三家的字段并不齐平，据此加了三个能力谓词（`domain.rs`）与前端
`lib/clients.ts` 的对应标记：

| 维度 | 有的客户端 | 落到哪 |
|---|---|---|
| 上游协议 `upstream_api` | OpenCode / OpenClaw / Hermes | `npm` / `api` / `api_mode` |
| 自定义请求头 `headers` | OpenCode / OpenClaw | `options.headers` / `headers` |
| 上下文长度 `context_length` | Hermes / OpenClaw | `models.<id>.context_length` / `models[].contextWindow` |

**原则：给了却不生效比不给更糟。** 不支持的客户端不显示该控件，后端 `update_relay`
也不收下这个值——存了但落不到配置上，用户下次打开还看到自己的选择"在那儿"，只会
更困惑。

> **2026-07-26 就地修订（本表原有两行是错的）**：初版称"Hermes 没有上游协议维度"
> 与"上下文长度仅 Hermes 有"，并把这两条写成守卫测试锁死。逐行读 cc-switch 与两家
> 官方文档后证伪：Hermes 的 `custom_providers[].api_mode` 有四个取值
> （`chat_completions` / `anthropic_messages` / `codex_responses` /
> `bedrock_converse`），cc-switch 预设注释明写「Always written explicitly」，59 条
> 预设里 37 条 chat、22 条 anthropic；OpenClaw 的 `models[].contextWindow` 是官方
> `docs/gateway/config-tools.md` 记载的字段，cc-switch 预设逐条带具体数值。
> 后果是 Hermes 接不了 Anthropic 系端点、OpenClaw 吃错上下文窗口（与已修的 Grok
> `context_window` 砍半同类）。**教训：把"我们认为它没有"写成守卫测试，等于给错误
> 假设上锁——能力谓词的依据必须落到源码行号或官方文档，不能靠推断。**

`context_length` 刻意**不预填数值**：cc-switch 的预设里带的是逐条查出来的具体数字，
我们没有那份数据，硬编就是瞎猜——用户不填就不写这个键，让客户端用自己的默认。

## 影响

- 客户端矩阵：切换式 5 家（Claude Code / Claude Desktop / Codex / Gemini / Grok）
  + additive 3 家（OpenCode / OpenClaw / Hermes）。
- 前端元数据收敛到 `apps/desktop/src/lib/clients.ts` 单一事实源，新增客户端
  只改一处；`clients.test.ts` 守卫"图标/官方 id/additive 一致性"——`BrandName`
  与 `ClientApp` 是两个类型，缺图标 TypeScript 不会报错。
- `ProviderState` 新增 `seeded_officials`、`client_config_dirs`、各 app 的
  `*_common_snippet`；`ProviderProfile` 新增 `models` / `upstream_api` / `headers` /
  `context_length`（都只服务 additive，切换式客户端忽略）；
  历史字段 `official_overrides` 在播种时一次性迁移后不再写出。
