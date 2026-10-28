//! Provider 领域对象（v2：官方订阅 + 第三方中转站二分模型）。
//!
//! 每个 Provider 归属单一客户端 app（见 `ClientApp::ALL`），只有两类：
//! - `Official`：官方订阅账号（OAuth 登录），首启按"已安装的客户端"播种进
//!   store，之后与中转站同权——可编辑、可删除、删了不复活；
//! - `Relay`：第三方中转站（base_url + API Key），用户增删改。

use mcpane_secrets::SecretReference;
use std::collections::{BTreeMap, BTreeSet};

/// 导入/导出 Schema 版本（破坏性变更时递增；v1 厂商枚举模型已废弃，不迁移）。
pub const PROFILE_SCHEMA_VERSION: u32 = 2;

/// Provider 归属的客户端 app。
///
/// serde 表示：`lowercase`（不按大写字母拆词）而不是 `kebab-case`。这批变体里
/// 只有 `ClaudeDesktop` 是真正的两词产品名，其余（含 `OpenCode`/`OpenClaw`
/// 这类品牌专有名词）本就该是没有连字符的一整个词——`kebab-case` 会按大写
/// 字母拆分，把 `OpenCode`/`OpenClaw` 错拆成 `open-code`/`open-claw`，
/// 与 [`ClientApp::as_str`]（前端 `lib/clients.ts`/`lib/ipc.ts` 用的字面量）
/// 及 `FromStr` 对不上，导致 `provider_add`/`provider_update` 对这两个客户端
/// 的 `ProviderProfile` 必定反序列化失败（P0，2026-07-28 IPC 契约审计发现）。
/// 类型标识用 `lowercase` 字面量 + 对复合词品牌名单独一条覆盖
/// （如 `ClaudeDesktop`），而不是给每个"恰好是复合词"的品牌名逐个打补丁。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum ClientApp {
    Claude,
    /// Anthropic 桌面 GUI 应用（与 MCP 侧 mcp_clients 的 "claude-desktop" 同名同物）。
    /// 落盘走 Desktop 内置推理网关（claude_desktop 模块），不是 env/config 补丁模型。
    /// 变体序即展示序：与 Claude Code 同族相邻。
    ///
    /// 唯一需要显式覆盖的变体：`lowercase` 默认会拼成 `claudedesktop`，
    /// 但这是"Claude Desktop"这个产品名，本该保留连字符。
    #[serde(rename = "claude-desktop")]
    ClaudeDesktop,
    Codex,
    Gemini,
    /// xAI 的 Grok CLI（`~/.grok/config.toml`，模型表 + 默认模型指针）。
    Grok,
    /// OpenCode（`~/.config/opencode/opencode.json`）。**additive**：多个供应商
    /// 并存于同一份配置，由 OpenCode 自己在 TUI 里选模型，没有"当前生效项"；
    /// 它也不自带模型，因而没有官方订阅。
    OpenCode,
    /// OpenClaw（`~/.openclaw/openclaw.json`，内容是 JSON5）。同为 additive。
    OpenClaw,
    /// Hermes（`~/.hermes/config.yaml`）。additive；供应商容器是数组。
    Hermes,
}

impl ClientApp {
    /// 全部客户端（前端 IPC 的 app 字符串、当前态遍历等按此单一来源展开，
    /// 新增客户端只改这里，杜绝命令层手写 match 漏掉某个 app）。
    /// 顺序即展示序（官方项列表、托盘、标签页）：Claude 系相邻。
    pub const ALL: [ClientApp; 8] = [
        ClientApp::Claude,
        ClientApp::ClaudeDesktop,
        ClientApp::Codex,
        ClientApp::Gemini,
        ClientApp::Grok,
        ClientApp::OpenCode,
        ClientApp::OpenClaw,
        ClientApp::Hermes,
    ];

    /// IPC/序列化用的字面量，与上面枚举定义处的 serde `rename_all`/`rename`
    /// 结果逐一对应（这条一致性由 `domain::tests` 里的 8 变体往返测试兜底，
    /// 改字面量或改 serde 属性但漏改另一边，测试会红）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::ClaudeDesktop => "claude-desktop",
            Self::Grok => "grok",
            Self::OpenCode => "opencode",
            Self::OpenClaw => "openclaw",
            Self::Hermes => "hermes",
        }
    }
}

impl ClientApp {
    /// 该客户端是否存在"官方订阅"这一形态。
    ///
    /// additive 型客户端（OpenCode）自己不提供模型，配置里住的全是第三方
    /// 供应商——给它播种一条"官方订阅"只会是一张切不动的空卡片。
    /// cc-switch 的 `OFFICIAL_SEEDS` 同样不含这类客户端。
    pub fn has_official(self) -> bool {
        !matches!(self, Self::OpenCode | Self::OpenClaw | Self::Hermes)
    }

    /// 该客户端的配置是否 additive（多供应商并存，无单一当前项）。
    /// 决定 UI 用「启用 / 已启用」还是「切换 / 使用中」。
    pub fn is_additive(self) -> bool {
        matches!(self, Self::OpenCode | Self::OpenClaw | Self::Hermes)
    }

    /// 该客户端的配置里是否有"上游协议"这一维度。
    ///
    /// OpenCode 用 `npm`（AI SDK 包名）、OpenClaw 用 `api` 表达它；
    /// **Hermes 的 `custom_providers` 条目里没有对应字段**，存了也落不到配置上，
    /// 所以在这里就挡住——UI 据此不显示下拉，避免"选了没反应"。
    pub fn supports_upstream_api(self) -> bool {
        // Hermes 同样有这一维度：`custom_providers[].api_mode`
        //（`chat_completions` / `anthropic_messages`，cc-switch 恒显式写出）。
        matches!(self, Self::OpenCode | Self::OpenClaw | Self::Hermes)
    }
}

impl std::str::FromStr for ClientApp {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|app| app.as_str() == s)
            .ok_or_else(|| format!("未知客户端 app：{s}"))
    }
}

/// additive 客户端的上游协议。名字取自协议本身而不是某家的字段值，
/// 免得读代码的人以为它只服务于 OpenCode 或只服务于 OpenClaw。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpstreamApi {
    /// OpenAI 兼容的 chat completions（绝大多数中转站）。
    #[default]
    Openai,
    /// Anthropic Messages（Claude 系端点，含各家 Claude 中转站）。
    Anthropic,
}

/// 只支持两类：官方订阅（OAuth）与第三方中转站。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderCategory {
    Official,
    Relay,
}

/// 官方 Provider 的固定 ID（播种与"重新添加官方"都用它，保证幂等）。
pub const OFFICIAL_CLAUDE_ID: &str = "official-claude";
pub const OFFICIAL_CODEX_ID: &str = "official-codex";
pub const OFFICIAL_GEMINI_ID: &str = "official-gemini";
pub const OFFICIAL_CLAUDE_DESKTOP_ID: &str = "official-claude-desktop";
pub const OFFICIAL_GROK_ID: &str = "official-grok";
/// 占位（OpenCode 无官方形态，见 [`ClientApp::has_official`]）。
pub const OFFICIAL_OPENCODE_ID: &str = "official-opencode";
/// 占位（OpenClaw 无官方形态，见 [`ClientApp::has_official`]）。
pub const OFFICIAL_OPENCLAW_ID: &str = "official-openclaw";
/// 占位（Hermes 无官方形态，见 [`ClientApp::has_official`]）。
pub const OFFICIAL_HERMES_ID: &str = "official-hermes";

/// Codex relay 的 `[model_providers.<slug>]` 表名前缀：
/// 只清理带此前缀的表，用户自建表绝不触碰。
pub const CODEX_SLUG_PREFIX: &str = "mcpane-";

/// Codex 1M 上下文窗口（顶层 `model_context_window`）。
/// Codex 缺省按 128K 提前 compact；DeepSeek V4 等官方窗口是 1,048,576。
pub const CODEX_CONTEXT_WINDOW_1M: u64 = 1_000_000;

/// 开启 1M 窗口时配套的自动压缩阈值默认值（窗口的 90%）。
pub const CODEX_AUTO_COMPACT_LIMIT_1M: u64 = 900_000;

/// Codex 中转站的上游协议。Codex 本身只说 Responses：
/// - `Responses`：原生支持 `/responses` 的端点，直连；
/// - `Chat` / `Anthropic`：只有 Chat Completions / Anthropic Messages 的端点，须经本地路由
///   转换（ADR-022），live 恒写 `wire_api = "responses"`；未开路由时切换被拦下。
///
/// Codex 2026-02 起 `wire_api = "chat"` 是反序列化硬错误（openai/codex#7782）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodexWireApi {
    #[default]
    Responses,
    Chat,
    Anthropic,
}

impl CodexWireApi {
    /// 写入 config.toml `wire_api` 字段的字符串值。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Responses => "responses",
            Self::Chat => "chat",
            Self::Anthropic => "anthropic",
        }
    }
}

/// Claude 中转站的认证环境变量名（对齐 cc-switch 的 apiKeyField）：
/// 多数中转站用 `ANTHROPIC_AUTH_TOKEN`（Bearer），少数要求 `ANTHROPIC_API_KEY`（x-api-key）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClaudeAuthField {
    #[default]
    AuthToken,
    ApiKey,
}

impl ClaudeAuthField {
    /// 写入 settings.json env 的键名。
    pub fn env_key(self) -> &'static str {
        match self {
            Self::AuthToken => "ANTHROPIC_AUTH_TOKEN",
            Self::ApiKey => "ANTHROPIC_API_KEY",
        }
    }

    /// 另一种形态的键名（切换时需清除残留）。
    pub fn other_env_key(self) -> &'static str {
        match self {
            Self::AuthToken => "ANTHROPIC_API_KEY",
            Self::ApiKey => "ANTHROPIC_AUTH_TOKEN",
        }
    }
}

/// Claude / Claude Desktop 中转站的上游协议。`OpenaiChat` / `OpenaiResponses` 须经本地路由
/// 转换（ADR-022），此时 `base_url` 是 OpenAI 根地址（如 `…/v1`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClaudeApiFormat {
    #[default]
    Anthropic,
    OpenaiChat,
    OpenaiResponses,
}

/// Claude Desktop 的 profile 写入模式（对齐 cc-switch meta.claudeDesktopMode）：
/// - `Direct`：供应商直接卖 claude-* 模型，profile 直指供应商 base_url；
/// - `Proxy`：非 Claude 厂商，profile 指向本地代理（anthropic 直通 + 模型名映射）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DesktopMode {
    #[default]
    Direct,
    Proxy,
}

/// Claude Desktop 模型路由（对齐 cc-switch ClaudeDesktopModelRoute + routeId）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRoute {
    /// Desktop 模型菜单的 route ID（必须 claude-safe，见 claude_desktop 模块校验）。
    pub route_id: String,
    /// 上游真实模型名（direct 模式必须与 route_id 相同——Desktop 侧不做映射）。
    pub upstream_model: String,
    /// 菜单显示名（品牌名，如 "DeepSeek"）；None 不写 labelOverride。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_override: Option<String>,
    /// 向 Desktop 声明 1M 上下文能力（写 profile 的 supports1m）。
    #[serde(default)]
    pub supports_1m: bool,
}

/// Claude Desktop 本地代理网关的默认监听端口。
/// 刻意不用 cc-switch 的 15721：双装场景两边的代理会互撞端口。
pub const DESKTOP_PROXY_DEFAULT_PORT: u16 = 24871;

fn default_desktop_proxy_port() -> u16 {
    DESKTOP_PROXY_DEFAULT_PORT
}

/// Claude Desktop 本地代理网关配置（对齐 cc-switch settings 表的
/// claude_desktop_gateway_token + global_proxy_config；我们落 provider store）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopGateway {
    /// Desktop → 本地代理的鉴权 token（`mcpane-` 前缀 uuid；仅 loopback 网关用，
    /// 不是上游凭据，故不进系统凭据库——对齐 cc-switch 存 settings 表的取舍）。
    pub token: String,
    /// 本地代理监听端口。
    #[serde(default = "default_desktop_proxy_port")]
    pub listen_port: u16,
}

impl DesktopGateway {
    /// 生成新网关（随机 token + 默认端口）。
    pub fn generate() -> Self {
        Self {
            token: format!("mcpane-{}", uuid::Uuid::new_v4().simple()),
            listen_port: DESKTOP_PROXY_DEFAULT_PORT,
        }
    }
}

/// 本地路由（ADR-022）：客户端经 loopback 路由转发，路由内做故障转移。
/// 对 Claude Code / Codex / Gemini CLI / Grok CLI 生效；网关 token 与端口复用 [`DesktopGateway`]。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingConfig {
    /// 路由接管开关：开启且当前为中转站时，live 配置指向本地路由。
    #[serde(default)]
    pub enabled: bool,
    /// 当前供应商失败时依次尝试的备用中转站 id（同 app）。
    #[serde(default)]
    pub failover_queue: Vec<String>,
    /// 超时与熔断调参；旧数据缺省取默认值。
    #[serde(default)]
    pub tuning: RoutingTuning,
    /// 候选排序策略；旧数据缺省为纯故障转移。
    #[serde(default)]
    pub strategy: RoutingStrategy,
}

/// 候选排序策略。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RoutingStrategy {
    /// 按「当前供应商 → 队列」顺序，失败才转移。
    #[default]
    Failover,
    /// 额度已用尽的候选后置到最后，省去必然失败的一跳；无额度数据的候选不受影响。
    QuotaAware,
}

/// 本地路由调参（秒 / 次），默认值对齐 cc-switch。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RoutingTuning {
    /// 流式请求等响应头的时限。
    pub first_byte_timeout_secs: u32,
    /// 流开始后多久没有任何数据即中止。
    pub idle_timeout_secs: u32,
    /// 连续失败多少次暂停该供应商。
    pub failure_threshold: u32,
    /// 暂停多久后再试探恢复。
    pub cooldown_secs: u32,
}

impl Default for RoutingTuning {
    fn default() -> Self {
        Self {
            first_byte_timeout_secs: 60,
            idle_timeout_secs: 120,
            failure_threshold: 4,
            cooldown_secs: 60,
        }
    }
}

impl RoutingTuning {
    /// 各项的允许范围：过小会误判正常的慢响应，过大等于关掉看门。
    pub const RANGES: [(&'static str, u32, u32); 4] = [
        ("首字节超时", 10, 600),
        ("无数据中止", 30, 1800),
        ("连续失败次数", 1, 20),
        ("暂停时长", 10, 3600),
    ];

    pub fn validate(&self) -> Result<(), String> {
        let values = [
            self.first_byte_timeout_secs,
            self.idle_timeout_secs,
            self.failure_threshold,
            self.cooldown_secs,
        ];
        for ((label, min, max), value) in Self::RANGES.iter().zip(values) {
            if !(*min..=*max).contains(&value) {
                return Err(format!("{label}须在 {min}–{max} 之间，当前为 {value}"));
            }
        }
        Ok(())
    }
}

impl ClientApp {
    /// 支持本地路由接管的客户端。
    pub fn supports_routing(self) -> bool {
        router_namespace(self).is_some()
    }
}

/// 路由命名空间路径段（`/claude`、`/codex`）；不支持路由的 app 为 None。
pub fn router_namespace(app: ClientApp) -> Option<&'static str> {
    match app {
        ClientApp::Claude => Some("claude"),
        ClientApp::Codex => Some("codex"),
        ClientApp::Gemini => Some("gemini"),
        ClientApp::Grok => Some("grok"),
        _ => None,
    }
}

/// 写进客户端 live 配置的本地路由地址。
pub fn router_base_url(app: ClientApp, port: u16) -> Option<String> {
    router_namespace(app).map(|ns| format!("http://127.0.0.1:{port}/{ns}"))
}

/// live 里的 base_url 是否为本地路由地址（回填时据此跳过，防止 loopback
/// 地址与网关 token 倒灌进供应商记录）。
pub fn is_router_url(url: &str) -> bool {
    let Some(rest) = url.trim().strip_prefix("http://127.0.0.1:") else {
        return false;
    };
    let path = rest.split_once('/').map(|(_, path)| path).unwrap_or("");
    let first = path.split('/').next().unwrap_or("");
    ClientApp::ALL
        .into_iter()
        .filter_map(router_namespace)
        .any(|ns| ns == first)
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfile {
    pub schema_version: u32,
    pub id: String,
    pub app: ClientApp,
    pub category: ProviderCategory,
    pub name: String,
    /// relay 必填；official 恒为 None。
    pub base_url: Option<String>,
    /// 可选模型覆盖（Claude 写 `ANTHROPIC_MODEL` 兜底模型；Codex 写顶层 `model`）。
    pub model: Option<String>,
    /// Claude 模型角色映射（写 `ANTHROPIC_DEFAULT_HAIKU_MODEL`；Codex 忽略）。
    #[serde(default)]
    pub model_haiku: Option<String>,
    /// Claude 模型角色映射（写 `ANTHROPIC_DEFAULT_SONNET_MODEL`；Codex 忽略）。
    #[serde(default)]
    pub model_sonnet: Option<String>,
    /// Claude 模型角色映射（写 `ANTHROPIC_DEFAULT_OPUS_MODEL`；Codex 忽略）。
    #[serde(default)]
    pub model_opus: Option<String>,
    /// Claude 模型角色映射（写 `ANTHROPIC_DEFAULT_FABLE_MODEL`；Codex 忽略）。
    ///
    /// 第四个角色槽。除了决定 `fable` 别名解析到哪个模型，官方文档还说明它是
    /// Claude Code 在**第三方供应商**上识别 Fable 5 以启用自动模型回落的依据，
    /// 所以中转站形态下不能空着。
    #[serde(default)]
    pub model_fable: Option<String>,
    /// 全部子代理 / agent team / workflow 的模型
    ///（写 `CLAUDE_CODE_SUBAGENT_MODEL`；Codex 忽略）。
    ///
    /// 与四个角色槽不同：**不做回落**。空着就让 Claude Code 走正常的别名解析
    /// （即上面四个槽），填了才覆盖每次调用的 `model` 参数与子代理 frontmatter。
    #[serde(default)]
    pub model_subagent: Option<String>,
    /// Claude 认证环境变量形态；Codex 忽略。
    #[serde(default)]
    pub api_key_field: ClaudeAuthField,
    /// Codex config.toml 的 wire_api；Claude 忽略。
    #[serde(default)]
    pub codex_wire_api: CodexWireApi,
    /// Claude / Claude Desktop 的上游协议；其他 app 忽略。
    #[serde(default)]
    pub claude_api_format: ClaudeApiFormat,
    /// Claude Desktop 的写入模式；其他 app 忽略。
    #[serde(default)]
    pub desktop_mode: DesktopMode,
    /// Claude Desktop 模型路由表；其他 app 忽略。
    /// direct 模式空表 = 不写 inferenceModels（Desktop 自读 /v1/models）。
    #[serde(default)]
    pub desktop_routes: Vec<DesktopRoute>,
    /// 附加配置片段（对齐 cc-switch 的「配置 JSON」）：
    /// - Claude：settings.json 的任意 JSON 子集（`env.API_TIMEOUT_MS`、
    ///   `attribution`、`permissions` 等）；
    /// - Codex：config.toml 的任意子树（`model_reasoning_effort` 等），
    ///   以 JSON 对象表达 TOML 结构。
    ///
    /// 切换时深合并写入，切走时按"值仍与写入时一致"深移除（不动用户手改）。
    /// 恒为 JSON object 或 Null（空）。
    #[serde(default)]
    pub extra_settings: serde_json::Value,
    /// 是否叠加所属 app 的全局「通用配置」片段
    ///（`ProviderState.claude_common_snippet` / `codex_common_snippet`）。
    #[serde(default)]
    pub common_config_enabled: bool,
    /// 请求地址候选列表（供测速/一键切换）。
    #[serde(default)]
    pub endpoint_candidates: Vec<String>,
    /// 模型上下文长度（**只有 Hermes 用**，写 `models.<id>.context_length`）。
    /// cc-switch 的预设里带的是逐条查出来的具体数值，我们没有那份数据——所以
    /// 不猜：`None` 就不写这个键，让 Hermes 用它自己的默认值。
    #[serde(default)]
    pub context_length: Option<u64>,
    /// 自定义请求头（**只有 additive 客户端用它**）：
    /// OpenCode 落 `options.headers`、OpenClaw 落 `headers`。有些中转站要求
    /// 额外的鉴权/路由头，没有这个就接不上。
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// 本地路由接管时对上游请求体的覆盖（JSON Merge Patch：null 删键；Null = 无）。
    /// 协议转换后客户端表达不了的厂商参数（如 `enable_thinking`）靠它补；
    /// 同理，路由接管的 app 也把上面的 `headers` 作为上游请求头发送。
    #[serde(default)]
    pub route_body_override: serde_json::Value,
    /// 上游协议（**只有 additive 客户端用它**）。同一件事在
    /// 两家有两种写法——OpenCode 是 AI SDK 包名（`@ai-sdk/openai-compatible`
    /// / `@ai-sdk/anthropic`），OpenClaw 是 `api` 字段
    /// （`openai-completions` / `anthropic-messages`）。这里统一成一个维度，
    /// 由各自 adapter 映射过去。`None` = OpenAI 兼容（两家的默认）。
    #[serde(default)]
    pub upstream_api: Option<UpstreamApi>,
    /// 声明给客户端的模型列表。**只有 additive 客户端用它**
    ///（OpenCode / OpenClaw 只认显式声明过的模型，且是一组而非一个）。
    /// 切换式客户端走 `model` 与角色映射字段，不看这里。
    #[serde(default)]
    pub models: Vec<String>,
    /// Codex 请求的 URL query 参数（写入 `[model_providers.<slug>.query_params]`，
    /// Azure OpenAI 需要 `api-version`）；空表不写且清除残留。Claude 忽略。
    #[serde(default)]
    pub codex_query_params: BTreeMap<String, String>,
    /// Codex 1M 上下文窗口。开启时写入顶层 `model_context_window = 1000000`，
    /// 关闭时两个键都删。Claude 忽略。
    #[serde(default)]
    pub codex_context_window_1m: bool,
    /// 开启 1M 时写入的 `model_auto_compact_token_limit`；`None` 用
    /// [`CODEX_AUTO_COMPACT_LIMIT_1M`]。未开 1M 时忽略。
    #[serde(default)]
    pub codex_auto_compact_limit: Option<u64>,
    /// 显式图标 key（前端内置品牌图标表；None 时按名称推断/首字母兜底。
    /// 对齐 cc-switch meta.icon，不写入客户端配置）。
    #[serde(default)]
    pub icon: Option<String>,
    /// 备注（卡片第二行优先展示；不写入客户端配置）。
    #[serde(default)]
    pub notes: Option<String>,
    /// 官网/控制台地址（卡片外链与「获取 API Key」入口；不写入客户端配置）。
    #[serde(default)]
    pub website_url: Option<String>,
    /// relay 的 API Key 引用（明文只存系统凭据库）；official 恒为 None。
    pub api_key_ref: Option<SecretReference>,
    /// Codex relay 的 `[model_providers.<slug>]` 表名，创建时生成后冻结
    ///（改名不变，保证切走时可清理）。
    pub codex_slug: Option<String>,
    pub sort_order: i64,
}

impl ProviderProfile {
    /// 新建一个中转站 Profile（Codex 的 slug 由 Service 在入库时补齐去重）。
    pub fn new_relay(app: ClientApp, name: impl Into<String>) -> Self {
        Self {
            schema_version: PROFILE_SCHEMA_VERSION,
            id: uuid::Uuid::new_v4().simple().to_string(),
            app,
            category: ProviderCategory::Relay,
            name: name.into(),
            base_url: None,
            model: None,
            model_haiku: None,
            model_sonnet: None,
            model_opus: None,
            model_fable: None,
            model_subagent: None,
            api_key_field: ClaudeAuthField::default(),
            codex_wire_api: CodexWireApi::default(),
            claude_api_format: ClaudeApiFormat::default(),
            desktop_mode: DesktopMode::default(),
            desktop_routes: Vec::new(),
            extra_settings: serde_json::Value::Null,
            common_config_enabled: false,
            endpoint_candidates: Vec::new(),
            models: Vec::new(),
            headers: BTreeMap::new(),
            route_body_override: serde_json::Value::Null,
            context_length: None,
            upstream_api: None,
            codex_query_params: BTreeMap::new(),
            codex_context_window_1m: false,
            codex_auto_compact_limit: None,
            icon: None,
            notes: None,
            website_url: None,
            api_key_ref: None,
            codex_slug: None,
            sort_order: 0,
        }
    }

    /// 官方 Provider 的播种模板（固定 ID；`seed_officials` 落 store 后即为普通记录）。
    pub fn official(app: ClientApp) -> Self {
        let (id, name) = match app {
            ClientApp::Claude => (OFFICIAL_CLAUDE_ID, "Claude 官方订阅"),
            ClientApp::Codex => (OFFICIAL_CODEX_ID, "ChatGPT 官方订阅"),
            // Gemini CLI 官方形态是 Google OAuth 登录（免费层/Code Assist），
            // 切换只设 selectedType=oauth-personal，凭据由 CLI 自己管理。
            ClientApp::Gemini => (OFFICIAL_GEMINI_ID, "Google 官方登录"),
            // Desktop 官方 = 应用内置 1P 登录；切换即恢复 deploymentMode "1p"。
            ClientApp::ClaudeDesktop => (OFFICIAL_CLAUDE_DESKTOP_ID, "Claude Desktop 官方登录"),
            // Grok CLI 官方形态 = 配置里没有自定义模型表，CLI 走内置模型 +
            // 自带的 xAI OAuth 登录，凭据由它自管。
            ClientApp::Grok => (OFFICIAL_GROK_ID, "xAI 官方登录"),
            // 不会走到：has_official() 已在播种/ensure 前挡掉（无官方形态）。
            ClientApp::OpenCode => (OFFICIAL_OPENCODE_ID, "OpenCode"),
            ClientApp::OpenClaw => (OFFICIAL_OPENCLAW_ID, "OpenClaw"),
            ClientApp::Hermes => (OFFICIAL_HERMES_ID, "Hermes"),
        };
        Self {
            schema_version: PROFILE_SCHEMA_VERSION,
            id: id.to_owned(),
            app,
            category: ProviderCategory::Official,
            name: name.to_owned(),
            base_url: None,
            model: None,
            model_haiku: None,
            model_sonnet: None,
            model_opus: None,
            model_fable: None,
            model_subagent: None,
            api_key_field: ClaudeAuthField::default(),
            codex_wire_api: CodexWireApi::default(),
            claude_api_format: ClaudeApiFormat::default(),
            desktop_mode: DesktopMode::default(),
            desktop_routes: Vec::new(),
            extra_settings: serde_json::Value::Null,
            common_config_enabled: false,
            endpoint_candidates: Vec::new(),
            models: Vec::new(),
            headers: BTreeMap::new(),
            route_body_override: serde_json::Value::Null,
            context_length: None,
            upstream_api: None,
            codex_query_params: BTreeMap::new(),
            codex_context_window_1m: false,
            codex_auto_compact_limit: None,
            icon: None,
            notes: None,
            website_url: None,
            api_key_ref: None,
            codex_slug: None,
            sort_order: -1,
        }
    }
}

/// 由名称生成 Codex slug：`mcpane-<sanitized>`，与 `taken` 冲突时追加 `-2`/`-3`…
pub fn generate_codex_slug(name: &str, taken: &[String]) -> String {
    let sanitized: String = name
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned();
    let base = if sanitized.is_empty() {
        format!("{CODEX_SLUG_PREFIX}relay")
    } else {
        format!("{CODEX_SLUG_PREFIX}{sanitized}")
    };
    if !taken.contains(&base) {
        return base;
    }
    let mut n = 2u64;
    loop {
        let candidate = format!("{base}-{n}");
        if !taken.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// 【历史结构】官方项曾是运行时合成物，可编辑项单独存在这里。
/// 官方项落库后本结构不再产生新数据，只在播种时被读取并迁移进官方 Profile。
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OfficialOverride {
    /// settings.json 附加片段（同 ProviderProfile::extra_settings 语义）。
    #[serde(default)]
    pub extra_settings: serde_json::Value,
    #[serde(default)]
    pub common_config_enabled: bool,
}

// ---------- 统一供应商（Universal Provider，对齐 cc-switch） ----------

/// 统一供应商影子条目的 id 前缀：`unified-<app>-<uid>`。
/// 影子条目是普通 ProviderProfile（切换/托盘全走既有机制），
/// 前端靠该前缀识别并把编辑入口路由回统一供应商。
pub const UNIFIED_ID_PREFIX: &str = "unified-";

/// 统一供应商在某 app 下的影子条目 id。
pub fn unified_shadow_id(app: ClientApp, unified_id: &str) -> String {
    let app_str = match app {
        ClientApp::Claude => "claude",
        ClientApp::Codex => "codex",
        ClientApp::Grok => "grok",
        ClientApp::OpenCode => "opencode",
        ClientApp::OpenClaw => "openclaw",
        ClientApp::Hermes => "hermes",
        // 统一供应商暂不物化 Gemini / Claude Desktop / Grok 影子（Gemini 原生协议
        // 网关罕见；Desktop 有独立的模式/路由维度；Grok 的模型表要 context_window
        // 等专属字段），但 id 规则先占位保持函数全射。
        ClientApp::Gemini => "gemini",
        ClientApp::ClaudeDesktop => "claude-desktop",
    };
    format!("{UNIFIED_ID_PREFIX}{app_str}-{unified_id}")
}

/// 统一供应商的应用开关。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnifiedApps {
    pub claude: bool,
    pub codex: bool,
}

/// 统一供应商（对齐 cc-switch Universal Provider）：一份 base_url + API Key，
/// 按 app 开关物化成各 app 的影子条目，适用于 NewAPI 等多协议网关。
/// 与 cc-switch 的差异：upsert 即同步（我们的影子是结构化派生数据，
/// 无自由 JSON 需要 merge 保留，不需要独立的 sync 步骤）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnifiedProvider {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub apps: UnifiedApps,
    /// Claude 兜底模型与角色映射。
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub model_haiku: Option<String>,
    #[serde(default)]
    pub model_sonnet: Option<String>,
    #[serde(default)]
    pub model_opus: Option<String>,
    #[serde(default)]
    pub model_fable: Option<String>,
    /// Codex 顶层 model。
    #[serde(default)]
    pub codex_model: Option<String>,
    /// API Key 引用（明文只进系统凭据库）；影子条目持有各自的副本引用。
    pub api_key_ref: Option<SecretReference>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub website_url: Option<String>,
    pub sort_order: i64,
}

impl UnifiedProvider {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            schema_version: PROFILE_SCHEMA_VERSION,
            id: uuid::Uuid::new_v4().simple().to_string(),
            name: name.into(),
            base_url: String::new(),
            apps: UnifiedApps {
                claude: true,
                codex: true,
            },
            model: None,
            model_haiku: None,
            model_sonnet: None,
            model_opus: None,
            model_fable: None,
            codex_model: None,
            api_key_ref: None,
            notes: None,
            website_url: None,
            sort_order: 0,
        }
    }
}

/// 网关 base_url → Codex base_url 的三态 /v1 补全（对齐 cc-switch 后端实现，
/// 前端简化版与之不一致，以此为准）：
/// - 已以 `/v1` 结尾 → 原样；
/// - 纯 origin（scheme:// 后无路径）→ 补 `/v1`；
/// - 带自定义路径前缀 → 不强补。
pub fn codex_base_url_for_gateway(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    let origin_only = match trimmed.split_once("://") {
        Some((_scheme, rest)) => !rest.contains('/'),
        None => !trimmed.contains('/'),
    };
    if trimmed.ends_with("/v1") {
        trimmed.to_owned()
    } else if origin_only {
        format!("{trimmed}/v1")
    } else {
        trimmed.to_owned()
    }
}

/// `providers.json` v2 顶层结构（不再是裸数组）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderState {
    pub schema_version: u32,
    /// 中转站与（已播种的）官方项——官方项落库后与中转站同权。
    pub providers: Vec<ProviderProfile>,
    /// 每个 app 当前生效的 Provider id（从未切换过则无键）。
    #[serde(default)]
    pub current: BTreeMap<ClientApp, String>,
    /// 统一供应商（跨 app 共享配置；影子条目在 `providers` 中）。
    #[serde(default)]
    pub unified: Vec<UnifiedProvider>,
    /// 各客户端配置目录的用户覆盖（app → 绝对路径）。默认位置不适用时用它——
    /// 多账号隔离、便携盘、公司规范目录都会把配置放在别处，写死路径的话这些
    /// 用户连"装没装"都探测不出来。对齐 cc-switch 的 `*_override_dir`。
    ///
    /// 放在这里而不是 SQLite 的 app_settings：`client_paths()` 是同步函数、在
    /// 启动路径上就要用，而 settings 仓储是 async。
    #[serde(default)]
    pub client_config_dirs: BTreeMap<ClientApp, String>,
    /// 已播种过官方项的 app（对齐 cc-switch 的 `official_providers_seeded` 语义）。
    /// 播种只发生一次：用户删掉官方项后不会在下次启动复活。按 app 记录而非
    /// 全局 flag，是为了让"先装 MCPane、后装 Gemini CLI"的用户也能补种。
    #[serde(default)]
    pub seeded_officials: BTreeSet<ClientApp>,
    /// 【历史字段】官方项曾是运行时合成物，可编辑项存这里。播种时一次性迁移进
    /// 官方 Profile 后清空；保留字段只为读得懂老 providers.json。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub official_overrides: BTreeMap<ClientApp, OfficialOverride>,
    /// 每个 app 上一次切换时**实际写入**的有效附加片段（含叠加的通用配置）。
    /// 清理残留一律以此为准：即使切换后用户改了 profile 的片段，
    /// 也能准确清掉当时写入的键；同一供应商重新启用同样先清后写。
    #[serde(default)]
    pub applied_fragments: BTreeMap<ClientApp, serde_json::Value>,
    /// Claude 全局「通用配置」片段（settings.json 子集，跨供应商共享）。
    /// 启用了 `common_config_enabled`
    /// 的中转站在切换时把它深合并进写入片段（同名键以通用配置为准）。
    #[serde(default)]
    pub claude_common_snippet: Option<serde_json::Value>,
    /// Codex 全局「通用配置」片段（config.toml 子树的 JSON 表达）。
    #[serde(default)]
    pub codex_common_snippet: Option<serde_json::Value>,
    /// Gemini 全局「通用配置」片段（settings.json 子集）。
    #[serde(default)]
    pub gemini_common_snippet: Option<serde_json::Value>,
    /// Grok 全局「通用配置」片段（config.toml 子树的 JSON 表达）。
    #[serde(default)]
    pub grok_common_snippet: Option<serde_json::Value>,
    /// OpenCode 全局「通用配置」片段（opencode.json 子集）。
    #[serde(default)]
    pub opencode_common_snippet: Option<serde_json::Value>,
    /// OpenClaw 全局「通用配置」片段（openclaw.json 子集）。
    #[serde(default)]
    pub openclaw_common_snippet: Option<serde_json::Value>,
    /// Hermes 全局「通用配置」片段（config.yaml 子集）。
    #[serde(default)]
    pub hermes_common_snippet: Option<serde_json::Value>,
    /// Claude Desktop 通用片段槽位（trait 全射占位）：Desktop profile 由 MCPane
    /// 全量生成，不支持附加片段叠加；UI 不暴露，恒为 None。
    #[serde(default)]
    pub desktop_common_snippet: Option<serde_json::Value>,
    /// Codex 官方 `auth.json` 的整份备份（内容在系统凭据库，这里只存引用）。
    #[serde(default)]
    pub codex_auth_backup: Option<SecretReference>,
    /// Claude Desktop 本地代理网关（proxy 模式首次切换时生成，之后复用）。
    #[serde(default)]
    pub desktop_gateway: Option<DesktopGateway>,
    /// 各 app 的本地路由配置（ADR-022）。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub routing: BTreeMap<ClientApp, RoutingConfig>,
}

impl ProviderState {
    /// 供应商被移除时同步清掉各 app 故障转移队列里的引用（所有移除路径共用）。
    pub fn drop_from_failover_queues(&mut self, id: &str) {
        for routing in self.routing.values_mut() {
            routing.failover_queue.retain(|queued| queued != id);
        }
    }
}

impl Default for ProviderState {
    fn default() -> Self {
        Self {
            schema_version: PROFILE_SCHEMA_VERSION,
            providers: Vec::new(),
            current: BTreeMap::new(),
            unified: Vec::new(),
            client_config_dirs: BTreeMap::new(),
            seeded_officials: BTreeSet::new(),
            official_overrides: BTreeMap::new(),
            applied_fragments: BTreeMap::new(),
            claude_common_snippet: None,
            codex_common_snippet: None,
            gemini_common_snippet: None,
            grok_common_snippet: None,
            opencode_common_snippet: None,
            openclaw_common_snippet: None,
            hermes_common_snippet: None,
            desktop_common_snippet: None,
            codex_auth_backup: None,
            desktop_gateway: None,
            routing: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn router_url_roundtrip() {
        let url = router_base_url(ClientApp::Claude, 15721).unwrap();
        assert_eq!(url, "http://127.0.0.1:15721/claude");
        assert!(is_router_url(&url));
        assert!(is_router_url("http://127.0.0.1:15721/codex"));
        assert!(!is_router_url("http://127.0.0.1:15721/claude-desktop"));
        assert!(!is_router_url("https://api.example.com/claude"));
        assert!(is_router_url("http://127.0.0.1:15721/gemini"));
        assert!(is_router_url("http://127.0.0.1:15721/grok"));
        assert!(router_base_url(ClientApp::OpenCode, 1).is_none());
    }

    #[test]
    fn export_json_contains_no_secret_material() {
        let mut p = ProviderProfile::new_relay(ClientApp::Claude, "某中转站");
        p.api_key_ref = Some(SecretReference::generate());
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("mcpane/"));
        assert!(!json.to_lowercase().contains("sk-"));
        assert!(json.contains("\"schemaVersion\":2"));
        assert!(json.contains("\"app\":\"claude\""));
        assert!(json.contains("\"category\":\"relay\""));
    }

    #[test]
    fn official_profiles_have_fixed_ids() {
        assert_eq!(
            ProviderProfile::official(ClientApp::Claude).id,
            OFFICIAL_CLAUDE_ID
        );
        assert_eq!(
            ProviderProfile::official(ClientApp::Codex).id,
            OFFICIAL_CODEX_ID
        );
        assert_eq!(
            ProviderProfile::official(ClientApp::ClaudeDesktop).id,
            OFFICIAL_CLAUDE_DESKTOP_ID
        );
        assert_eq!(
            ProviderProfile::official(ClientApp::Claude).category,
            ProviderCategory::Official
        );
    }

    #[test]
    fn claude_desktop_slug_roundtrips() {
        assert_eq!(ClientApp::ClaudeDesktop.as_str(), "claude-desktop");
        assert_eq!(
            "claude-desktop".parse::<ClientApp>().unwrap(),
            ClientApp::ClaudeDesktop
        );
        // serde 表示与 as_str 一致（前端 IPC 依赖）。
        assert_eq!(
            serde_json::to_string(&ClientApp::ClaudeDesktop).unwrap(),
            "\"claude-desktop\""
        );
    }

    #[test]
    fn profile_deserializes_without_desktop_fields() {
        // 旧版 providers.json 无 desktopMode/desktopRoutes，必须能加载并取默认值。
        let json = serde_json::to_string(&ProviderProfile::new_relay(ClientApp::Claude, "旧站"))
            .unwrap()
            .replace(",\"desktopMode\":\"direct\"", "")
            .replace(",\"desktopRoutes\":[]", "");
        assert!(!json.contains("desktopMode"), "替换未生效：{json}");
        let back: ProviderProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.desktop_mode, DesktopMode::Direct);
        assert!(back.desktop_routes.is_empty());
    }

    #[test]
    fn desktop_gateway_generates_prefixed_token_and_default_port() {
        let gw = DesktopGateway::generate();
        assert!(gw.token.starts_with("mcpane-"));
        assert!(gw.token.len() > "mcpane-".len() + 20);
        assert_eq!(gw.listen_port, DESKTOP_PROXY_DEFAULT_PORT);
        // 旧 state 缺 listenPort 时按默认端口反序列化。
        let back: DesktopGateway = serde_json::from_str("{\"token\":\"mcpane-x\"}").unwrap();
        assert_eq!(back.listen_port, DESKTOP_PROXY_DEFAULT_PORT);
    }

    #[test]
    fn codex_slug_sanitizes_and_dedupes() {
        assert_eq!(generate_codex_slug("My Relay!", &[]), "mcpane-my-relay");
        assert_eq!(generate_codex_slug("中文站", &[]), "mcpane-relay");
        let taken = vec!["mcpane-my-relay".to_owned(), "mcpane-my-relay-2".to_owned()];
        assert_eq!(generate_codex_slug("My Relay", &taken), "mcpane-my-relay-3");
    }

    #[test]
    fn codex_gateway_base_url_three_state_v1_completion() {
        // 已有 /v1 → 原样（含尾斜杠归一）。
        assert_eq!(
            codex_base_url_for_gateway("https://gw.example.com/v1/"),
            "https://gw.example.com/v1"
        );
        // 纯 origin → 补 /v1。
        assert_eq!(
            codex_base_url_for_gateway("https://gw.example.com"),
            "https://gw.example.com/v1"
        );
        // 自定义路径前缀 → 不强补。
        assert_eq!(
            codex_base_url_for_gateway("https://gw.example.com/openai"),
            "https://gw.example.com/openai"
        );
    }

    #[test]
    fn unified_shadow_ids_are_prefixed_per_app() {
        let id = unified_shadow_id(ClientApp::Claude, "abc123");
        assert_eq!(id, "unified-claude-abc123");
        assert!(id.starts_with(UNIFIED_ID_PREFIX));
        assert_eq!(
            unified_shadow_id(ClientApp::Codex, "abc123"),
            "unified-codex-abc123"
        );
    }

    #[test]
    fn profile_deserializes_without_new_optional_fields() {
        // 老版本 providers.json 里没有 codexWireApi / extraSettings 等新字段，必须能加载。
        let json = serde_json::to_string(&ProviderProfile::new_relay(ClientApp::Codex, "旧站"))
            .unwrap()
            .replace(",\"codexWireApi\":\"responses\"", "")
            .replace(",\"extraSettings\":null", "")
            .replace(",\"commonConfigEnabled\":false", "")
            .replace(",\"codexContextWindow1m\":false", "")
            .replace(",\"codexAutoCompactLimit\":null", "");
        let back: ProviderProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.codex_wire_api, CodexWireApi::Responses);
        assert!(back.extra_settings.is_null());
        assert!(!back.common_config_enabled);
        assert!(!back.codex_context_window_1m);
        assert_eq!(back.codex_auto_compact_limit, None);
    }

    #[test]
    fn state_current_map_serializes_with_kebab_keys() {
        let mut state = ProviderState::default();
        state.current.insert(ClientApp::Claude, "abc".into());
        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains("\"claude\":\"abc\""));
        let back: ProviderState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, state);
    }

    // ---------- 审计发现：ClientApp 的 serde derive 曾与 as_str() 不一致 ----------
    //
    // 曾经的 `#[serde(rename_all = "kebab-case")]` 会按大写字母拆分变体名；
    // `as_str()`（domain.rs 里紧邻 ALL 的那个方法）是手写的，对 OpenCode/
    // OpenClaw 这两个品牌专有名词特意不拆。两条路径本该产出同一个字符串
    // （前端 lib/clients.ts / lib/ipc.ts 的 ClientApp 字面量），但 kebab-case
    // 在这两个变体上其实拆了——`provider_add`/`provider_update` 对这两个客户端
    // 的 `ProviderProfile` 必定反序列化失败（P0）。现已改用 `lowercase` +
    // `ClaudeDesktop` 单独 rename（同 cc-switch `AppType` 的做法）修复，
    // 下面这组测试就是当时的红灯，现在应全绿。

    #[test]
    fn client_app_opencode_serde_matches_frontend_literal() {
        assert_eq!(
            ClientApp::OpenCode.as_str(),
            "opencode",
            "as_str() 的字面量（前端 lib/clients.ts:108 / lib/ipc.ts 联合类型用的就是这个）"
        );
        assert_eq!(
            serde_json::to_string(&ClientApp::OpenCode).unwrap(),
            "\"opencode\"",
            "serde 实际序列化值应与 as_str() 一致"
        );
        // 反序列化前端实际发送的 "opencode" 应该成功——这正是 provider_add/
        // provider_update 收到 ProviderProfile.app 时要做的事。
        let parsed: ClientApp = serde_json::from_str(r#""opencode""#).unwrap();
        assert_eq!(parsed, ClientApp::OpenCode);
    }

    #[test]
    fn client_app_openclaw_serde_matches_frontend_literal() {
        assert_eq!(ClientApp::OpenClaw.as_str(), "openclaw");
        assert_eq!(
            serde_json::to_string(&ClientApp::OpenClaw).unwrap(),
            "\"openclaw\""
        );
        // 反序列化前端实际发送的 "openclaw" 应该成功。
        let parsed: ClientApp = serde_json::from_str(r#""openclaw""#).unwrap();
        assert_eq!(parsed, ClientApp::OpenClaw);
    }

    /// 对照组：ClaudeDesktop 的 as_str()（"claude-desktop"）与 serde 表示
    /// 本就一致（`#[serde(rename = "claude-desktop")]` 显式覆盖）——这条必须绿，
    /// 证明测试方法本身有效，不是所有变体都炸，只有 OpenCode/OpenClaw 这两个
    /// 内部含大写字母的品牌名曾被 kebab-case 拆开。
    #[test]
    fn client_app_claude_desktop_serde_matches_frontend_literal_control_group() {
        assert_eq!(ClientApp::ClaudeDesktop.as_str(), "claude-desktop");
        assert_eq!(
            serde_json::to_string(&ClientApp::ClaudeDesktop).unwrap(),
            "\"claude-desktop\""
        );
        let parsed: ClientApp = serde_json::from_str(r#""claude-desktop""#).unwrap();
        assert_eq!(parsed, ClientApp::ClaudeDesktop);
    }

    /// 全部 8 个变体的 serde 往返契约（frontend-gate 建议补齐，堵住这整类
    /// bug）：`as_str()` 与 serde 序列化值必须逐一相等，且序列化后能整段
    /// 反序列化回同一个变体——不止查 OpenCode/OpenClaw 这两个已知肇事的，
    /// 新增变体时忘了给 as_str()/serde 属性对齐也会在这里被抓到。
    #[test]
    fn client_app_all_variants_serde_roundtrip_matches_as_str() {
        for app in ClientApp::ALL {
            let literal = app.as_str();
            let serialized = serde_json::to_string(&app).unwrap();
            assert_eq!(
                serialized,
                format!("\"{literal}\""),
                "{app:?} 的 serde 序列化值应与 as_str() 一致"
            );
            let parsed: ClientApp = serde_json::from_str(&serialized)
                .unwrap_or_else(|e| panic!("{app:?} 用自己的序列化值反序列化应成功：{e}"));
            assert_eq!(parsed, app, "{app:?} 序列化后反序列化应得到原变体");
            // 前端字面量本身就是 as_str() 的值（见 lib/clients.ts），这里再
            // 显式核一遍"用 as_str() 的字面量能反序列化回同一变体"，覆盖
            // "两边字面量凑巧对上但其中一边其实没被测到"的情况。
            let from_literal: ClientApp = serde_json::from_str(&format!("\"{literal}\""))
                .unwrap_or_else(|e| panic!("{app:?} 用 as_str() 字面量反序列化应成功：{e}"));
            assert_eq!(from_literal, app);
        }
    }
}
