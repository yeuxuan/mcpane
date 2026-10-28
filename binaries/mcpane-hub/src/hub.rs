//! MCP Hub 的 MCP Server 实现：内建 Tool + 下游聚合路由（TASK-406）。
//!
//! - 内建 Tool 一律使用冻结前缀 `mcpane__`（ADR-006 / ADR-011）；
//! - 每个 Session 在创建时 pin 一份 Catalog Revision（ADR-004）：
//!   会话内工具名、Schema、路由目标保持稳定；
//! - `tools/call` 对禁用 Tool 做二次权限校验并返回明确错误。

use std::sync::{Arc, OnceLock};

use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    handler::server::tool::ToolCallContext,
    handler::server::wrapper::{Json, Parameters},
    model::{
        CallToolRequestParams, CallToolResponse, GetPromptRequestParams, GetPromptResponse,
        Implementation, ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult,
        ListToolsResult, PaginatedRequestParams, Prompt, ProtocolVersion,
        ReadResourceRequestParams, ReadResourceResponse, Resource, ResourceTemplate,
        ServerCapabilities, ServerInfo, Tool,
    },
    schemars,
    service::{RequestContext, SubscriptionContext},
    tool, tool_router,
};
use serde::{Deserialize, Serialize};

use crate::auth::{ClientIdentity, ClientType};
use crate::guard::RequestId;
use crate::state::AggregationState;
use crate::{MCP_PROTOCOL_VERSION, SERVER_VERSION};

/// 内建 Tool 前缀（冻结标识）。唯一事实源在 `mcpane-hub-catalog`——那边
/// 是暴露名的生成/校验发生的地方（`exposed_name`/`is_valid_alias`），常量
/// 若在这里另存一份，两边随时可能各自改动而互不知情，此前正是如此：
/// catalog crate 对这个前缀一无所知，用户能把下游工具 alias 成
/// `mcpane__echo` 而不被拒绝（见该 crate 的回归测试）。
pub use mcpane_hub_catalog::TOOL_PREFIX;

/// 网关模式的 meta 工具名（list_tools 按模式显隐时用）。
pub const TOOL_DISCOVERY_NAME: &str = "mcpane__tool_discovery";
pub const TOOL_EXECUTE_NAME: &str = "mcpane__tool_execute";
pub const TOOL_DESCRIBE_NAME: &str = "mcpane__tool_describe";

/// 能力索引里每个 Server 至多罗列的工具名数。
const INDEX_NAMES_PER_SERVER: usize = 10;

/// 发现模式的能力索引：按 Server 罗列工具名，挂进 mcpane__tool_discovery 的
/// description 常驻上下文。全隐藏时模型不知道下游有什么，连搜索的念头都不会起；
/// 工具名每个只占几 token，是「知道存在、细节按需检索」的最小披露量。
fn capability_index(visible: &[&mcpane_hub_catalog::CatalogTool]) -> Option<String> {
    if visible.is_empty() {
        return None;
    }
    let mut by_server: std::collections::BTreeMap<&str, Vec<&str>> =
        std::collections::BTreeMap::new();
    for tool in visible {
        by_server
            .entry(tool.server_slug.as_str())
            .or_default()
            .push(tool.original_name.as_str());
    }
    let lines: Vec<String> = by_server
        .iter()
        .map(|(slug, names)| {
            let shown = names
                .iter()
                .take(INDEX_NAMES_PER_SERVER)
                .copied()
                .collect::<Vec<_>>()
                .join("、");
            if names.len() > INDEX_NAMES_PER_SERVER {
                format!("- {slug}（共 {} 个）：{shown} 等", names.len())
            } else {
                format!("- {slug}：{shown}")
            }
        })
        .collect();
    Some(format!(
        "当前可检索的能力（Server：工具名）：\n{}",
        lines.join("\n")
    ))
}

/// auto 判定下改走平铺的可见工具数上限。
///
/// 网关是 `discovery → describe → execute` 三跳漏斗，每一跳都是一次模型放弃的
/// 机会；它换来的只有上下文成本，而成本 = O(工具数 × schema)。工具少时这笔
/// 交易不成立——ADR-020 只按 client_type 分流，漏了这一维。
/// 80 的来历要说清楚，免得后人以为它算出来过：初值 40 撞上了实测配置——
/// 一台真实机器的工作区正好 40 个下游工具（firecrawl 一家占 27），压线通过、
/// 再接一个 server 就掉回网关。**80 是「覆盖已知重度配置 + 留一倍余量」拍的，
/// 不是上下文成本在此翻转的测算结果**；客户端的硬上限也尚未实测。
/// 真要调，依据应当是「某客户端在 N 个工具时开始截断/报错」这类实测数字。
/// 仍不做成可配置项：想强制网关的用户走 per-connection 显式 gateway，
/// 不为一个内部启发式再铺一条设置管道。
const AUTO_FLAT_TOOL_LIMIT: usize = 80;

/// 单个下游 instructions 拼进能力总览时的截断长度（按字符）。
const SERVER_INSTRUCTIONS_MAX_CHARS: usize = 400;

/// 网关模式目录里单个工具描述的截断长度：目录要的是「一行认得出」，细节在
/// mcpane__tool_describe 里。
const CATALOG_TOOL_DESC_MAX_CHARS: usize = 100;

/// 目录条目的一行描述：把 [`CATALOG_TOOL_DESC_MAX_CHARS`] 的预算**优先分给
/// 触发提示**，不够了先截下游自述。
///
/// 真机验证（ADR-021 验证项 2）抓到的：拼好的描述整体从尾部截，而触发提示恰好
/// 在尾部——「关键词：搜索、查找、检索」被切成「关键词：搜索…」，而关键词正是
/// 触发匹配最靠的那部分。目录要的是「一行认得出、知道什么时候用」，用户写的
/// 触发条件比下游自述更贴这个用途，该保的是它。
fn catalog_desc_line(description: Option<&str>, suffix: Option<&str>) -> String {
    fn flatten(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }
    fn clip(text: &str, budget: usize) -> String {
        if text.chars().count() <= budget {
            return text.to_owned();
        }
        let mut out: String = text.chars().take(budget).collect();
        out.push('…');
        out
    }

    let suffix = suffix.map(flatten).filter(|s| !s.is_empty());
    let desc = description.map(flatten).filter(|s| !s.is_empty());
    match (desc, suffix) {
        (_, Some(suffix)) if suffix.chars().count() >= CATALOG_TOOL_DESC_MAX_CHARS => {
            // 提示自己就超预算：下游自述让位，只截提示。
            clip(&suffix, CATALOG_TOOL_DESC_MAX_CHARS)
        }
        (Some(desc), Some(suffix)) => {
            // 留一个字符给两段之间的空格。
            let budget = CATALOG_TOOL_DESC_MAX_CHARS - suffix.chars().count() - 1;
            format!("{} {suffix}", clip(&desc, budget))
        }
        (None, Some(suffix)) => suffix,
        (Some(desc), None) => clip(&desc, CATALOG_TOOL_DESC_MAX_CHARS),
        (None, None) => String::new(),
    }
}

/// auto 模式的形态判定。抽成自由函数是为了能直接测：判定要看三个输入的组合，
/// 而 `RequestContext` 在单测里造不出来。
///
/// - Claude Code 无条件平铺：实测自带 ToolSearch 延迟加载，工具再多也不怕；
/// - 其余客户端在发现开关开启时，仍要先问一句「装得下吗」——装得下就别让模型
///   走 `discovery → describe → execute` 三跳，每一跳都是一次放弃的机会；
/// - `visible_tools` 为 None（Revision 取不到）时保持发现开关的原判定：
///   回退无条件，不拿一个未知数去翻既有配置（同 ADR-019 决策 6）。
fn resolve_auto_exposure(
    client_type: Option<ClientType>,
    workspace_discovery: bool,
    visible_tools: Option<usize>,
) -> crate::state::ExposureMode {
    use crate::state::ExposureMode;
    if client_type == Some(ClientType::Claude) {
        return ExposureMode::Flat;
    }
    if !workspace_discovery {
        return ExposureMode::Flat;
    }
    match visible_tools {
        Some(count) if count <= AUTO_FLAT_TOOL_LIMIT => ExposureMode::Flat,
        _ => ExposureMode::Gateway,
    }
}

/// 会话开场的能力总览：Hub 自述 + 各下游自述摘要（网关模式再附全量工具目录
/// 与使用提示）。instructions 是客户端会注入模型上下文的字段——模型对
/// 「Hub 后面有什么」的第一印象来自这里。网关模式的目录直给是可达率的
/// 主通道之一（ADR-020）：模型对完整工具名单有视野才谈得上自己挑，
/// 不过 instructions 是否注入取决于 host，确定性通道是 tool_discovery 浏览模式。
fn compose_instructions(
    revision: &mcpane_hub_catalog::CatalogRevision,
    denied: impl Fn(&str) -> bool,
    gateway: bool,
    user_guidance: &str,
) -> String {
    let mut out = String::from("MCP Hub：Claude Code、Codex 与 MCP 工具的统一控制面。");
    let mut tool_counts: std::collections::BTreeMap<&str, usize> =
        std::collections::BTreeMap::new();
    for tool in &revision.tools {
        *tool_counts.entry(tool.server_id.as_str()).or_default() += 1;
    }
    let visible: Vec<_> = revision
        .servers
        .iter()
        .filter(|s| !denied(&s.server_id))
        .collect();
    // 后面要在拼接完成后判断「有没有可见 Server」，而 visible 到那时已被消耗。
    let visible_empty = visible.is_empty();
    if !visible.is_empty() {
        out.push_str("\n\n已接入的 MCP Server：");
        for server in visible {
            let count = tool_counts
                .get(server.server_id.as_str())
                .copied()
                .unwrap_or(0);
            out.push_str(&format!("\n- {}（{count} 个工具）", server.server_slug));
            // 下游自述压成单行摘要：多行原文会破坏列表结构，细节留给工具描述。
            let snippet = server
                .instructions
                .as_deref()
                .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
                .unwrap_or_default();
            if !snippet.is_empty() {
                let mut shown: String = snippet
                    .chars()
                    .take(SERVER_INSTRUCTIONS_MAX_CHARS)
                    .collect();
                if snippet.chars().count() > SERVER_INSTRUCTIONS_MAX_CHARS {
                    shown.push('…');
                }
                out.push('：');
                out.push_str(&shown);
            }
            // 网关模式附全量工具目录（名字 + 截断的一行描述，不含 schema）。
            if gateway {
                for tool in revision
                    .tools
                    .iter()
                    .filter(|t| t.server_id == server.server_id)
                {
                    out.push_str(&format!("\n  - {}", tool.exposed_name));
                    let line = catalog_desc_line(
                        tool.downstream_description(),
                        tool.description_suffix.as_deref(),
                    );
                    if !line.is_empty() {
                        out.push_str(&format!("：{line}"));
                    }
                }
            }
        }
        // 主动性教义（ADR-020 第三轴）：自报家门只解决「知道有」，不解决
        // 「想起来用」。两种形态都注入同一句优先准则——这是 MCP 给服务端
        // 影响模型行为的唯一正规通道，缺了它用户只能在每个客户端各配一遍。
        out.push_str(
            "\n\n使用准则：凡任务落在上述 Server 的能力范围内（以各自述与工具描述为准），\
             一律优先调用对应工具获取实时、准确的结果，不要凭记忆或训练数据作答；\
             不确定有没有合适的工具时，先查看工具列表/目录再下结论。\
             与宿主自带的同类工具能力重叠时，优先考虑这里的专用工具——它们是用户\
             为这个工作区特意接入并配好凭证的。",
        );
    }
    // 用户自定义准则原样注入（在内置教义之后）：精确到工具的路由规则
    // （「代码搜索一律先用 X」）只有用户自己知道，这是把各客户端的
    // CLAUDE.md/AGENTS.md 里那份 MCP 决策表下沉到 Hub 的入口。
    let user_guidance = user_guidance.trim();
    if !user_guidance.is_empty() {
        out.push_str("\n\n工作区使用准则（用户配置，须遵守）：\n");
        out.push_str(user_guidance);
    }
    if gateway {
        out.push_str(
            "\n\n本 Hub 处于工具网关模式：上面的目录就是全部可用工具，目录里没有 = \
             真的没有。用 mcpane__tool_describe(name) 取某工具的完整参数 schema\
             （它会同时把该工具加入 tools/list，支持列表刷新的客户端此后可直接调用），\
             用 mcpane__tool_execute 执行；mcpane__tool_discovery 可检索或重新浏览目录。\
             注意：你的宿主可能给这些工具名加了自己的前缀（如 mcp__<server>__），\
             按后缀匹配你可调用列表里的对应工具即可；传给 describe/execute 的 name \
             参数则始终用目录里的原始名。",
        );
    } else if !visible_empty && !user_guidance.is_empty() {
        // 平铺形态只在**用户确实写了准则时**才自报。这句话是给「准则与形态对不上」
        // 兜底的：准则是静态文本、形态却按客户端与工具数自适应（ADR-021 决策 1），
        // 一份为网关写的准则（「先 tool_discovery 检索」）碰上平铺，就是在指挥
        // 模型去调一个不存在的工具。
        // 没写准则的用户不存在这个问题，对他们这句纯属噪音——而 instructions
        // 每长一分，真正重要的那几句就被稀释一分。
        out.push_str(
            "\n\n补充：本 Hub 当前是平铺形态，上述工具已全部在你的工具列表里，\
             按名直接调用即可。若上面的使用准则提到检索、两步调用之类的流程，\
             那是网关形态的说法，此处不适用。",
        );
    }
    out
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct EchoParams {
    /// 需要原样返回的文本。
    pub message: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct EchoOutput {
    pub message: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct ToolDiscoveryParams {
    /// 检索关键词（匹配工具名、描述与所属 Server，支持前缀）。
    /// 省略或留空 = 浏览模式：返回全量工具目录（不含 schema）。
    pub query: Option<String>,
    /// 检索命中时最多返回条数（默认 10，上限 50）；浏览/兜底模式忽略。
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolDiscoveryMatch {
    /// 工具的暴露名——原样传给 mcpane__tool_describe / mcpane__tool_execute。
    pub name: String,
    pub description: Option<String>,
    /// 所属 Server（slug）。
    pub server: String,
    /// 目标工具的参数 JSON Schema。仅检索命中时内联；浏览/兜底模式省略
    /// （全量目录带 schema 就退化成平铺了），用 mcpane__tool_describe 精确取。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolDiscoveryOutput {
    pub matches: Vec<ToolDiscoveryMatch>,
    /// 本 Workspace 可用工具总数（供模型判断是否需要换关键词再搜）。
    pub total_tools: usize,
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct ToolDescribeParams {
    /// 工具暴露名（来自目录/检索结果，精确匹配）。
    pub name: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolDescribeOutput {
    pub name: String,
    pub description: Option<String>,
    /// 所属 Server（slug）。
    pub server: String,
    /// 完整参数 JSON Schema。
    pub input_schema: serde_json::Value,
    /// 输出 JSON Schema（如有）。类型用 Map 而非 Value：schemars 对裸 Value
    /// 生成布尔 schema `true`，规范合法但 Claude Code 的解析层吃不下，
    /// 一个工具带上它整张 tools/list 都会被丢弃（真机实测，ADR-020）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Map<String, serde_json::Value>>,
    /// 工具注解（如有）。Map 理由同上。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<serde_json::Map<String, serde_json::Value>>,
    /// 上面那份注解是谁说的：`downstream` = 下游 Server 申报，`user` = 用户
    /// 在本 Workspace 自己声明的（下游没申报时的补位）。无注解时不出现。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations_source: Option<String>,
    /// 本次 describe 是否把该工具升格进了 tools/list（网关模式的新条目为 true）。
    pub promoted: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct ToolExecuteParams {
    /// 工具暴露名（来自 mcpane__tool_discovery 的检索结果）。
    pub name: String,
    /// 传给目标工具的参数对象。
    pub arguments: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionInfo {
    pub client_connection_id: String,
    pub client_type: ClientType,
    pub request_id: String,
    /// 有状态模式为当前 `MCP-Session-Id`；无状态模式为 null。
    pub session_id: Option<String>,
    /// 本连接生效的 Workspace 与其已 pin 的 Catalog Revision。
    pub workspace_id: String,
    pub catalog_revision: String,
    pub protocol_version: String,
    pub server_version: String,
    /// 连接配置的暴露模式（flat/gateway/auto）。
    pub tool_exposure_mode: String,
    /// 本次请求实际生效的形态（flat/gateway，auto 已解析）——互操作自诊断用。
    pub effective_exposure: String,
    /// 本连接可见的工具数（扣掉被拒 Server）。auto 判定的输入之一，
    /// 光看 effective_exposure 说不出「为什么是 flat」。
    pub visible_tool_count: Option<usize>,
    /// auto 判定改走平铺的工具数上限（与上一字段配套读）。
    pub auto_flat_tool_limit: usize,
}

/// 本次请求是否协商到了 2026-07-28 及以上。按版本分叉的地方一律问它，
/// 别再各自写一遍比较——漏掉一处就是新协议字段漏进 legacy 响应。
fn is_modern_request(context: &RequestContext<RoleServer>) -> bool {
    context
        .protocol_version()
        .is_some_and(|v| v.as_str() >= ProtocolVersion::V_2026_07_28.as_str())
}

/// MRTR `requestState` 的密封器（SEP-2322 §1.4）。
///
/// 客户端会把 `requestState` **原样回传**，所以它是不可信输入：不密封就等于
/// 让客户端自己指定"这轮对话属于哪个下游"。密钥每进程随机生成——Hub 重启后
/// 旧 state 一律失效，客户端重试会拿到明确错误并从头开始，这比持久化一个
/// 长期密钥更安全。
fn request_state_codec() -> &'static rmcp::model::RequestStateCodec {
    static CODEC: OnceLock<rmcp::model::RequestStateCodec> = OnceLock::new();
    CODEC.get_or_init(|| {
        use rand::TryRngCore;
        let mut key = [0u8; rmcp::model::RequestStateCodec::MIN_KEY_LENGTH];
        rand::rngs::OsRng
            .try_fill_bytes(&mut key)
            .unwrap_or_else(|error| panic!("系统安全随机源不可用: {error}"));
        rmcp::model::RequestStateCodec::try_new(key)
            .unwrap_or_else(|error| panic!("requestState 密封密钥不满足最小长度: {error}"))
    })
}

/// MRTR 往返的存活时限。够人完成一次确认/授权交互，又不至于让一个悬而未决的
/// 往返无限期有效。
const REQUEST_STATE_TTL: std::time::Duration = std::time::Duration::from_secs(600);

/// 上游 `tools/call` 里与 MRTR 续轮有关的两个字段。首轮调用两者皆 None。
#[derive(Default)]
struct MrtrRetry {
    input_responses: Option<rmcp::model::InputResponses>,
    request_state: Option<String>,
}

impl MrtrRetry {
    fn from_params(params: &CallToolRequestParams) -> Self {
        Self {
            input_responses: params.input_responses.clone(),
            request_state: params.request_state.clone(),
        }
    }
}

/// 列表结果的缓存有效期（SEP-2549）。这是 Workspace 改绑生效的**兜底**时限：
/// 即时生效走 `subscriptions/listen` 的 listChanged 通知，TTL 只保证通知丢失
/// 时客户端最终也会重拉。
const LIST_CACHE_TTL_MS: u64 = 60_000;

/// `resources/read` 的缓存有效期：0 = 立即过期（别缓存）。资源内容属于下游，
/// 可能随时变化且不带任何变更通知，Hub 无法为它的新鲜度作保。
const READ_CACHE_TTL_MS: u64 = 0;

/// 给结果挂缓存契约（SEP-2549 对 tools/list、prompts/list、resources/list、
/// resources/templates/list、resources/read 五个方法**必填**），
/// **仅限协商到 2026-07-28 及以上的 peer**。
///
/// rmcp 会按 peer 版本清理 `resultType`，但 `ttlMs`/`cacheScope` 是我们自己
/// 填的，SDK 不管——不门控就会漏进 legacy 响应，改变旧客户端看到的 wire 形状。
/// `cacheScope` 取 private：内容按 Workspace/连接隔离，共享中间层不得缓存。
fn apply_cache_hints(
    context: &RequestContext<RoleServer>,
    ttl: u64,
    ttl_ms: &mut Option<u64>,
    cache_scope: &mut Option<rmcp::model::CacheScope>,
) {
    if is_modern_request(context) {
        *ttl_ms = Some(ttl);
        *cache_scope = Some(rmcp::model::CacheScope::Private);
    }
}

/// 本 Session 首次请求时 pin 下的身份：发起该请求的连接 ID + 由它解析出的
/// 生效 Workspace。两者一起存、一起校验（见 [`HubService::session_workspace`]）。
struct PinnedIdentity {
    connection_id: String,
    workspace_id: String,
}

/// Hub 上游 MCP 服务。有状态模式下 Session 工厂每次创建新实例，
/// 构造时 pin 当前 Catalog（Session-Pinned Workspace 语义）。
#[derive(Clone)]
pub struct HubService {
    state: Arc<AggregationState>,
    /// Session 创建时 pin 的全 Workspace Catalog 映射（每个 Revision 不可变）。
    pinned: Arc<crate::state::CatalogMap>,
    /// 本 Session 生效的身份 + Workspace，在**首个请求**解析后固定（ADR-004）。
    /// 用 `Arc<OnceLock>` 而非每请求实时查：否则管理员中途 `set-connection-workspace`
    /// 会改变活跃 Session 的路由目标——客户端手里的工具名还是旧 Workspace 的，
    /// 却被路由到新 Workspace 的 Revision，撞上 unknown tool 或同名工具静默串味。
    /// `Arc` 保证 rmcp 在 Session 内克隆本实例时各克隆共享同一次 pin。
    ///
    /// 只存 Workspace 不够：rmcp 的 `StreamableHttpService` 纯按
    /// `Mcp-Session-Id` 查找/复用已有 Session worker，不校验请求方身份
    /// （核实过 rmcp 源码：`transport/streamable_http_server/tower.rs`
    /// 的 `handle_post` 与 `session/local.rs` 的 `LocalSessionManager` 均无
    /// 此校验；3.1.4 再核，结论未变）。若只 pin Workspace 字符串，任何持有*别的*合法 Token、又拿到
    /// 了这个 Session-Id 的连接（Session-Id 会写进 `hub_request_logs`，同机
    /// 有文件读权限的进程都能捞到）就能借用这个 Session 已 pin 的 Workspace，
    /// 越过自己真正被分配的 Workspace 边界——这不是"中途改绑"那个已被
    /// ADR-004 覆盖的场景，是"从一开始就不是同一个连接"。故额外把 pin 时的
    /// connection_id 也存下来，此后每个请求都核对一致性，不一致直接拒绝。
    pinned_identity: Arc<OnceLock<PinnedIdentity>>,
}

#[tool_router]
impl HubService {
    pub fn new(state: Arc<AggregationState>) -> Self {
        let pinned = state.pinned_catalog_map();
        Self {
            state,
            pinned,
            pinned_identity: Arc::new(OnceLock::new()),
        }
    }

    /// 当前请求的客户端连接 ID（鉴权中间件注入；缺失时为空串）。
    fn connection_id_of(context: &RequestContext<RoleServer>) -> String {
        context
            .extensions
            .get::<http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<ClientIdentity>())
            .map(|identity| identity.client_connection_id.clone())
            .unwrap_or_default()
    }

    /// 当前请求的客户端类型（鉴权中间件注入；缺失时为 None）。
    fn client_type_of(context: &RequestContext<RoleServer>) -> Option<ClientType> {
        context
            .extensions
            .get::<http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<ClientIdentity>())
            .map(|identity| identity.client_type)
    }

    /// 本次请求生效的暴露形态（ADR-020）。连接显式配置直接生效；auto 下
    /// 只有 Claude Code（实测自带 ToolSearch 延迟加载）走平铺，其余客户端
    /// 沿用 Workspace 级发现开关——既有用户行为不变。
    fn effective_exposure(
        &self,
        context: &RequestContext<RoleServer>,
        workspace_id: &str,
    ) -> crate::state::ExposureMode {
        use crate::state::ExposureMode;
        match self.state.exposure_for(&Self::connection_id_of(context)) {
            ExposureMode::Flat => ExposureMode::Flat,
            ExposureMode::Gateway => ExposureMode::Gateway,
            ExposureMode::Auto => resolve_auto_exposure(
                Self::client_type_of(context),
                self.state.workspace_discovery(workspace_id),
                self.visible_tool_count(context),
            ),
        }
    }

    /// 本连接在当前 Revision 下能看到的工具数（扣掉被拒的 Server）。
    /// 未 pin 到 Revision（如尚未完成首次解析）时返回 None，判定方按原逻辑走。
    fn visible_tool_count(&self, context: &RequestContext<RoleServer>) -> Option<usize> {
        let revision = self.pinned_for(context).ok()?;
        let connection_id = Self::connection_id_of(context);
        Some(
            revision
                .tools
                .iter()
                .filter(|t| !self.state.is_server_denied(&connection_id, &t.server_id))
                .count(),
        )
    }

    /// 旧式 elicitation 桥接的登记：仅 legacy 上游可转——
    /// - 2026-07-28 上游禁止服务器脱离「正在处理的请求任务」发起 server→client
    ///   请求（SEP-2260），而转发发生在下游连接的任务里，过不了 rmcp 的关联检查；
    ///   新协议上游的确认走 MRTR 通道，本就不经这里；
    /// - 上游未声明 elicitation 能力时不转（规范禁止对未声明者发起），
    ///   保持下游侧默认拒绝。
    fn register_elicitation_bridge(
        &self,
        context: &RequestContext<RoleServer>,
        handle: &mcpane_hub_downstream::DownstreamHandle,
    ) -> Option<mcpane_hub_downstream::ElicitationRelayGuard> {
        let peer_info = context.peer.peer_info()?;
        if peer_info.capabilities.elicitation.is_none()
            || peer_info.protocol_version.as_str() >= ProtocolVersion::V_2026_07_28.as_str()
        {
            return None;
        }
        let peer = context.peer.clone();
        Some(handle.elicitation_relay().register(Arc::new(move |params| {
            let peer = peer.clone();
            Box::pin(async move {
                peer.create_elicitation(params).await.map_err(|error| {
                    ErrorData::internal_error(
                        format!("forwarding elicitation to upstream failed: {error}"),
                        None,
                    )
                })
            })
        })))
    }

    /// 把静态 instructions 换成按连接生成的能力总览。任何一步解析失败都保留
    /// 静态文案——initialize/discover 不应因总览生成失败而报错；未认证请求
    /// （无连接身份）不展开，避免向外泄露 Workspace 工具面貌。
    fn enrich_instructions(&self, info: &mut ServerInfo, context: &RequestContext<RoleServer>) {
        let connection_id = Self::connection_id_of(context);
        if connection_id.is_empty() {
            return;
        }
        let Ok(workspace_id) = self.session_workspace(context) else {
            return;
        };
        let Ok(revision) = self.pinned_for(context) else {
            return;
        };
        info.instructions = Some(compose_instructions(
            &revision,
            |server_id: &str| self.state.is_server_denied(&connection_id, server_id),
            self.effective_exposure(context, &workspace_id) == crate::state::ExposureMode::Gateway,
            &self.state.workspace_guidance(&workspace_id),
        ));
    }

    /// 本 Session 已 pin 的 Workspace：首次调用按当前连接映射解析并固定，
    /// 此后不再随控制面变更改变（ADR-004 Session-Pinned）。**同时**核对
    /// 当前请求的连接身份是否与 pin 时一致——不一致说明这次调用不是来自
    /// 建立本 Session 的那个连接（借用/拿到了别人的 Mcp-Session-Id），必须
    /// 拒绝而不是沿用 pin 下的 Workspace（否则等于让它越权看到/调用不属于
    /// 自己 Workspace 的 Server，见 struct 字段注释）。
    ///
    /// 以上只对 legacy 路径成立：2026-07-28 无 Session，rmcp 每请求新建 handler，
    /// 这个 `OnceLock` 每请求重置，等价于按 connection_id 实时解析（ADR-019）。
    fn session_workspace(&self, context: &RequestContext<RoleServer>) -> Result<String, ErrorData> {
        let connection_id = Self::connection_id_of(context);
        // 顺手记录该连接的协商协议版本：本方法是几乎所有请求的必经点。
        if let Some(version) = context.protocol_version() {
            self.state
                .note_connection_protocol(&connection_id, version.as_str());
        }
        let pinned = self.pinned_identity.get_or_init(|| PinnedIdentity {
            workspace_id: self.state.workspace_for(&connection_id),
            connection_id: connection_id.clone(),
        });
        if pinned.connection_id != connection_id {
            return Err(ErrorData::invalid_request(
                "session bound to a different client connection",
                None,
            ));
        }
        Ok(pinned.workspace_id.clone())
    }

    /// 解析本 Session 所属 Workspace 的已 pin Revision。
    fn pinned_for(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<Arc<mcpane_hub_catalog::CatalogRevision>, ErrorData> {
        let workspace = self.session_workspace(context)?;
        Ok(self
            .pinned
            .get(&workspace)
            .or_else(|| self.pinned.get(crate::state::DEFAULT_WORKSPACE))
            .cloned()
            .unwrap_or_else(|| {
                Arc::new(mcpane_hub_catalog::CatalogRevision::new(
                    "rev_empty".into(),
                    workspace,
                    Vec::new(),
                ))
            }))
    }

    // 完整 ToolAnnotations 是 MCP 最佳实践；同时规避 Codex exec 对无注解
    // 自定义工具误入 app-tool 审批路径导致的自动取消（openai/codex#16685）。
    #[tool(
        name = "mcpane__echo",
        description = "回显输入文本，用于连通性验证",
        annotations(
            title = "MCPane Echo",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn echo(&self, Parameters(EchoParams { message }): Parameters<EchoParams>) -> Json<EchoOutput> {
        Json(EchoOutput { message })
    }

    /// 网关模式 meta 工具：浏览/检索本 Workspace 可用工具。
    #[tool(
        name = "mcpane__tool_discovery",
        description = "浏览或检索当前可用的 MCP 工具。不传 query = 返回全量工具目录\
                       （名称+描述）；传 query 按关键词检索（匹配名称/描述/所属 Server，\
                       支持前缀），命中时结果内联 inputSchema。检索无命中时自动降级为\
                       全量目录，永不返回空——目录里没有才是真的没有。返回的 name 传给 \
                       mcpane__tool_describe 取完整 schema，或直接传给 mcpane__tool_execute 执行。",
        annotations(
            title = "MCPane Tool Discovery",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn tool_discovery(
        &self,
        Parameters(ToolDiscoveryParams { query, limit }): Parameters<ToolDiscoveryParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ToolDiscoveryOutput>, ErrorData> {
        let revision = self.pinned_for(&context)?;
        let connection_id = Self::connection_id_of(&context);
        let visible: Vec<&mcpane_hub_catalog::CatalogTool> = revision
            .tools
            .iter()
            .filter(|t| !self.state.is_server_denied(&connection_id, &t.server_id))
            .collect();
        let limit = limit.unwrap_or(10).min(50) as usize;
        let query = query.unwrap_or_default();
        let hits = if query.trim().is_empty() {
            Vec::new()
        } else {
            mcpane_hub_catalog::search::search_tools(&visible, &query, limit)
        };
        // 无命中（含浏览模式）兜底全量目录：检索是概率性的，空结果会让模型
        // 误判「没这个工具」直接放弃——那是最危险的输出（ADR-020）。
        // 目录不带 schema：全量带 schema 就退化成平铺，目录经济性归零。
        let matches = if hits.is_empty() {
            let mut all: Vec<&mcpane_hub_catalog::CatalogTool> = visible.clone();
            all.sort_by(|a, b| a.exposed_name.cmp(&b.exposed_name));
            all.into_iter()
                .map(|tool| ToolDiscoveryMatch {
                    name: tool.exposed_name.clone(),
                    description: tool.description.clone(),
                    server: tool.server_slug.clone(),
                    input_schema: None,
                })
                .collect()
        } else {
            hits.into_iter()
                .map(|tool| ToolDiscoveryMatch {
                    name: tool.exposed_name.clone(),
                    description: tool.description.clone(),
                    server: tool.server_slug.clone(),
                    input_schema: Some(tool.input_schema.clone()),
                })
                .collect()
        };
        Ok(Json(ToolDiscoveryOutput {
            matches,
            total_tools: visible.len(),
        }))
    }

    /// 网关模式 meta 工具：按暴露名精确取完整 schema，并把该工具升格进 tools/list。
    #[tool(
        name = "mcpane__tool_describe",
        description = "按工具暴露名（来自 mcpane__tool_discovery 的目录/检索结果）\
                       精确返回完整 inputSchema 等定义，并把该工具加入 tools/list——\
                       支持工具列表刷新的客户端此后可直接原生调用它；也可以继续用 \
                       mcpane__tool_execute 执行。",
        annotations(
            title = "MCPane Tool Describe",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn tool_describe(
        &self,
        Parameters(ToolDescribeParams { name }): Parameters<ToolDescribeParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ToolDescribeOutput>, ErrorData> {
        let revision = self.pinned_for(&context)?;
        let workspace_id = self.session_workspace(&context)?;
        let connection_id = Self::connection_id_of(&context);
        let tool = revision
            .route(&name)
            .filter(|t| !self.state.is_server_denied(&connection_id, &t.server_id));
        let Some(tool) = tool else {
            // 打错名字不能是死胡同：附上最近似候选，模型可自愈重试。
            let visible: Vec<&mcpane_hub_catalog::CatalogTool> = revision
                .tools
                .iter()
                .filter(|t| !self.state.is_server_denied(&connection_id, &t.server_id))
                .collect();
            let candidates = mcpane_hub_catalog::search::search_tools(&visible, &name, 5)
                .into_iter()
                .map(|t| t.exposed_name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let hint = if candidates.is_empty() {
                "调用 mcpane__tool_discovery（不传 query）查看全量目录".to_owned()
            } else {
                format!("最相近的候选：{candidates}")
            };
            return Err(ErrorData::invalid_params(
                format!("unknown tool: {name}；{hint}"),
                None,
            ));
        };
        // 升格只在网关形态有意义：平铺形态下工具本就全量在 tools/list 里。
        let promoted = self.effective_exposure(&context, &workspace_id)
            == crate::state::ExposureMode::Gateway
            && self.state.promote_tool(&connection_id, &tool.exposed_name);
        let effective_annotations = tool.effective_annotations();
        Ok(Json(ToolDescribeOutput {
            name: tool.exposed_name.clone(),
            description: tool.description.clone(),
            server: tool.server_slug.clone(),
            input_schema: tool.input_schema.clone(),
            output_schema: tool
                .output_schema
                .as_ref()
                .and_then(|v| v.as_object().cloned()),
            annotations: effective_annotations
                .as_ref()
                .and_then(|(v, _)| v.as_object().cloned()),
            annotations_source: effective_annotations.as_ref().map(|(_, source)| {
                match source {
                    mcpane_hub_catalog::AnnotationSource::Downstream => "downstream",
                    mcpane_hub_catalog::AnnotationSource::User => "user",
                }
                .to_owned()
            }),
            promoted,
        }))
    }

    /// 发现模式 meta 工具：按暴露名执行任意可用工具（路由/权限与直接调用同规）。
    #[tool(
        name = "mcpane__tool_execute",
        description = "执行 mcpane__tool_discovery 检索到的工具：name 传其返回的 \
                       工具名，arguments 传目标工具的参数对象。",
        annotations(
            title = "MCPane Tool Execute",
            // 代理任意下游工具，语义未知——按最保守的注解申报。
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn tool_execute(
        &self,
        Parameters(ToolExecuteParams { name, arguments }): Parameters<ToolExecuteParams>,
        rmcp::handler::server::tool::RequestState(request_state): rmcp::handler::server::tool::RequestState,
        rmcp::handler::server::tool::InputResponses(input_responses): rmcp::handler::server::tool::InputResponses,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        // 不允许经 execute 套娃调用内建工具：内建始终在 tools/list 里直接可见。
        if name.starts_with(TOOL_PREFIX) {
            let message = format!("built-in tools must be called directly: {name}");
            // 这条早退不经 call_downstream_tool，是 execute 路径上唯一没有内层
            // 记录的分支——外层信封已不入日志，这里不记就整个调用查无此事。
            let request_json = arguments
                .as_ref()
                .map(|map| serde_json::Value::Object(map.clone()));
            let workspace_id = self.session_workspace(&context).unwrap_or_default();
            self.log_reject(
                &context,
                "tools/call",
                None,
                &name,
                &workspace_id,
                "unknown",
                &message,
                request_json.as_ref(),
            )
            .await;
            return Err(ErrorData::invalid_params(message, None));
        }
        // 发现模式同样要能走完多轮交互：续轮字段挂在外层 tool_execute 的请求上，
        // 用 rmcp 的提取器取出来原样转交（发现模式恰恰用在工具多的场景，
        // 需要确认的工具在这里更常见，不是可以放弃的边角）。
        self.call_downstream_tool(
            &context,
            &name,
            arguments,
            MrtrRetry {
                input_responses,
                request_state,
            },
            true,
        )
        .await
    }

    #[tool(
        name = "mcpane__connection_info",
        description = "返回当前客户端连接与协议信息，用于互操作验证",
        annotations(
            title = "MCPane Connection Info",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn connection_info(
        &self,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ConnectionInfo>, ErrorData> {
        let parts = context
            .extensions
            .get::<http::request::Parts>()
            .ok_or_else(|| ErrorData::internal_error("http request parts unavailable", None))?;
        let identity = parts
            .extensions
            .get::<ClientIdentity>()
            .ok_or_else(|| ErrorData::internal_error("client identity unavailable", None))?;
        let request_id = parts
            .extensions
            .get::<RequestId>()
            .map(|r| r.0.clone())
            .unwrap_or_else(|| "req_unknown".to_owned());
        let session_id = parts
            .headers
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);

        // 回显本 Session 已 pin 的 Workspace（与路由/校验同源），而非实时映射。
        let workspace_id = self.session_workspace(&context)?;
        let revision = self
            .pinned
            .get(&workspace_id)
            .map(|r| r.id.clone())
            .unwrap_or_else(|| "rev_empty".to_owned());
        // 报告**本次请求实际协商**的版本，不是编译期常量——双栈下同一个 Hub
        // 会同时服务多个协议版本，这个工具存在的意义就是互操作验证，回显错误
        // 版本比不回显更糟。
        let protocol_version = context
            .protocol_version()
            .map(|v| v.as_str().to_owned())
            .unwrap_or_else(|| MCP_PROTOCOL_VERSION.to_owned());
        let tool_exposure_mode = self
            .state
            .exposure_for(&identity.client_connection_id)
            .as_str()
            .to_owned();
        let effective_exposure = self
            .effective_exposure(&context, &workspace_id)
            .as_str()
            .to_owned();
        Ok(Json(ConnectionInfo {
            client_connection_id: identity.client_connection_id.clone(),
            client_type: identity.client_type,
            request_id,
            session_id,
            workspace_id,
            catalog_revision: revision,
            protocol_version,
            server_version: SERVER_VERSION.to_owned(),
            tool_exposure_mode,
            effective_exposure,
            visible_tool_count: self.visible_tool_count(&context),
            auto_flat_tool_limit: AUTO_FLAT_TOOL_LIMIT,
        }))
    }
}

/// 本地轻量参数校验：只查 `required` 成员缺失与顶层 `properties` 的类型不符。
/// **明确不管**：嵌套对象内部、anyOf/oneOf/allOf、$ref、enum、format、
/// additionalProperties——结果只用作自愈提示、从不拦截调用，漏报无害而
/// 误报有害，所以只做最不会错的两类检查。
fn validate_arguments(
    schema: &serde_json::Value,
    arguments: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Vec<String> {
    let empty = serde_json::Map::new();
    let args = arguments.unwrap_or(&empty);
    let mut findings = Vec::new();
    if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
        for field in required.iter().filter_map(|f| f.as_str()) {
            if !args.contains_key(field) {
                findings.push(format!("缺少必填参数 {field}"));
            }
        }
    }
    if let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) {
        for (key, value) in args {
            let Some(expected) = properties
                .get(key)
                .and_then(|p| p.get("type"))
                .and_then(|t| t.as_str())
            else {
                continue; // 未声明类型（含 type 为数组的多类型声明）一律放过。
            };
            let ok = match expected {
                "string" => value.is_string(),
                "number" => value.is_number(),
                "integer" => value.is_i64() || value.is_u64(),
                "boolean" => value.is_boolean(),
                "array" => value.is_array(),
                "object" => value.is_object(),
                _ => true,
            };
            if !ok {
                findings.push(format!("参数 {key} 应为 {expected} 类型"));
            }
        }
    }
    findings
}

/// Catalog 条目 → rmcp Tool（暴露给上游客户端）。
fn catalog_tool_to_rmcp(entry: &mcpane_hub_catalog::CatalogTool) -> Tool {
    let input_schema: rmcp::model::JsonObject =
        entry.input_schema.as_object().cloned().unwrap_or_default();
    let mut tool = Tool::new(
        entry.exposed_name.clone(),
        entry.description.clone().unwrap_or_default(),
        Arc::new(input_schema),
    );
    if let Some(output) = &entry.output_schema
        && let Some(object) = output.as_object()
    {
        tool.output_schema = Some(Arc::new(object.clone()));
    }
    // 下游注解原样透传；下游缺席时才用用户在本 Workspace 声明的那份补空位，
    // 两者都没有就不发——Hub 自己永远不编（ADR-002 Gate 与其例外口）。
    if let Some((annotations, _source)) = entry.effective_annotations() {
        tool.annotations = serde_json::from_value(annotations).ok();
    }
    if let Some(icons) = &entry.icons {
        tool.icons = serde_json::from_value(icons.clone()).ok();
    }
    tool
}

impl ServerHandler for HubService {
    fn get_info(&self) -> ServerInfo {
        let mut server_info = Implementation::from_build_env();
        server_info.name = "mcpane-hub".to_owned();
        server_info.version = SERVER_VERSION.to_owned();

        // listChanged 只对新协议成立：它靠 subscriptions/listen 推送，而该方法
        // 是 2026-07-28 才有的。legacy 路径 Hub 从不主动推 list_changed，所以
        // 由 `initialize` 覆写把这项裁掉——宣告了却不发是更坏的事。
        // 三类一起宣告：Catalog 重建同时改变 tools/prompts/resources 三张列表，
        // 三类治理的行为必须一致（与 get_prompt 禁用校验同一条原则）。
        let mut info = ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .enable_prompts()
                .enable_prompts_list_changed()
                .enable_resources()
                .enable_resources_list_changed()
                // completions 按引用路由到下游；下游不支持时回空建议（见 complete）。
                .enable_completions()
                .build(),
        );
        info.protocol_version = ProtocolVersion::V_2025_11_25;
        info.server_info = server_info;
        info.instructions =
            Some("MCP Hub：Claude Code、Codex 与 MCP 工具的统一控制面。".to_owned());
        info
    }

    /// 覆写只为一件事：把 `listChanged` 从 legacy 的 capabilities 里裁掉。
    /// 该能力靠 `subscriptions/listen` 兑现，而那是 2026-07-28 才有的方法；
    /// legacy 客户端拿到这项声明只会空等一个永远不来的通知。
    /// 其余（回填 peer_info、版本协商）照抄 rmcp 默认实现（
    /// `negotiate_protocol_version` 仍是 pub(crate)，只能复制）。3.1.4 再核：
    /// 仍是「在 supported 集合内用请求版本，否则回退 get_info()」。
    async fn initialize(
        &self,
        request: rmcp::model::InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::InitializeResult, ErrorData> {
        context.peer.set_peer_info(request.clone());
        let mut info = self.get_info();
        let supported = self.supported_protocol_versions();
        if supported.contains(&request.protocol_version) {
            info.protocol_version = request.protocol_version;
        }
        if info.protocol_version.as_str() < ProtocolVersion::V_2026_07_28.as_str() {
            if let Some(tools) = info.capabilities.tools.as_mut() {
                tools.list_changed = None;
            }
            if let Some(prompts) = info.capabilities.prompts.as_mut() {
                prompts.list_changed = None;
            }
            if let Some(resources) = info.capabilities.resources.as_mut() {
                resources.list_changed = None;
            }
        }
        self.enrich_instructions(&mut info, &context);
        Ok(info)
    }

    /// 无状态发现（2026-07-28）：与 initialize 同样把 instructions 换成
    /// 按连接生成的能力总览——新协议客户端不走 initialize，只有这里能看到。
    async fn discover(
        &self,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::DiscoverResult, ErrorData> {
        let mut info = self.get_info();
        self.enrich_instructions(&mut info, &context);
        Ok(rmcp::model::DiscoverResult::from_server_info(
            self.supported_protocol_versions().into_owned(),
            info,
        ))
    }

    /// completion/complete：按引用类型路由到对应下游（暴露名改写回原名），
    /// 权限校验与 prompts/resources 读取同规；未声明 completions 能力的
    /// 下游不打扰，直接回空建议（规范允许，客户端按无建议处理）。
    async fn complete(
        &self,
        mut request: rmcp::model::CompleteRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::CompleteResult, ErrorData> {
        let revision = self.pinned_for(&context)?;
        let connection_id = Self::connection_id_of(&context);
        // 实时禁用校验与 prompts/get、resources/read 同规：pin 之后被禁用的
        // 项不再提供补全——三类治理行为必须一致，补全不能成为治理的旁路。
        let workspace_id = self.session_workspace(&context)?;
        let server_id = match &mut request.r#ref {
            rmcp::model::Reference::Prompt(prompt) => {
                let Some(route) = revision.route_prompt(&prompt.name) else {
                    return Err(ErrorData::invalid_params(
                        format!("unknown prompt: {}", prompt.name),
                        None,
                    ));
                };
                if self.state.is_prompt_disabled(
                    &workspace_id,
                    &route.server_id,
                    &route.original_name,
                ) {
                    return Err(ErrorData::invalid_params(
                        format!("prompt disabled by workspace policy: {}", prompt.name),
                        None,
                    ));
                }
                prompt.name = route.original_name.clone();
                route.server_id.clone()
            }
            rmcp::model::Reference::Resource(template) => {
                let Some(route) = revision.route_resource_template(&template.uri) else {
                    return Err(ErrorData::invalid_params(
                        format!("unknown resource template: {}", template.uri),
                        None,
                    ));
                };
                if self
                    .state
                    .is_resource_disabled(&workspace_id, &route.server_id, &template.uri)
                {
                    return Err(ErrorData::invalid_params(
                        format!("resource disabled by workspace policy: {}", template.uri),
                        None,
                    ));
                }
                route.server_id.clone()
            }
            // Reference 是 non_exhaustive：未来新增引用类型时宁可明确报错。
            other => {
                return Err(ErrorData::invalid_params(
                    format!("unsupported completion reference: {other:?}"),
                    None,
                ));
            }
        };
        if self.state.is_server_denied(&connection_id, &server_id) {
            return Err(ErrorData::invalid_params(
                "server disabled by client access control".to_owned(),
                None,
            ));
        }
        let Some(handle) = self.state.downstream(&server_id) else {
            return Err(ErrorData::internal_error("downstream unavailable", None));
        };
        if handle
            .capabilities()
            .unwrap_or_default()
            .completions
            .is_none()
        {
            return Ok(rmcp::model::CompleteResult::default());
        }
        handle
            .complete(request)
            .await
            .map_err(downstream_error_data)
    }

    /// 只接受工具列表变更；资源订阅（`resourceSubscriptions`）P0 不支持——
    /// `supported_by` 会按 capabilities 把未宣告的类别一并滤掉。
    fn accepted_subscription_filter(
        &self,
        requested: &rmcp::model::SubscriptionFilter,
    ) -> Option<rmcp::model::SubscriptionFilter> {
        Some(requested.supported_by(&self.get_info().capabilities))
    }

    /// 长驻订阅：把会改变本连接可见列表的事件（Catalog 重建、Workspace 改绑、
    /// 访问控制启停）按客户端接受的类别推成 `*/list_changed`，直到请求被取消。
    async fn listen(&self, context: SubscriptionContext) -> Result<(), ErrorData> {
        let accepted = context.accepted().clone();
        if accepted.tools_list_changed != Some(true)
            && accepted.prompts_list_changed != Some(true)
            && accepted.resources_list_changed != Some(true)
        {
            context.cancelled().await;
            return Ok(());
        }
        use crate::state::CatalogChange;
        use tokio::sync::broadcast::error::RecvError;

        let connection_id = Self::connection_id_of(context.request_context());
        // 只作初值：订阅是长驻的，期间这条连接可能被改绑到别的 Workspace，
        // 所以下面收到连接级事件时都要重算，不能一直用这一份。
        let mut workspace_id = self.session_workspace(context.request_context())?;
        let mut changes = self.state.subscribe_catalog_changes();
        loop {
            tokio::select! {
                () = context.cancelled() => return Ok(()),
                received = changes.recv() => {
                    let should_notify = match received {
                        // 别的 Workspace 重建与本连接无关。
                        Ok(CatalogChange::Rebuilt(changed)) => changed == workspace_id,
                        // 连接级事件（改绑/访问控制）：只关心自己在不在受影响集合里。
                        // 改绑会换 Workspace，重算以便后续 Rebuilt 过滤仍然正确。
                        Ok(CatalogChange::ConnectionsChanged(ids)) => {
                            let mine = ids.contains(&connection_id);
                            if mine {
                                workspace_id = self.state.workspace_for(&connection_id);
                            }
                            mine
                        }
                        // Lagged：漏了若干事件，一律当作"变了"推一次，宁多勿漏。
                        Err(RecvError::Lagged(_)) => {
                            workspace_id = self.state.workspace_for(&connection_id);
                            true
                        }
                        Err(RecvError::Closed) => return Ok(()),
                    };
                    // 三类列表都可能被同一事件改变，按客户端接受的类别逐个推。
                    if should_notify {
                        if accepted.tools_list_changed == Some(true)
                            && context.sink().notify_tool_list_changed().await.is_err()
                        {
                            return Ok(());
                        }
                        if accepted.prompts_list_changed == Some(true)
                            && context.sink().notify_prompt_list_changed().await.is_err()
                        {
                            return Ok(());
                        }
                        if accepted.resources_list_changed == Some(true)
                            && context.sink().notify_resource_list_changed().await.is_err()
                        {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let revision = self.pinned_for(&_context)?;
        let workspace_id = self.session_workspace(&_context)?;
        let mut tools = Self::tool_router().list_all();
        if self.effective_exposure(&_context, &workspace_id) == crate::state::ExposureMode::Gateway
        {
            // 网关模式：暴露内建工具（含三个 meta 工具）+ 本连接已升格的工具。
            // 下游工具经 mcpane__tool_discovery 浏览/检索、mcpane__tool_describe
            // 取 schema 并升格、mcpane__tool_execute 执行——避免撑爆模型上下文。
            let connection_id = Self::connection_id_of(&_context);
            let visible: Vec<&mcpane_hub_catalog::CatalogTool> = revision
                .tools
                .iter()
                .filter(|t| !self.state.is_server_denied(&connection_id, &t.server_id))
                .collect();
            if let Some(discovery) = tools.iter_mut().find(|t| t.name == TOOL_DISCOVERY_NAME)
                && let Some(index) = capability_index(&visible)
            {
                let base = discovery.description.as_deref().unwrap_or_default();
                discovery.description = Some(format!("{base}\n\n{index}").into());
            }
            // 升格工具以真实 schema 追加：catalog 重建后失效的名字在这里被
            // 惰性过滤掉（visible 只含当前 Revision 的工具），无需主动清理。
            let promoted = self.state.promoted_snapshot(&connection_id);
            if !promoted.is_empty() {
                tools.extend(
                    visible
                        .iter()
                        .filter(|t| promoted.contains(&t.exposed_name))
                        .map(|t| catalog_tool_to_rmcp(t)),
                );
            }
            let mut result = ListToolsResult {
                tools,
                ..Default::default()
            };
            apply_cache_hints(
                &_context,
                LIST_CACHE_TTL_MS,
                &mut result.ttl_ms,
                &mut result.cache_scope,
            );
            return Ok(result);
        }
        // 平铺模式：meta 工具没有存在意义，藏掉免得模型绕远路。
        tools.retain(|t| {
            t.name != TOOL_DISCOVERY_NAME
                && t.name != TOOL_EXECUTE_NAME
                && t.name != TOOL_DESCRIBE_NAME
        });
        // 访问控制：被拒绝 Server 的工具不进列表；内建 mcpane__* 不受影响（诊断保底）。
        let connection_id = Self::connection_id_of(&_context);
        tools.extend(
            revision
                .tools
                .iter()
                .filter(|t| !self.state.is_server_denied(&connection_id, &t.server_id))
                .map(catalog_tool_to_rmcp),
        );
        let mut result = ListToolsResult {
            tools,
            ..Default::default()
        };
        apply_cache_hints(
            &_context,
            LIST_CACHE_TTL_MS,
            &mut result.ttl_ms,
            &mut result.cache_scope,
        );
        Ok(result)
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let name = params.name.to_string();
        let router = Self::tool_router();

        // 1. 内建 Tool。
        if router.has_route(&name) {
            let request_json = params
                .arguments
                .as_ref()
                .map(|map| serde_json::Value::Object(map.clone()));
            let workspace_id = self.session_workspace(&context).ok();
            let log_ctx = context.clone();
            let started = std::time::Instant::now();
            let result = router
                .call(ToolCallContext::new(self, params, context))
                .await;
            let duration_ms = started.elapsed().as_millis() as i64;
            let mut tool_err = None;
            let response_json;
            let outcome = match &result {
                Ok(response) => {
                    response_json = Some(tool_response_json(response));
                    complete_tool_outcome(response, &mut tool_err)
                }
                Err(error) => {
                    response_json = None;
                    LogOutcome::Failed {
                        category: "call",
                        message: error.message.as_ref(),
                    }
                }
            };
            // mcpane__tool_execute 只是个信封：它把活转交给 call_downstream_tool，
            // 那边已经按**真实工具名**记了一条（连路由失败、策略拒绝都记）。这里
            // 再记一条，同一次调用就在日志里出现两遍——时刻相同、耗时只差一两毫秒
            // （差的就是网关自己的路由开销），列表里看着像重复记录，SQL 侧的请求数
            // 与耗时分布也跟着翻倍。网关模式下每一次调用都会这样。
            // 唯一没有内层记录的是 execute 自己那道前置校验，它在 tool_execute 里
            // 单独记（见 TOOL_PREFIX 分支）。
            if name != TOOL_EXECUTE_NAME {
                self.log_call(
                    &log_ctx,
                    CallLog {
                        method: "tools/call",
                        server_id: None,
                        exposed_name: &name,
                        workspace_id: workspace_id.as_deref(),
                        duration_ms,
                        retry_count: 0,
                        request: request_json.as_ref(),
                        response: response_json.as_ref(),
                        outcome,
                    },
                )
                .await;
            }
            return result;
        }

        // 2. 聚合路由（与网关模式的 mcpane__tool_execute 共用同一套校验链）。
        let mrtr = MrtrRetry::from_params(&params);
        self.call_downstream_tool(&context, &name, params.arguments.clone(), mrtr, false)
            .await
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        let revision = self.pinned_for(&context)?;
        let connection_id = Self::connection_id_of(&context);
        let prompts = revision
            .prompts
            .iter()
            .filter(|p| !self.state.is_server_denied(&connection_id, &p.server_id))
            .filter_map(|p| {
                let mut prompt: Prompt = serde_json::from_value(p.raw.clone()).ok()?;
                prompt.name = p.exposed_name.clone();
                Some(prompt)
            })
            .collect();
        let mut result = ListPromptsResult {
            prompts,
            ..Default::default()
        };
        apply_cache_hints(
            &context,
            LIST_CACHE_TTL_MS,
            &mut result.ttl_ms,
            &mut result.cache_scope,
        );
        Ok(result)
    }

    async fn get_prompt(
        &self,
        params: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        let revision = self.pinned_for(&context)?;
        let name = params.name.clone();
        let request = serde_json::json!({
            "name": name,
            "arguments": params.arguments,
        });
        let Some(route) = revision.route_prompt(&name) else {
            let message = format!("unknown prompt: {name}");
            self.log_reject(
                &context,
                "prompts/get",
                None,
                &name,
                &revision.workspace_id,
                "unknown",
                &message,
                Some(&request),
            )
            .await;
            return Err(ErrorData::invalid_params(message, None));
        };
        // 二次权限校验：pin 之后被禁用的 Prompt 立即拒绝（与 call_tool 同规）。
        // 少了这一步，「在界面上禁用某个 Prompt」对已连接的客户端毫无作用——
        // 它只是从 prompts/list 消失，按名字照样读得到内容，而同样的操作对
        // Tool 是立刻生效的。三类治理的行为必须一致。
        let workspace_id = self.session_workspace(&context)?;
        if self
            .state
            .is_prompt_disabled(&workspace_id, &route.server_id, &route.original_name)
        {
            let message = format!("prompt disabled by workspace policy: {name}");
            self.log_reject(
                &context,
                "prompts/get",
                Some(&route.server_id),
                &route.exposed_name,
                &revision.workspace_id,
                "policy",
                &message,
                Some(&request),
            )
            .await;
            return Err(ErrorData::invalid_params(message, None));
        }
        // 访问控制与 call_tool 同规：实时查询，撤销对活跃会话即时生效。
        let connection_id = Self::connection_id_of(&context);
        if self
            .state
            .is_server_denied(&connection_id, &route.server_id)
        {
            let message = format!("server disabled by client access control: {name}");
            self.log_reject(
                &context,
                "prompts/get",
                Some(&route.server_id),
                &route.exposed_name,
                &revision.workspace_id,
                "policy",
                &message,
                Some(&request),
            )
            .await;
            return Err(ErrorData::invalid_params(message, None));
        }
        let Some(handle) = self.state.downstream(&route.server_id) else {
            let message = format!("downstream unavailable: {}", route.server_slug);
            self.log_reject(
                &context,
                "prompts/get",
                Some(&route.server_id),
                &route.exposed_name,
                &revision.workspace_id,
                "unavailable",
                &message,
                Some(&request),
            )
            .await;
            return Err(ErrorData::internal_error(message, None));
        };

        let started = std::time::Instant::now();
        let result = handle
            .get_prompt(&route.original_name, params.arguments.clone())
            .await;
        let duration_ms = started.elapsed().as_millis() as i64;
        let response = result
            .as_ref()
            .ok()
            .and_then(|value| serde_json::to_value(value).ok());
        self.log_call(
            &context,
            CallLog {
                method: "prompts/get",
                server_id: Some(&route.server_id),
                exposed_name: &route.exposed_name,
                workspace_id: Some(&revision.workspace_id),
                duration_ms,
                retry_count: 0,
                request: Some(&request),
                response: response.as_ref(),
                outcome: match result.as_ref() {
                    Ok(_) => LogOutcome::Ok,
                    Err(error) => LogOutcome::Downstream(error),
                },
            },
        )
        .await;
        result.map_err(downstream_error_data).map(Into::into)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let revision = self.pinned_for(&context)?;
        let connection_id = Self::connection_id_of(&context);
        let resources = revision
            .resources
            .iter()
            .filter(|r| !self.state.is_server_denied(&connection_id, &r.server_id))
            .filter_map(|r| serde_json::from_value::<Resource>(r.raw.clone()).ok())
            .collect();
        let mut result = ListResourcesResult {
            resources,
            ..Default::default()
        };
        apply_cache_hints(
            &context,
            LIST_CACHE_TTL_MS,
            &mut result.ttl_ms,
            &mut result.cache_scope,
        );
        Ok(result)
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        let revision = self.pinned_for(&context)?;
        let connection_id = Self::connection_id_of(&context);
        let resource_templates = revision
            .resource_templates
            .iter()
            .filter(|t| !self.state.is_server_denied(&connection_id, &t.server_id))
            .filter_map(|t| serde_json::from_value::<ResourceTemplate>(t.raw.clone()).ok())
            .collect();
        let mut result = ListResourceTemplatesResult {
            resource_templates,
            ..Default::default()
        };
        apply_cache_hints(
            &context,
            LIST_CACHE_TTL_MS,
            &mut result.ttl_ms,
            &mut result.cache_scope,
        );
        Ok(result)
    }

    async fn read_resource(
        &self,
        params: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let revision = self.pinned_for(&context)?;
        let uri = params.uri;
        let request = serde_json::json!({ "uri": uri });
        // 精确命中静态条目优先；未命中再按 template 字面前缀路由（确定性，
        // 不做「挨个下游试调」）。
        let (server_id, server_slug) = match revision.route_resource(&uri) {
            Some(entry) => (entry.server_id.clone(), entry.server_slug.clone()),
            // 精确未命中：先看是否被治理显式禁用——禁用的 resource 不得经
            // 同源 template 前缀兜底放行（否则 per-resource 禁用被架空）。
            None if revision.is_resource_denied(&uri) => {
                let message = format!("resource disabled by workspace policy: {uri}");
                self.log_reject(
                    &context,
                    "resources/read",
                    None,
                    &uri,
                    &revision.workspace_id,
                    "policy",
                    &message,
                    Some(&request),
                )
                .await;
                return Err(ErrorData::invalid_params(message, None));
            }
            None => match revision.route_resource_template(&uri) {
                Some(t) => (t.server_id.clone(), t.server_slug.clone()),
                None => {
                    let message = format!("unknown resource: {uri}");
                    self.log_reject(
                        &context,
                        "resources/read",
                        None,
                        &uri,
                        &revision.workspace_id,
                        "unknown",
                        &message,
                        Some(&request),
                    )
                    .await;
                    return Err(ErrorData::invalid_params(message, None));
                }
            },
        };
        // 二次权限校验：pin 之后被禁用的 Resource 立即拒绝（与 call_tool 同规）。
        // 上面那条 `revision.is_resource_denied` 查的是**已 pin 的快照**，只挡得住
        // 「pin 时就已禁用」的情况；会话建立之后才禁用的，快照里它还在，精确命中
        // 直接放行。同一动作对 Tool 是即时生效的，这里必须查实时状态对齐。
        let workspace_id = self.session_workspace(&context)?;
        if self
            .state
            .is_resource_disabled(&workspace_id, &server_id, &uri)
        {
            let message = format!("resource disabled by workspace policy: {uri}");
            self.log_reject(
                &context,
                "resources/read",
                Some(&server_id),
                &uri,
                &revision.workspace_id,
                "policy",
                &message,
                Some(&request),
            )
            .await;
            return Err(ErrorData::invalid_params(message, None));
        }
        let connection_id = Self::connection_id_of(&context);
        if self.state.is_server_denied(&connection_id, &server_id) {
            let message = format!("server disabled by client access control: {uri}");
            self.log_reject(
                &context,
                "resources/read",
                Some(&server_id),
                &uri,
                &revision.workspace_id,
                "policy",
                &message,
                Some(&request),
            )
            .await;
            return Err(ErrorData::invalid_params(message, None));
        }
        let Some(handle) = self.state.downstream(&server_id) else {
            let message = format!("downstream unavailable: {server_slug}");
            self.log_reject(
                &context,
                "resources/read",
                Some(&server_id),
                &uri,
                &revision.workspace_id,
                "unavailable",
                &message,
                Some(&request),
            )
            .await;
            return Err(ErrorData::internal_error(message, None));
        };

        let started = std::time::Instant::now();
        let result = handle.read_resource(&uri).await;
        let duration_ms = started.elapsed().as_millis() as i64;
        let response = result
            .as_ref()
            .ok()
            .and_then(|value| serde_json::to_value(value).ok());
        self.log_call(
            &context,
            CallLog {
                method: "resources/read",
                server_id: Some(&server_id),
                exposed_name: &uri,
                workspace_id: Some(&revision.workspace_id),
                duration_ms,
                retry_count: 0,
                request: Some(&request),
                response: response.as_ref(),
                outcome: match result.as_ref() {
                    Ok(_) => LogOutcome::Ok,
                    Err(error) => LogOutcome::Downstream(error),
                },
            },
        )
        .await;
        let mut result = result.map_err(downstream_error_data)?;
        apply_cache_hints(
            &context,
            READ_CACHE_TTL_MS,
            &mut result.ttl_ms,
            &mut result.cache_scope,
        );
        Ok(result.into())
    }
}

/// DownstreamError → 上游 MCP 错误（tools/prompts/resources 三路共用）。
fn downstream_error_data(error: mcpane_hub_downstream::DownstreamError) -> ErrorData {
    match &error {
        mcpane_hub_downstream::DownstreamError::Timeout(t) => {
            ErrorData::internal_error(format!("downstream timeout after {t:?}"), None)
        }
        other => ErrorData::internal_error(other.to_string(), None),
    }
}

impl HubService {
    /// 聚合路由 + 校验链 + 下游调用（tools/call 与 mcpane__tool_execute 共用）：
    /// 路由查本 Session pin 的 Revision（目标绝不中途改变，ADR-004）；
    /// 禁用与访问控制实时查询（撤销对活跃会话即时生效）。
    /// `via_execute`：调用是否来自 mcpane__tool_execute。只有该路径附加本地
    /// 校验自愈提示——直连调用的客户端手里本就有真实 schema，且其响应形状
    /// 受双栈守卫约束，必须逐字节不变。
    async fn call_downstream_tool(
        &self,
        context: &RequestContext<RoleServer>,
        name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
        mrtr: MrtrRetry,
        via_execute: bool,
    ) -> Result<CallToolResponse, ErrorData> {
        let revision = self.pinned_for(context)?;
        let request_json = arguments
            .as_ref()
            .map(|map| serde_json::Value::Object(map.clone()));
        let Some(route) = revision.route(name) else {
            // execute 路径打错名字不能是死胡同：附最近似候选供模型自愈。
            let hint = if via_execute {
                let connection_id = Self::connection_id_of(context);
                let visible: Vec<&mcpane_hub_catalog::CatalogTool> = revision
                    .tools
                    .iter()
                    .filter(|t| !self.state.is_server_denied(&connection_id, &t.server_id))
                    .collect();
                let candidates = mcpane_hub_catalog::search::search_tools(&visible, name, 5)
                    .into_iter()
                    .map(|t| t.exposed_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                if candidates.is_empty() {
                    String::from("；调用 mcpane__tool_discovery（不传 query）查看全量目录")
                } else {
                    format!("；最相近的候选：{candidates}")
                }
            } else {
                String::new()
            };
            let message = format!("unknown tool: {name}{hint}");
            self.log_reject(
                context,
                "tools/call",
                None,
                name,
                &revision.workspace_id,
                "unknown",
                &message,
                request_json.as_ref(),
            )
            .await;
            return Err(ErrorData::invalid_params(message, None));
        };
        // 本地轻量校验只产提示、不拦截：校验器可能因 schema 方言/漂移误杀，
        // 下游才是参数合法性的最终裁判（回退无条件原则，ADR-020）。
        let validation_findings = if via_execute {
            validate_arguments(&route.input_schema, arguments.as_ref())
        } else {
            Vec::new()
        };

        // 二次权限校验：pin 之后被禁用的 Tool 立即拒绝（按本 Session 的 Workspace）。
        let workspace_id = self.session_workspace(context)?;
        if self
            .state
            .is_tool_disabled(&workspace_id, &route.server_id, &route.original_name)
        {
            let message = format!("tool disabled by workspace policy: {name}");
            self.log_reject(
                context,
                "tools/call",
                Some(&route.server_id),
                &route.exposed_name,
                &revision.workspace_id,
                "policy",
                &message,
                request_json.as_ref(),
            )
            .await;
            return Err(ErrorData::invalid_params(message, None));
        }

        // 访问控制：该连接被拒绝访问目标 Server 时立即拒绝。
        let connection_id = Self::connection_id_of(context);
        if self
            .state
            .is_server_denied(&connection_id, &route.server_id)
        {
            let message = format!("server disabled by client access control: {name}");
            self.log_reject(
                context,
                "tools/call",
                Some(&route.server_id),
                &route.exposed_name,
                &revision.workspace_id,
                "policy",
                &message,
                request_json.as_ref(),
            )
            .await;
            return Err(ErrorData::invalid_params(message, None));
        }

        // 下游调用。
        let Some(handle) = self.state.downstream(&route.server_id) else {
            let message = format!("downstream unavailable: {}", route.server_slug);
            self.log_reject(
                context,
                "tools/call",
                Some(&route.server_id),
                &route.exposed_name,
                &revision.workspace_id,
                "unavailable",
                &message,
                request_json.as_ref(),
            )
            .await;
            return Err(ErrorData::internal_error(message, None));
        };

        // MRTR 续轮标记：带 requestState/inputResponses 的请求是同一次交互的
        // 第 2+ 轮（日志的 retry_count 字段与首轮区分）。
        let mrtr_round = i64::from(mrtr.request_state.is_some() || mrtr.input_responses.is_some());

        // MRTR 重试：上游回传的 requestState 是我们密封过的，解封才拿得到下游
        // 那一份。associated_data 绑死 server_id——拿着 A 下游的 state 去调 B
        // 下游会在这里解封失败，而不是把一轮交互的中间态跨下游重放。
        let downstream_state = match &mrtr.request_state {
            None => None,
            Some(sealed) => Some(
                request_state_codec()
                    .open_with(sealed, route.server_id.as_bytes())
                    .map_err(|error| {
                        ErrorData::invalid_params(
                            format!("invalid or expired requestState: {error}"),
                            None,
                        )
                    })
                    .and_then(|bytes| {
                        String::from_utf8(bytes).map_err(|_| {
                            ErrorData::internal_error("corrupt requestState payload", None)
                        })
                    })?,
            ),
        };

        // 旧式确认桥接：下游若在本次调用期间发来 elicitation/create（2025-06-18
        // 的 server→client 直连形态，未走 MRTR），转发给触发本调用的上游。
        // guard 存活期即登记期，调用结束自动注销。
        let _elicitation_bridge = self.register_elicitation_bridge(context, &handle);

        // 进度转发：上游带 progressToken 时登记转发器，本次调用期间下游报的
        // notifications/progress 换回上游 token 转发——长任务不再黑盒等待。
        // 下游侧 token 由 rmcp 每请求自动注入，这里无需（也无法）指定。
        // 顺序转发：每条各起一个 task 会让写入顺序看调度器脸色，上游可能看到
        // 进度倒退。单通道单消费者，送达顺序即下游上报顺序；上游消费太慢时
        // try_send 丢新保序（进度本就允许有损）。guard 落地 → 发送端随之 drop
        // → 消费任务自然退出。
        let _progress_bridge = context.meta.get_progress_token().map(|upstream_token| {
            let peer = context.peer.clone();
            let (tx, mut rx) =
                tokio::sync::mpsc::channel::<rmcp::model::ProgressNotificationParam>(32);
            tokio::spawn(async move {
                while let Some(mut params) = rx.recv().await {
                    params.progress_token = upstream_token.clone();
                    let _ = peer.notify_progress(params).await;
                }
            });
            handle.progress_relay().register(Arc::new(move |params| {
                let _ = tx.try_send(params);
            }))
        });

        let started = std::time::Instant::now();
        // 取消级联：上游取消本请求（context.ct 触发）时，下游同步收到
        // notifications/cancelled，不再白跑。
        let response = handle
            .call_tool_once(
                &route.original_name,
                arguments,
                mrtr.input_responses,
                downstream_state,
                context.ct.clone(),
            )
            .await;
        let duration_ms = started.elapsed().as_millis() as i64;
        let response_json = response.as_ref().ok().map(tool_response_json);
        let mut tool_err = None;
        // input_required 记为成功：它是"还没结束"，不是失败，不该把正常的多轮
        // 交互刷成一片错误日志。下游 `isError: true` 仍是 MCP 成功信封，
        // 但业务失败必须记红灯，否则详情里是错误内容、列表却显示成功。
        let outcome = match &response {
            Ok(call) => complete_tool_outcome(call, &mut tool_err),
            Err(error) => LogOutcome::Downstream(error),
        };
        self.log_call(
            context,
            CallLog {
                method: "tools/call",
                server_id: Some(&route.server_id),
                exposed_name: &route.exposed_name,
                workspace_id: Some(&revision.workspace_id),
                duration_ms,
                retry_count: mrtr_round,
                request: request_json.as_ref(),
                response: response_json.as_ref(),
                outcome,
            },
        )
        .await;

        match response.map_err(|error| {
            let mut data = downstream_error_data(error);
            if !validation_findings.is_empty() {
                data.message = format!(
                    "{}；本地参数校验发现：{}",
                    data.message,
                    validation_findings.join("；")
                )
                .into();
                data.data
                    .get_or_insert_with(|| serde_json::json!({}))
                    .as_object_mut()
                    .map(|obj| obj.insert("inputSchema".into(), route.input_schema.clone()));
            }
            data
        })? {
            rmcp::model::CallToolResponse::Complete(mut result) => {
                // 下游业务失败 + 本地校验有发现 → 附 schema 供模型一轮自愈。
                // 校验干净的失败不加料：业务错误里塞 schema 只是噪音。
                if result.is_error == Some(true) && !validation_findings.is_empty() {
                    result.content.push(rmcp::model::ContentBlock::text(format!(
                        "本地参数校验发现：{}。该工具的完整 inputSchema：{}",
                        validation_findings.join("；"),
                        route.input_schema
                    )));
                }
                Ok(result.into())
            }
            rmcp::model::CallToolResponse::InputRequired(mut pending) => {
                // 上游若是 legacy，它既不认识 input_required 也不会回传
                // requestState，透传过去只会得到一个无从解释的错误。
                if !is_modern_request(context) {
                    return Err(ErrorData::internal_error(
                        format!(
                            "downstream tool {name} needs additional input, \
                             which requires MCP 2026-07-28; this connection negotiated an older version"
                        ),
                        None,
                    ));
                }
                // 重新密封：上游拿到的永远是我们的 state，看不到也伪造不了下游那份。
                pending.request_state = pending.request_state.map(|state| {
                    request_state_codec().seal_with(
                        state.as_bytes(),
                        &rmcp::model::SealOptions::new()
                            .associated_data(route.server_id.as_bytes())
                            .ttl(REQUEST_STATE_TTL),
                    )
                });
                Ok(rmcp::model::CallToolResponse::InputRequired(pending))
            }
            rmcp::model::CallToolResponse::Task(task) => {
                if !is_modern_request(context) {
                    return Err(ErrorData::internal_error(
                        format!(
                            "downstream tool {name} returned a task handle, \
                             which requires MCP 2026-07-28; this connection negotiated an older version"
                        ),
                        None,
                    ));
                }
                Ok(rmcp::model::CallToolResponse::Task(task))
            }
            // CallToolResponse 是 non_exhaustive：将来 SDK 加了新变体，宁可在
            // 这里明确报错，也不要静默按成功透传一个我们没处理过的语义。
            _ => Err(ErrorData::internal_error(
                format!("downstream returned an unsupported response kind for tool {name}"),
                None,
            )),
        }
    }

    /// Hub 侧拒绝（未知/策略/下游不可用）：打到下游前就失败，也要留下记录，
    /// 否则「工具没跑」在日志里是空白。
    #[allow(clippy::too_many_arguments)]
    async fn log_reject(
        &self,
        context: &RequestContext<RoleServer>,
        method: &'static str,
        server_id: Option<&str>,
        exposed_name: &str,
        workspace_id: &str,
        category: &'static str,
        message: &str,
        request: Option<&serde_json::Value>,
    ) {
        self.log_call(
            context,
            CallLog {
                method,
                server_id,
                exposed_name,
                workspace_id: Some(workspace_id),
                duration_ms: 0,
                retry_count: 0,
                request,
                response: None,
                outcome: LogOutcome::Failed { category, message },
            },
        )
        .await;
    }

    /// 请求日志（TASK-407：元数据 + 脱敏截断摘要）。`exposed_name` 对
    /// resources 为其 URI；内建工具没有下游 server_id。
    async fn log_call(&self, context: &RequestContext<RoleServer>, log: CallLog<'_>) {
        let parts = context.extensions.get::<http::request::Parts>();
        let identity = parts.and_then(|p| p.extensions.get::<ClientIdentity>());
        let request_id = parts
            .and_then(|p| p.extensions.get::<RequestId>())
            .map(|r| r.0.clone())
            .unwrap_or_else(|| "req_unknown".to_owned());
        let session_id = parts
            .and_then(|p| p.headers.get("mcp-session-id"))
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);

        let (success, error_category, error_message) = match log.outcome {
            LogOutcome::Ok => (true, None, None),
            LogOutcome::Downstream(error) => (
                false,
                downstream_err_category(error),
                Some(error.to_string()),
            ),
            LogOutcome::Failed { category, message } => {
                (false, Some(category.to_owned()), Some(message.to_owned()))
            }
        };
        tracing::info!(
            request_id = %request_id,
            method = %log.method,
            exposed_name = %log.exposed_name,
            server_id = %log.server_id.unwrap_or("-"),
            duration_ms = log.duration_ms,
            success,
            error_category = error_category.as_deref().unwrap_or("-"),
            "downstream call"
        );
        if let Some(logs) = &self.state.logs {
            let mut entry = mcpane_storage::RequestLogEntry {
                id: String::new(),
                ts: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0),
                request_id,
                client_connection_id: identity.map(|i| i.client_connection_id.clone()),
                session_id,
                workspace_id: log.workspace_id.map(str::to_owned),
                server_id: log.server_id.map(str::to_owned),
                exposed_name: Some(log.exposed_name.to_owned()),
                method: log.method.into(),
                duration_ms: log.duration_ms,
                success,
                error_category,
                request_size: None,
                response_size: None,
                retry_count: log.retry_count,
                request_summary: None,
                response_summary: None,
                error_message: None,
            };
            entry.attach_payloads(log.request, log.response);
            if let Some(message) = error_message {
                entry.attach_error(&message);
            }
            if let Err(error) = logs.append(&entry).await {
                tracing::warn!(%error, "请求日志写入失败");
            }
        }
    }
}

struct CallLog<'a> {
    method: &'static str,
    server_id: Option<&'a str>,
    exposed_name: &'a str,
    workspace_id: Option<&'a str>,
    duration_ms: i64,
    retry_count: i64,
    request: Option<&'a serde_json::Value>,
    response: Option<&'a serde_json::Value>,
    outcome: LogOutcome<'a>,
}

enum LogOutcome<'a> {
    Ok,
    Downstream(&'a mcpane_hub_downstream::DownstreamError),
    Failed {
        category: &'static str,
        message: &'a str,
    },
}

/// 下游结果写成可落盘的 JSON：Complete 原样序列化；多轮/任务只记状态，
/// 不把密封的 requestState 写进日志。
fn tool_response_json(response: &CallToolResponse) -> serde_json::Value {
    match response {
        CallToolResponse::Complete(result) => {
            serde_json::to_value(result).unwrap_or(serde_json::Value::Null)
        }
        CallToolResponse::InputRequired(_) => {
            serde_json::json!({ "status": "input_required" })
        }
        CallToolResponse::Task(task) => {
            serde_json::to_value(task).unwrap_or_else(|_| serde_json::json!({ "status": "task" }))
        }
        _ => serde_json::json!({ "status": "unsupported" }),
    }
}

fn complete_tool_outcome<'a>(
    response: &'a CallToolResponse,
    tool_error: &'a mut Option<String>,
) -> LogOutcome<'a> {
    if let CallToolResponse::Complete(result) = response
        && result.is_error == Some(true)
    {
        *tool_error = Some(tool_error_text(result));
        return LogOutcome::Failed {
            category: "tool",
            message: tool_error.as_deref().unwrap_or("tool returned isError"),
        };
    }
    LogOutcome::Ok
}

fn tool_error_text(result: &rmcp::model::CallToolResult) -> String {
    let texts: Vec<&str> = result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|t| t.text.as_str()))
        .collect();
    if texts.is_empty() {
        "tool returned isError".into()
    } else {
        texts.join("\n")
    }
}

fn downstream_err_category(error: &mcpane_hub_downstream::DownstreamError) -> Option<String> {
    match error {
        mcpane_hub_downstream::DownstreamError::Timeout(_) => Some("timeout".into()),
        mcpane_hub_downstream::DownstreamError::Spawn { .. } => Some("spawn".into()),
        mcpane_hub_downstream::DownstreamError::Call(_) => Some("call".into()),
        mcpane_hub_downstream::DownstreamError::Config(_) => Some("config".into()),
        mcpane_hub_downstream::DownstreamError::AuthRequired { .. }
        | mcpane_hub_downstream::DownstreamError::Auth(_) => Some("auth".into()),
    }
}

#[cfg(test)]
mod tests {
    /// 真机回归（ADR-020）：schemars 对裸 `serde_json::Value` 字段生成布尔
    /// schema `true`——JSON Schema 规范合法，但 Claude Code 2.1.226 的解析层
    /// 吃不下，**一个工具带上它整张 tools/list 都会被丢弃**（网关模式全部
    /// 工具消失，实测半天才定位到）。守卫：全部内建工具的 input/output
    /// schema 里，任何 properties/$defs 的成员都必须是对象，不得是布尔。
    #[test]
    #[allow(clippy::unwrap_used)]
    fn builtin_tool_schemas_never_contain_boolean_schemas() {
        fn assert_no_bool_schemas(tool_name: &str, path: &str, value: &serde_json::Value) {
            if let Some(map) = value.as_object() {
                for key in ["properties", "$defs"] {
                    if let Some(members) = map.get(key).and_then(|v| v.as_object()) {
                        for (name, member) in members {
                            assert!(
                                member.is_object(),
                                "{tool_name}: {path}.{key}.{name} 是布尔 schema（{member}），\
                                 Claude Code 会丢弃整张 tools/list"
                            );
                        }
                    }
                }
                for (key, nested) in map {
                    assert_no_bool_schemas(tool_name, &format!("{path}.{key}"), nested);
                }
            } else if let Some(items) = value.as_array() {
                for (i, nested) in items.iter().enumerate() {
                    assert_no_bool_schemas(tool_name, &format!("{path}[{i}]"), nested);
                }
            }
        }
        for tool in super::HubService::tool_router().list_all() {
            let input = serde_json::to_value(tool.input_schema.as_ref()).unwrap();
            assert_no_bool_schemas(&tool.name, "inputSchema", &input);
            if let Some(output) = &tool.output_schema {
                let output = serde_json::to_value(output.as_ref()).unwrap();
                assert_no_bool_schemas(&tool.name, "outputSchema", &output);
            }
        }
    }

    /// auto 判定的四种组合。工具数这一维是 ADR-020 之后补的：网关是三跳漏斗，
    /// 只有工具多到平铺装不下时才值得付那个代价。
    #[test]
    fn auto_exposure_prefers_flat_until_tools_outgrow_the_limit() {
        use crate::state::ExposureMode;
        let claude = Some(ClientType::Claude);
        let codex = Some(ClientType::Codex);

        // Claude Code 有 ToolSearch，工具再多也平铺。
        assert_eq!(
            super::resolve_auto_exposure(claude, true, Some(AUTO_FLAT_TOOL_LIMIT * 10)),
            ExposureMode::Flat
        );
        // 发现开关关着：与改动前一致，平铺。
        assert_eq!(
            super::resolve_auto_exposure(codex, false, Some(AUTO_FLAT_TOOL_LIMIT * 10)),
            ExposureMode::Flat
        );
        // 发现开关开着但装得下 → 改走平铺（本次改动的目标行为）。
        assert_eq!(
            super::resolve_auto_exposure(codex, true, Some(AUTO_FLAT_TOOL_LIMIT)),
            ExposureMode::Flat,
            "边界值本身应算装得下"
        );
        // 超出一个就切网关。
        assert_eq!(
            super::resolve_auto_exposure(codex, true, Some(AUTO_FLAT_TOOL_LIMIT + 1)),
            ExposureMode::Gateway
        );
        // 数不出来时保持发现开关的原判定，不拿未知数翻配置。
        assert_eq!(
            super::resolve_auto_exposure(codex, true, None),
            ExposureMode::Gateway
        );
    }

    /// 真机抓到的缺陷（ADR-021 验证项 2）：拼好的描述整体从尾部截，触发提示
    /// 恰在尾部，「关键词：搜索、查找、检索」被切成「关键词：搜索…」——而关键词
    /// 是触发匹配最靠的部分。预算必须优先给提示。
    #[test]
    fn catalog_line_spends_its_budget_on_the_trigger_hint_first() {
        let long_desc = "下游自述".repeat(60); // 240 字符，远超预算
        let hint = "Use when: 需要检索代码仓库时。关键词：搜索、查找、检索、定位";

        let line = super::catalog_desc_line(Some(&long_desc), Some(hint));
        assert!(
            line.ends_with(hint),
            "触发提示必须完整保留在行尾，实际：{line}"
        );
        assert!(
            line.chars().count() <= CATALOG_TOOL_DESC_MAX_CHARS + 1,
            "总长仍要受上界钉死（+1 是省略号）：{}",
            line.chars().count()
        );
        assert!(line.contains('…'), "被牺牲的是下游自述，该留省略号：{line}");

        // 没有提示时行为不变：整体截断。
        let plain = super::catalog_desc_line(Some(&long_desc), None);
        assert!(plain.chars().count() <= CATALOG_TOOL_DESC_MAX_CHARS + 1);
        assert!(plain.starts_with("下游自述"));

        // 提示本身就超预算：下游自述整个让位，只截提示。
        let huge_hint = "触发条件".repeat(60);
        let clipped = super::catalog_desc_line(Some(&long_desc), Some(&huge_hint));
        assert!(clipped.starts_with("触发条件"), "{clipped}");
        assert!(
            !clipped.contains("下游自述"),
            "预算不够时自述该整个让位：{clipped}"
        );

        assert_eq!(super::catalog_desc_line(None, None), "");
    }

    /// 两种形态都得自报家门。用户的准则是静态文本、形态却按客户端与工具数
    /// 自适应——一份为网关写的准则碰上平铺，就是在指挥模型去调一个不存在的
    /// `tool_discovery`。只在网关时说明，等于放任那份准则误导模型。
    #[test]
    fn both_exposure_shapes_announce_themselves() {
        let mut revision =
            mcpane_hub_catalog::CatalogRevision::new("rev".into(), "ws".into(), Vec::new());
        revision.servers = vec![mcpane_hub_catalog::CatalogServer {
            server_id: "s1".into(),
            server_slug: "gh".into(),
            instructions: None,
        }];

        let gateway = super::compose_instructions(&revision, |_| false, true, "");
        assert!(gateway.contains("网关模式"), "{gateway}");
        assert!(gateway.contains("mcpane__tool_discovery"), "{gateway}");

        // 平铺只在用户写了准则时自报——那句话是给「准则与形态对不上」兜底的。
        let with_guidance =
            super::compose_instructions(&revision, |_| false, false, "代码搜索先用 X");
        assert!(
            with_guidance.contains("平铺形态"),
            "配了准则就必须说明当前形态：{with_guidance}"
        );
        assert!(
            !with_guidance.contains("mcpane__tool_discovery"),
            "平铺下不该提网关才有的工具：{with_guidance}"
        );

        // 没写准则的用户不存在「准则过时」这个问题，这句话对他们纯属噪音——
        // instructions 每长一分，真正重要的那几句就被稀释一分。
        let no_guidance = super::compose_instructions(&revision, |_| false, false, "");
        assert!(
            !no_guidance.contains("平铺形态"),
            "没配准则时不该多这一句：{no_guidance}"
        );

        // 一个 Server 都看不到时同样不说——那时整段能力总览本就不展开。
        let none = super::compose_instructions(&revision, |_| true, false, "代码搜索先用 X");
        assert!(
            !none.contains("平铺形态"),
            "无可见 Server 时不该自报形态：{none}"
        );
    }

    #[test]
    fn gateway_catalog_in_instructions_truncates_tool_descriptions() {
        // 目录膨胀护栏：每个工具在目录里的占用被描述截断钉死上界，
        // 100 个长描述工具也不该把 instructions 撑到不可控。
        let tools: Vec<mcpane_hub_catalog::CatalogTool> = (0..100)
            .map(|i| mcpane_hub_catalog::CatalogTool {
                server_id: "srv_a".into(),
                server_slug: "alpha".into(),
                original_name: format!("tool_{i}"),
                exposed_name: format!("alpha__tool_{i}"),
                description: Some("很长的描述 ".repeat(200)),
                description_suffix: None,
                input_schema: serde_json::json!({"type": "object"}),
                output_schema: None,
                annotations: None,
                declared_annotations: None,
                icons: None,
                schema_hash: String::new(),
            })
            .collect();
        let mut revision =
            mcpane_hub_catalog::CatalogRevision::new("rev_test".into(), "ws_default".into(), tools);
        revision.servers = vec![mcpane_hub_catalog::CatalogServer {
            server_id: "srv_a".into(),
            server_slug: "alpha".into(),
            instructions: None,
        }];
        let text = super::compose_instructions(&revision, |_| false, true, "");
        assert!(text.contains("alpha__tool_0"), "目录须含工具名");
        assert!(text.contains('…'), "超长描述必须截断");
        // 上界：每工具 ≈ 名字 + 截断描述(100) + 标点缩进，放宽到 200 字符。
        let per_tool_budget = 200;
        assert!(
            text.chars().count() < 100 * per_tool_budget + 1000,
            "目录膨胀失控：{} chars",
            text.chars().count()
        );
    }

    use super::*;

    #[test]
    fn all_builtin_tools_use_frozen_prefix() {
        let tools = HubService::tool_router().list_all();
        assert!(!tools.is_empty());
        for tool in &tools {
            assert!(
                tool.name.starts_with(TOOL_PREFIX),
                "内建 Tool `{}` 未使用 `{TOOL_PREFIX}` 前缀",
                tool.name
            );
        }
        let names: Vec<_> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert!(names.contains(&"mcpane__echo"));
        assert!(names.contains(&"mcpane__connection_info"));
    }
}
