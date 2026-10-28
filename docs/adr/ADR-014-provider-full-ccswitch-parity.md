# ADR-014：Provider 对齐 cc-switch 完整能力（Codex 预设 / 配置片段 / 统一供应商）

- 状态：Accepted（§1 于 2026-09-20 修订；§1「不建代理」与 §7 于 2026-09-23 被 ADR-022 推翻）
- 日期：2026-07-12
- 关联：ADR-013（本 ADR 撤销其"Codex 侧不提供厂商预设"的取舍）

> **修订（2026-09-20）**：§1 的前提「Codex 原生支持 `wire_api = "chat"`」已失效——
> Codex 2026-02 起整体移除该协议（openai/codex#7782，`WireApi` 反序列化对 "chat"
> 直接报错，整份 config.toml 拒绝加载）。现行规则：Codex 预设**只收原生支持
> `/responses` 的端点**（官方文档或「/responses 401 而伪路由 404」探测核验），无
> Responses 端点的厂商不设 Codex 预设；`codex_wire_api: chat` 仅为读懂旧配置保留，
> 切换时拒绝落盘。不建代理的取舍不变（核验完成于 2026-09-20 的上游复查批次）。
>
> **再修订（2026-09-23）**：ADR-022 引入本地路由，「不建代理」与 §7「不移植自动故障转移」
> 不再成立：Chat 端点厂商可在开启路由后接 Codex（由路由转换协议），故障转移随路由提供。

## 背景

ADR-013 落地后与 cc-switch 逐项对照，仍有四块能力缺失：

1. **Codex 侧零厂商预设**，与 Claude 侧选择不对等。当时的理由是
   "cc-switch 依赖本地代理做 openai_chat→responses 转换"——进一步核验后
   发现该前提不完整：Codex **原生支持 `wire_api = "chat"`**，且相当多
   厂商（MiniMax / 百炼 / LongCat / 豆包 / MiMo）已原生支持 Responses API；
2. **无统一供应商**（cc-switch Universal Provider）：NewAPI 型多协议网关
   需要一份 base_url + Key 同时应用到 Claude 与 Codex；
3. **无「配置 JSON」**：cc-switch 的 Claude 供应商可携带任意 settings.json
   片段（含 5 个快捷开关），官方供应商同样可编辑；
4. **无「通用配置」**：跨供应商共享的 settings 片段。

## 决策

### 1. Codex 厂商预设：原生 wire_api 二选一，不建代理

`ProviderProfile` 新增 `codex_wire_api: responses | chat`（默认 responses），
切换时写入 `[model_providers.<slug>].wire_api`。预设据厂商端点格式标注：

- Responses 原生/聚合站 → `responses`；
- Chat Completions 厂商（DeepSeek / GLM / Kimi / 千帆 / 火山 / StepFun /
  ModelScope / SiliconFlow / Novita / Nvidia…）→ Codex 原生 `chat`。

与 cc-switch 的差异：它恒写 responses 并靠本地代理转换（能注入
reasoning 参数、modelCatalog 工具剖面）；MCPane 走原生 chat wire，
功能完整、推理链展示受限。不移植其代理与 model-catalog 投影。
Codex 预设随片段机制标配 `model_reasoning_effort = "high"`
（cc-switch 模板恒写该键）。

### 2. 预设收录原则：厂商官方 + 中立聚合，拒收推广中转站

cc-switch 预设含大量 `isPartner` / `partnerPromotionKey` 商业推广
relay 站（含返利注册链接）。MCPane 只收录：

- 模型厂商官方端点（DeepSeek / GLM / Kimi / MiniMax / 百炼 / 千帆 /
  火山 / BytePlus / 豆包 / StepFun / LongCat / 百灵 / MiMo）；
- 中立知名聚合平台（ModelScope / SiliconFlow / OpenRouter / Novita /
  Nvidia / AiHubMix）。

全部 URL 去除 aff/ref/utm/邀请码等跟踪参数（有测试守护）。
Claude / Codex 两侧各 25 项，厂商集合对等。

### 3. 附加配置片段（extra_settings）——适配而非照搬，两端通用

cc-switch 的 settingsConfig 是**整份 settings.json 全量覆盖**（切走前
回填），Codex 侧则是整份 config.toml 文本。MCPane 坚持最小 Patch 安全
引擎，等价改造为（Claude 与 Codex 共用同一套片段机制）：

- `extra_settings`：Claude 为任意 settings.json JSON 子集
  （`env.API_TIMEOUT_MS`、`attribution`、`permissions`…），Codex 为
  config.toml 任意子树的 JSON 表达（`model_reasoning_effort` 等）。
  切换时按叶子深合并写入；切走时
  **按"值仍与写入时一致"深移除**（等价 deepRemove：用户手改的值不动），
  片段对象与 live 完全一致时整节点移除（`env` 节点除外，受管键共存）；
- 受管键禁止进片段：Claude 为 7 个受管 env 键，Codex 为
  `model_provider` / `model_providers` / `disable_response_storage` /
  `model` 顶层键（认证/模型/路由必须走结构化字段，否则清理失效）；
- backfill：只回填片段中已有叶子的 live 值，live 删除即同步删除；
- **编辑器体验对齐 cc-switch**：配置片段编辑器打开时展示整份 live 配置
  （剔除受管键，`provider_live_settings` 命令提供基线），快捷开关如实
  反映当前生效状态；保存时前端做 diff，只把相对基线的**新增/修改**存为
  受管片段（相对 live 的删除不生效——本机制只做叠加覆盖，绝不把用户
  自有配置纳入管理再误删，这是与 cc-switch 全文件覆盖模型的刻意差异）；
- **清理记账（applied_fragments）**：每次切换把实际写入的有效片段存入
  state，清理一律以它为准——两次切换之间编辑过片段也能准确清残留，
  同一供应商"重新启用"同样先清后写；
- 5 个快捷开关逐一对齐 cc-switch：隐藏 AI 署名（`attribution`）、
  Teammates（`env.CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`）、
  Tool Search（`env.ENABLE_TOOL_SEARCH=true`）、最大强度思考
  （`env.CLAUDE_CODE_EFFORT_LEVEL=max`）、禁用自动升级
  （`env.DISABLE_AUTOUPDATER=1`）。

### 4. 官方订阅可编辑（两端）

官方 Profile 仍运行时合成，但 `ProviderState.official_overrides` 存
按 app 的覆盖项（extra_settings + common_config_enabled），合成时套用；
切到/切走官方时片段同样深合并/深移除。UI 上两端官方卡片均开放编辑，
仅暴露配置片段区。

### 5. 通用配置（Common Config，按 app 各一份）

`ProviderState.claude_common_snippet` / `codex_common_snippet` 存全局
片段；Profile 级 `common_config_enabled` 决定是否叠加（同名键以通用
配置为准，等价 cc-switch deepMerge(snippet over config)）。切换时合并
进有效片段，切走时随片段一起清理，不固化进单个 Profile。

### 5a. Codex query_params（Azure OpenAI）

`ProviderProfile.codex_query_params` 结构化存 URL query 参数，写入
`[model_providers.<slug>.query_params]`（空表清除残留）。Azure OpenAI
Codex 预设随之收录（`api-version = 2025-04-01-preview`，base_url 带
YOUR_RESOURCE_NAME 占位符，与 cc-switch 相同做法）。

### 5b. 端点候选与多端点测速

`ProviderProfile.endpoint_candidates` 存候选地址（预设可携带，如
AiHubMix 双域名；用户可自行添加），`test_endpoints` 并发测 HTTP 首
响应延迟（8s 超时，任何状态码都算可达），编辑弹窗内点击候选一键设为
Base URL。对齐 cc-switch 的 endpointCandidates + test_api_endpoints。

### 5c. Coding Plan 用量/配额查询

对齐 cc-switch coding_plan 的内置厂商查询（其 token_plan 分支与自定义
JS 脚本引擎 rquickjs 本就互斥，只移植内置查询、不引入 JS 引擎）：

- 已移植：Kimi For Coding / 智谱 GLM（国内+国际站）/ MiniMax（国内+国际），
  按 base_url 子串自动识别，Key 直接从系统 Keychain 物化（优于 cc-switch
  的 settings_config 明文读取）；解析产出 5 小时窗 / 周窗已用百分比 +
  重置时间 + 套餐等级，卡片上手动触发展示（≥90% 红 / ≥70% 橙 / 绿）；
- 暂未移植（需要额外凭据形态）：火山方舟（AK/SK + 火山签名 V4 变体）、
  智谱团队版（组织/项目 ID 请求头）、ZenMux（不在预设集）。

### 6. 统一供应商（Universal Provider）——影子条目模式

- `UnifiedProvider`：id/name/base_url/apps{claude,codex}/Claude 模型
  角色映射/codex_model/api_key_ref/notes/website_url/sort_order；
- **保存即同步**（cc-switch 是 upsert 与 sync 两步、深合并保留影子的
  自定义 JSON；MCPane 的影子是结构化派生数据，无需 merge，合并为一步）；
- 影子 id `unified-<app>-<uid>`，是普通 relay Profile：切换/托盘/连通性
  测试全走既有机制；Key 以副本形式下发到每个影子自己的 Keychain 条目
  （普通删除级联语义不被破坏）；
- Codex base_url 三态 /v1 补全（对齐 cc-switch Rust 端实现）：已含
  `/v1` 原样；纯 origin 补 `/v1`；带路径前缀不强补。Codex 影子恒
  `wire_api = responses`（NewAPI 型网关反代 OpenAI）；
- 保护：影子使用中时禁止关闭对应 app / 删除统一供应商；影子不可直接
  删除（生命周期归统一记录，UI 的编辑入口路由回统一弹窗）。

### 7. 不移植：自动故障转移（架构依赖，非取舍）

cc-switch 的 failover 是**本地代理转发循环内的跨上游重试**（按队列逐家
尝试 + 熔断器由真实转发流量驱动 + 自动切换硬性要求代理接管，未接管时
它自己的 UI 都禁用此开关）。失败检测、转移动作、隐式回切全部只存在于
代理请求路径上——MCPane 无代理架构下不存在等价移植路径，除非引入整个
代理子系统（已在第 1 节决策不建；后由 ADR-022 以自带本地路由实现）。
已有的替代能力：手动连通性测试 + 多端点测速一键换址。

## 影响

- `crates/mcpane-provider`：domain 新增 `CodexWireApi` / `extra_settings` /
  `common_config_enabled` / `OfficialOverride` / `UnifiedProvider`（state
  serde 全部带 default，旧 providers.json 直接兼容，schema 仍为 2）；
  service 新增片段深合并/按值深移除/回填、通用配置、官方覆盖、统一供应商
  CRUD+影子同步；
- 桌面端：ipc 新增 11 个命令；供应商页新增「统一供应商」标签页；编辑弹窗
  新增配置片段区（快捷开关/通用配置，两端；JSON 编辑器为 CodeMirror 6 渲染：高亮/行号/实时 lint/格式化，对齐 cc-switch JsonEditor）、Codex Wire API / Query 参数
  与端点测速区；卡片新增用量按钮（Coding Plan 厂商）；影子条目带「统一」
  徽章；品牌图标补 doubao/xiaomimimo/novita/nvidia/aihubmix/azure，
  Profile 支持显式 icon 字段（选预设时携带落库，对齐 cc-switch meta.icon，无独立选择 UI）；
- 测试：mcpane-provider 25 个集成测试 + 13 个单元测试（quota 解析/检测）、
  前端 27 个（预设健全性 + 基线/差集语义 + 图标推断/注册表守卫）。
