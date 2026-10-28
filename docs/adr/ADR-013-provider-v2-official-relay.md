# ADR-013：Provider v2——官方订阅与第三方中转站二分模型

- 状态：Accepted
- 日期：2026-07-12

## 背景

v1 的 Provider 按厂商枚举建模（`ProviderKind`: Anthropic / OpenAI / Azure /
Bedrock / Vertex…，7 个内置模板），一个 Profile 同时映射 Claude + Codex 双端，
只能从托盘"双端同切"。实际使用中暴露三个结构性缺陷：

1. 缺少"官方订阅账号 vs 第三方中转站"这一核心维度——Anthropic 模板实为
   API-Key 直连，切换会顶掉用户的 Claude Pro/Max OAuth 订阅登录；
2. 没有"切回官方"的路径，Codex 官方 `auth.json` 的 OAuth token 被中转站
   Key 覆盖后即丢失；
3. 双端绑定不符合真实场景：用户通常 Claude 用官方订阅、Codex 用中转站。

参考 cc-switch（已验证的同类开源实现，源码逐行核对）重构。

## 决策

1. **Provider 归属单一客户端 app**（`ClientApp`: claude | codex），每个 app
   独立切换，`ProviderState.current` 按 app 记录当前生效 id。
2. **类别只有两种**（`ProviderCategory`）：
   - `Official`：官方订阅（OAuth 登录）。~~内置固定项，运行时合成，不落 store，
     不可删改~~ → **已由 ADR-018 修订**：改为首启只为已安装客户端播种一条落库的
     Profile，之后与中转站同权（可编辑、可删除、删了不复活）；
   - `Relay`：第三方中转站（base_url + API Key），用户增删改，
     API Key 明文只进系统 Keychain（`SecretReference` 引用落盘）。
3. **Claude 切换语义**：中转站写 7 个受管 env 键（`ANTHROPIC_BASE_URL`、
   `ANTHROPIC_AUTH_TOKEN` 或 `ANTHROPIC_API_KEY`（按 `api_key_field` 二选一，
   另一形态清除）、`ANTHROPIC_MODEL`、`ANTHROPIC_DEFAULT_HAIKU/SONNET/OPUS_MODEL`；
   无值的键 Remove 清残留）；切回官方即整组移除，OAuth 订阅登录自然生效。
   快照全程脱敏（新旧 Key 均不进历史）。
4. **Codex 切换语义**（顺序：备份 auth → 写 auth → 改 config.toml）：
   - 切中转站时若 `auth.json` 是官方 OAuth 登录，先整份备份进 Keychain 并
     **先持久化备份引用再覆写**（防崩溃丢登录）；relay→relay 不动备份；
   - `auth.json` 原子写 `{"OPENAI_API_KEY": ...}`（chmod 600），不走
     ConfigEngine（避免 OAuth token 进快照）；
   - `config.toml` 写 `[model_providers.mcpane-<slug>]`（wire_api =
     "responses"），只清理 `mcpane-` 前缀的表，用户自建表不碰；
   - 失败补偿：config.toml 写失败时回滚刚备份的 auth.json；
   - 切回官方：恢复备份、删 Keychain 备份、清 `model_provider` 与全部
     mcpane 表。
5. **backfill**：切走前把 live 配置回填当前 relay profile（base_url/model/
   认证形态/Key 变更回写），失败降级 warning 不阻断。
6. **不迁移 v1**：项目未发布，`providers.json` 读到旧格式（裸数组 /
   schemaVersion≠2）直接重置为空态并告警。

## 影响

- `crates/mcpane-provider`：`domain.rs` 重写（`ProviderProfile` 含模型角色
  映射、认证字段、备注、官网等 UI 对齐字段），`templates.rs` 删除；
- `crates/mcpane-adapter-codex`：新增 `auth.rs`（auth.json 分类/构造）；
- 桌面端：供应商页交互对齐 cc-switch（分段切换器、品牌图标、拖拽排序、
  预设一键填充、官方一键启用），托盘按 app 分组打勾；
- ~~已知取舍：Codex 侧不提供厂商预设（cc-switch 依赖本地代理做
  openai_chat→responses 转换，MCPane 无此代理），仅支持 Responses API 兼容站。~~
  **已被 ADR-014 撤销**：Codex 原生支持 `wire_api = "chat"`，厂商预设
  据端点格式二选一，无需代理；并补齐配置片段/通用配置/统一供应商。
  **2026-09-20 再修订**：Codex 已移除 chat 协议，预设只收原生 Responses 端点
  （见 ADR-014 修订注）。
