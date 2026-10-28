//! Tool/Prompt/Resource Catalog（TASK-405，ADR-006）。
//!
//! - Tool/Prompt 暴露名：`{server_slug}__{original_name}`；
//! - 规范化后必须符合 MCP name 约束（`[a-zA-Z0-9_-]{1,128}`）；
//! - 超长截断 + 稳定短哈希；Workspace 内唯一，冲突先到先得跳过后者；
//! - Resource URI 不改写、原样透传，跨下游冲突同样先到先得；
//! - Revision 一旦生成即不可变（Session-Pinned，ADR-004）；
//! - `TOOL_PREFIX` 是 Hub 内建工具（`mcpane__echo` 等，见
//!   `mcpane-hub::hub::TOOL_PREFIX`）冻结占用的命名空间（ADR-006/ADR-011），
//!   唯一事实源收在本 crate：本 crate 负责生成/校验暴露名，若常量另存一份在
//!   调用方（hub 二进制），两边随时可能各自改动而互不知情——此前正是如此，
//!   `is_valid_alias`/`exposed_name` 对这个前缀一无所知，用户能把下游工具
//!   alias 成 `mcpane__echo` 而不被拒绝，造成 tools/list 里的重名与永久
//!   不可达（见回归测试）。

pub mod sanitize;
pub mod search;

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

/// MCP Tool name 最大长度（规范约束 128，预留余量）。
const MAX_TOOL_NAME_LEN: usize = 120;

/// Hub 内建工具冻结前缀（ADR-006/ADR-011）。任何暴露名——无论来自默认命名
/// 还是用户 alias——都不得落进这个命名空间，否则会撞上 Hub 自己的
/// `mcpane__echo` / `mcpane__tool_discovery` / `mcpane__tool_execute` /
/// `mcpane__connection_info`。定义下沉在 mcpane-hub-protocol（桌面端也要
/// 用同一份 slug 规则），这里连同 `normalize_slug` 一起 re-export，
/// `mcpane-hub` 侧再从这里 re-export，三层同源。
pub use mcpane_hub_protocol::{DeclaredAnnotations, TOOL_PREFIX, normalize_slug};

/// Catalog 构建错误。
///
/// 注：`build_revision` 现已**就地降级处理**命名冲突（跳过后者）与非法 alias
/// （回退默认名），不再据此中止整张 Catalog，故这两个变体当前不会被构造；
/// 保留类型与 `Result` 签名是为 API 稳定，也为将来可能新增的硬错误留出通道。
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum CatalogError {
    #[error("暴露名冲突：{exposed_name}（{server_a} 与 {server_b}）")]
    NameConflict {
        exposed_name: String,
        server_a: String,
        server_b: String,
    },
    #[error("alias 非法：{0}")]
    InvalidAlias(String),
}

/// 单条 Tool 路由（Revision 内不可变）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogTool {
    pub server_id: String,
    pub server_slug: String,
    pub original_name: String,
    pub exposed_name: String,
    pub description: Option<String>,
    /// 用户配置的触发提示，**在 `description` 里已拼好**，这里再留一份原文。
    /// 网关目录要按「先保它、再截下游自述」的优先级分配截断预算——拼完就没法
    /// 还原边界了（下游描述本身可能带换行）。
    pub description_suffix: Option<String>,
    pub input_schema: serde_json::Value,
    pub output_schema: Option<serde_json::Value>,
    /// **下游申报的**注解，原样透传；下游没申报就是 None，Hub 永不代填
    /// （ADR-002 Gate）。用户自己标的那份在 `declared_annotations`，两者
    /// 分列而存——目录里任何一处都能分辨这句话是谁说的。
    pub annotations: Option<serde_json::Value>,
    /// **用户声明的**注解（ADR-002 Gate 的例外口）。只在 `annotations` 为
    /// None 时才会被出口采用：它是补空位的，不是改下游的口径。
    ///
    /// 来源是全局表 `tool_declared_annotations`（键不含 workspace，迁移 0025）
    /// ——注解陈述的是工具行为，跨工作区恒成立；`load_overrides` 在每个工作区
    /// 重建时并入同一份。
    pub declared_annotations: Option<DeclaredAnnotations>,
    /// 下游申报的图标（原样透传；桌面端与上游客户端自行选型渲染）。
    pub icons: Option<serde_json::Value>,
    pub schema_hash: String,
}

/// 一条工具注解的来源。上游客户端只看得到注解本身（`ToolAnnotations` 是
/// 规范里的闭合结构，塞不进自定义标记），所以来源只在 Hub 内部、日志、
/// 桌面端与 `mcpane__tool_describe` 里体现——用户始终认得出哪条是自己标的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AnnotationSource {
    /// 下游 Server 自己申报的。
    Downstream,
    /// 用户自己声明的（全局，非按工作区）。
    User,
}

impl CatalogTool {
    /// 下游自述部分（把已拼进 `description` 的触发提示摘掉）。
    ///
    /// 网关目录要分别对待两段（预算优先给提示），拿不到这一段就会把拼好的整体
    /// 当成下游自述，截断后残留提示的开头、后面又完整追加一遍。剥离规则与
    /// [`append_suffix`] 的拼接规则成对，故放在一处——分散到调用方去各写各的，
    /// 改了拼接就必然对不上。剥不掉时退回完整 `description`（不比不剥更差）。
    pub fn downstream_description(&self) -> Option<&str> {
        let desc = self.description.as_deref()?;
        let Some(suffix) = self.description_suffix.as_deref().filter(|s| !s.is_empty()) else {
            return Some(desc);
        };
        Some(
            desc.strip_suffix(suffix)
                .map(str::trim_end)
                .filter(|d| !d.is_empty())
                .unwrap_or(desc),
        )
    }

    /// 出口该发哪份注解，以及这份是谁说的。
    ///
    /// 决策只此一处：下游申报优先原样透传，缺席时才拿用户声明补空位，两者
    /// 都没有就是 None（Hub 不编）。tools/list 与 tool_describe 共用它，
    /// 免得两个出口各判一次、判出两种口径。
    pub fn effective_annotations(&self) -> Option<(serde_json::Value, AnnotationSource)> {
        if let Some(downstream) = &self.annotations {
            return Some((downstream.clone(), AnnotationSource::Downstream));
        }
        let declared = self.declared_annotations.as_ref()?;
        let value = serde_json::to_value(declared).ok()?;
        Some((value, AnnotationSource::User))
    }
}

/// 单条 Prompt 路由。命名空间与 Tool 同规（`{slug}__{name}`，ADR-006）。
///
/// `raw` 是下游返回的 `rmcp::model::Prompt` 原样序列化——上游 list 时反序列化
/// 后仅替换 name 为暴露名，title/arguments/icons 等字段零损耗透传。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogPrompt {
    pub server_id: String,
    pub server_slug: String,
    pub original_name: String,
    pub exposed_name: String,
    pub raw: serde_json::Value,
}

/// 单条 Resource 路由。
///
/// URI **不做改写**：URI 是客户端可持有的语义标识（`file:///...` 等），套
/// `{slug}__` 前缀会产出非法 URI，改写 scheme 则破坏客户端对协议的理解
/// （mcp-router 为此维护了一套 originalProtocols 还原补丁）。跨下游 URI
/// 冲突按 ADR-006 精神先到先得跳过后者。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogResource {
    pub server_id: String,
    pub server_slug: String,
    pub uri: String,
    pub raw: serde_json::Value,
}

/// 单条 Resource Template 路由（URI 模板同样透传，冲突同规）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogResourceTemplate {
    pub server_id: String,
    pub server_slug: String,
    pub uri_template: String,
    pub raw: serde_json::Value,
}

/// 参与本 Revision 的下游 Server 元信息（instructions 供 Hub 聚合成
/// 会话开场的能力总览——它是客户端会注入模型上下文的字段，丢掉可惜）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogServer {
    pub server_id: String,
    pub server_slug: String,
    pub instructions: Option<String>,
}

/// 某 Workspace 的稳定 Catalog 快照。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogRevision {
    pub id: String,
    pub workspace_id: String,
    pub servers: Vec<CatalogServer>,
    pub tools: Vec<CatalogTool>,
    pub prompts: Vec<CatalogPrompt>,
    pub resources: Vec<CatalogResource>,
    pub resource_templates: Vec<CatalogResourceTemplate>,
    /// exposed_name → tools 下标（Revision 不可变，首次 route 时构建，
    /// 之后每次工具调用 O(log n) 查找而非线性扫描）。
    #[serde(skip)]
    index: std::sync::OnceLock<BTreeMap<String, usize>>,
    /// exposed_name → prompts 下标。
    #[serde(skip)]
    prompt_index: std::sync::OnceLock<BTreeMap<String, usize>>,
    /// uri → resources 下标。
    #[serde(skip)]
    resource_index: std::sync::OnceLock<BTreeMap<String, usize>>,
    /// 被 override 显式禁用的 resource URI。read_resource 回落 template 前
    /// 必查——否则「禁用具体 resource」会被同源 template 的字面前缀兜底放行。
    #[serde(skip)]
    denied_resource_uris: std::collections::BTreeSet<String>,
}

impl CatalogRevision {
    pub fn new(id: String, workspace_id: String, tools: Vec<CatalogTool>) -> Self {
        Self {
            id,
            workspace_id,
            servers: Vec::new(),
            tools,
            prompts: Vec::new(),
            resources: Vec::new(),
            resource_templates: Vec::new(),
            index: std::sync::OnceLock::new(),
            prompt_index: std::sync::OnceLock::new(),
            resource_index: std::sync::OnceLock::new(),
            denied_resource_uris: std::collections::BTreeSet::new(),
        }
    }

    /// 填入 prompts/resources（build_revision 专用；空 Revision 走 `new` 即可）。
    pub fn with_prompts_and_resources(
        mut self,
        prompts: Vec<CatalogPrompt>,
        resources: Vec<CatalogResource>,
        resource_templates: Vec<CatalogResourceTemplate>,
        denied_resource_uris: std::collections::BTreeSet<String>,
    ) -> Self {
        self.prompts = prompts;
        self.resources = resources;
        self.resource_templates = resource_templates;
        self.denied_resource_uris = denied_resource_uris;
        self
    }

    /// 该 URI 是否被 override 显式禁用（read_resource 回落 template 前查）。
    pub fn is_resource_denied(&self, uri: &str) -> bool {
        self.denied_resource_uris.contains(uri)
    }

    pub fn route(&self, exposed_name: &str) -> Option<&CatalogTool> {
        let index = self.index.get_or_init(|| {
            self.tools
                .iter()
                .enumerate()
                .map(|(i, t)| (t.exposed_name.clone(), i))
                .collect()
        });
        index.get(exposed_name).map(|&i| &self.tools[i])
    }

    pub fn route_prompt(&self, exposed_name: &str) -> Option<&CatalogPrompt> {
        let index = self.prompt_index.get_or_init(|| {
            self.prompts
                .iter()
                .enumerate()
                .map(|(i, p)| (p.exposed_name.clone(), i))
                .collect()
        });
        index.get(exposed_name).map(|&i| &self.prompts[i])
    }

    /// Resource 路由：URI 精确命中 Catalog 条目。
    pub fn route_resource(&self, uri: &str) -> Option<&CatalogResource> {
        let index = self.resource_index.get_or_init(|| {
            self.resources
                .iter()
                .enumerate()
                .map(|(i, r)| (r.uri.clone(), i))
                .collect()
        });
        index.get(uri).map(|&i| &self.resources[i])
    }

    /// Template 路由：URI 未命中静态条目时，按模板**字面前缀**（首个 `{` 之前的
    /// 部分）匹配，取最长命中者——确定性优于「挨个下游试调」。模板数量小，线性扫。
    pub fn route_resource_template(&self, uri: &str) -> Option<&CatalogResourceTemplate> {
        self.resource_templates
            .iter()
            .filter_map(|t| {
                let literal = t.uri_template.split('{').next().unwrap_or_default();
                (!literal.is_empty() && uri.starts_with(literal)).then_some((literal.len(), t))
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, t)| t)
    }
}

/// 生成暴露名：`{slug}__{original}`，非法字符转 `_`，超长截断加稳定短哈希。
///
/// 拼出来的名字若落进 [`TOOL_PREFIX`] 保留命名空间——通常是下游 Server 的
/// slug 未经 [`normalize_slug`] 校验、直接被调用方传成了 `mcpane`（或本身
/// 就带着 `mcpane__` 前缀）——会被 [`escape_reserved_prefix`] 改写掉，不会
/// 原样放行。检查最终结果而非"slug 是否等于 mcpane"这一个特例，是为了
/// 兜住任何未经 `normalize_slug` 的调用点，不依赖调用方老实。
pub fn exposed_name(server_slug: &str, original_name: &str) -> String {
    let sanitized: String = original_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let candidate = escape_reserved_prefix(format!("{server_slug}__{sanitized}"));
    if candidate.len() <= MAX_TOOL_NAME_LEN {
        return candidate;
    }
    // 稳定短哈希基于完整原始输入，截断后仍可复现且唯一。
    let digest = Sha256::digest(format!("{server_slug}__{original_name}").as_bytes());
    let short_hash: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    // 本函数按契约要兜住未经 normalize_slug 的调用方（见上方文档注释）：
    // slug 含多字节字符时按字节数截断会切在 char 边界内直接 panic，
    // 回退到边界上（哈希部分不变，唯一性不受影响）。
    let mut keep = MAX_TOOL_NAME_LEN - short_hash.len() - 1;
    while !candidate.is_char_boundary(keep) {
        keep -= 1;
    }
    format!("{}_{short_hash}", &candidate[..keep])
}

/// 候选名撞上 [`TOOL_PREFIX`] 时前面加一个不含 `_` 的短前缀避开。
/// `TOOL_PREFIX` 本身全由字母与 `__` 构成、不含连字符，加了 `srv-` 之后
/// 绝无可能又意外撞回同一个前缀（一次转换即彻底摆脱，不必递归检查）。
fn escape_reserved_prefix(candidate: String) -> String {
    if candidate.starts_with(TOOL_PREFIX) {
        format!("srv-{candidate}")
    } else {
        candidate
    }
}

pub fn schema_hash(input_schema: &serde_json::Value) -> String {
    let canonical = serde_json::to_string(input_schema).unwrap_or_default();
    Sha256::digest(canonical.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// alias 是否合法：非空、不超长、仅 ASCII 字母数字与 `_`/`-`、且不落进
/// [`TOOL_PREFIX`] 保留命名空间——否则用户能把任意下游工具的 alias 设成
/// `mcpane__echo`，制造一个和内建工具同名的暴露名：tools/list 里出现重复
/// 项，且该工具从此被内建路由永久挡住、再也调不到（见回归测试）。
fn is_valid_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias.len() <= MAX_TOOL_NAME_LEN
        && !alias.starts_with(TOOL_PREFIX)
        && alias
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// 参与构建 Catalog 的下游 Server 输入。
#[derive(Debug, Clone, Default)]
pub struct ServerTools {
    pub server_id: String,
    pub server_slug: String,
    /// 下游 initialize 返回的自述（原样保留，聚合展示时再截断）。
    pub instructions: Option<String>,
    pub tools: Vec<rmcp::model::Tool>,
    pub prompts: Vec<rmcp::model::Prompt>,
    pub resources: Vec<rmcp::model::Resource>,
    pub resource_templates: Vec<rmcp::model::ResourceTemplate>,
}

/// 单条覆盖的取值（tool/prompt 共用）——`build_revision` 的统一输入。
///
/// 从 `(bool, Option<String>)` 二元组改成具名结构体，是因为 tool 多出了两项
/// （触发提示、用户声明的注解）——再往元组里塞 `Option<_>`，读代码的人就只能
/// 靠位置猜含义了。
///
/// 注意三类字段的来源不在一个层级：`enabled`/`alias`/`description_suffix` 是
/// Workspace 级治理（`workspace_tools`），`declared_annotations` 是全局声明
/// （`tool_declared_annotations`，注解陈述的是工具行为、不按项目分），
/// 由 `load_overrides` 合并成这一个结构体。
#[derive(Debug, Clone, PartialEq)]
pub struct ItemOverride {
    pub enabled: bool,
    pub alias: Option<String>,
    /// 追加到下游描述之后的触发式提示（ADR-021 闸③）。None/空串 = 未配置。
    pub description_suffix: Option<String>,
    /// 用户声明的注解（全局来源，见结构体文档）。prompt 无此语义，恒为 None。
    pub declared_annotations: Option<DeclaredAnnotations>,
}

impl Default for ItemOverride {
    /// 「没有覆盖记录」的等价取值：启用、无别名、无提示、无声明。
    /// `build_revision` 的 `unwrap_or_default()` 直接吃这个默认。
    fn default() -> Self {
        Self {
            enabled: true,
            alias: None,
            description_suffix: None,
            declared_annotations: None,
        }
    }
}

impl ItemOverride {
    /// 禁用该条目。
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }

    /// 改暴露名，其余不变。
    pub fn alias(alias: impl Into<String>) -> Self {
        Self {
            alias: Some(alias.into()),
            ..Default::default()
        }
    }

    /// 给描述追加触发式提示，其余不变。
    pub fn suffix(suffix: impl Into<String>) -> Self {
        Self {
            description_suffix: Some(suffix.into()),
            ..Default::default()
        }
    }
}

/// Workspace 级 Tool/Prompt 覆盖。键为 `(server_id, original_name)`。
pub type ToolOverrides = BTreeMap<(String, String), ItemOverride>;

/// Workspace 级三类条目覆盖（构建 Catalog 的统一输入）。
/// - tools/prompts：禁用 + alias（同一命名空间规则）；
/// - resources：仅禁用，键为 `(server_id, uri)`——URI 不改写故无 alias。
#[derive(Debug, Clone, Default)]
pub struct CatalogOverrides {
    pub tools: ToolOverrides,
    pub prompts: ToolOverrides,
    pub resources: BTreeMap<(String, String), bool>,
}

impl CatalogOverrides {
    /// 仅 tools 覆盖的便捷构造（测试与旧调用面）。
    pub fn from_tools(tools: ToolOverrides) -> Self {
        Self {
            tools,
            ..Default::default()
        }
    }
}

/// 把用户配置的触发式提示接到下游描述之后。**追加不覆盖**——下游改了自己的
/// 描述，用户写的触发条件依然在；后缀为空则逐字节保留原描述。
/// 下游无描述时后缀独立成立：没有描述的工具恰恰最需要一句话说清何时该用。
fn append_suffix(description: Option<String>, suffix: Option<&str>) -> Option<String> {
    let suffix = suffix.map(str::trim).filter(|s| !s.is_empty());
    match (description, suffix) {
        (Some(desc), Some(suffix)) if desc.trim().is_empty() => Some(suffix.to_owned()),
        (Some(desc), Some(suffix)) => Some(format!("{desc}\n{suffix}")),
        (desc, None) => desc,
        (None, Some(suffix)) => Some(suffix.to_owned()),
    }
}

/// 构建 Catalog Revision：命名空间、三类覆盖、冲突检测。
pub fn build_revision(
    workspace_id: &str,
    servers: &[ServerTools],
    overrides: &CatalogOverrides,
) -> Result<CatalogRevision, CatalogError> {
    let mut tools: Vec<CatalogTool> = Vec::new();
    let mut seen: BTreeMap<String, String> = BTreeMap::new(); // exposed_name -> server_slug

    for server in servers {
        for tool in &server.tools {
            let key = (server.server_id.clone(), tool.name.to_string());
            let ItemOverride {
                enabled,
                alias,
                description_suffix,
                declared_annotations,
            } = overrides.tools.get(&key).cloned().unwrap_or_default();
            if !enabled {
                continue;
            }
            // 单个工具的命名问题不应拖垮整张 Catalog——聚合网关对下游 tools/list
            // 失败已是「缺席即可、不影响其余」，命名冲突/非法 alias 也照此处理：
            // 非法 alias 回退到默认暴露名（工具仍可用），冲突则跳过后者（先到先得），
            // 各记一条 warn 供诊断。避免一个坏工具让整个 Workspace 的 Catalog 冻结。
            let name = match alias {
                Some(alias) if is_valid_alias(&alias) => alias,
                Some(bad) => {
                    tracing::warn!(
                        server = %server.server_slug,
                        tool = %tool.name,
                        alias = %bad,
                        "alias 非法，回退到默认暴露名"
                    );
                    exposed_name(&server.server_slug, &tool.name)
                }
                None => exposed_name(&server.server_slug, &tool.name),
            };
            if let Some(other) = seen.get(&name) {
                tracing::warn!(
                    exposed_name = %name,
                    kept = %other,
                    skipped = %server.server_slug,
                    "暴露名冲突，跳过后者以保全其余 Catalog"
                );
                continue;
            }
            seen.insert(name.clone(), server.server_slug.clone());

            let mut input_schema = serde_json::to_value(tool.input_schema.clone())
                .unwrap_or_else(|_| serde_json::json!({"type": "object"}));
            // 规范化在入口做一次，tools/list、discovery、describe 全部出口受益。
            // 布尔 schema、顶层组合子等「规范合法但客户端吃不下」的形态会让
            // Claude Code 静默丢弃整张工具表甚至会话不可恢复（sanitize 模块注释）。
            for change in sanitize::sanitize_tool_schema(&mut input_schema) {
                tracing::warn!(
                    server = %server.server_slug,
                    tool = %tool.name,
                    change,
                    "下游 inputSchema 已规范化"
                );
            }
            let output_schema = tool
                .output_schema
                .as_ref()
                .and_then(|s| serde_json::to_value(s.clone()).ok())
                .map(|mut schema| {
                    for change in sanitize::sanitize_tool_schema(&mut schema) {
                        tracing::warn!(
                            server = %server.server_slug,
                            tool = %tool.name,
                            change,
                            "下游 outputSchema 已规范化"
                        );
                    }
                    schema
                });
            let downstream_annotations = tool
                .annotations
                .as_ref()
                .and_then(|a| serde_json::to_value(a.clone()).ok());
            // 声明只补空位、不覆盖下游：下游一旦自己申报，用户那份就自动让位
            // （ADR-002 Gate 的边界）。留痕在这里，用户才知道"我标的没生效"
            // 不是丢了，是下游后来自己说了。
            match (&downstream_annotations, &declared_annotations) {
                (None, Some(_)) => tracing::info!(
                    server = %server.server_slug,
                    tool = %tool.name,
                    "下游未申报注解，采用用户声明的注解"
                ),
                (Some(_), Some(_)) => tracing::info!(
                    server = %server.server_slug,
                    tool = %tool.name,
                    "下游已申报注解，用户声明未生效（原样透传下游）"
                ),
                _ => {}
            }
            tools.push(CatalogTool {
                server_id: server.server_id.clone(),
                server_slug: server.server_slug.clone(),
                original_name: tool.name.to_string(),
                exposed_name: name,
                description: append_suffix(
                    tool.description.as_ref().map(|d| d.to_string()),
                    description_suffix.as_deref(),
                ),
                description_suffix: description_suffix
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
                schema_hash: schema_hash(&input_schema),
                input_schema,
                output_schema,
                annotations: downstream_annotations,
                declared_annotations,
                icons: tool
                    .icons
                    .as_ref()
                    .and_then(|i| serde_json::to_value(i.clone()).ok()),
            });
        }
    }

    let mut prompts: Vec<CatalogPrompt> = Vec::new();
    let mut seen_prompts: BTreeMap<String, String> = BTreeMap::new();
    let mut resources: Vec<CatalogResource> = Vec::new();
    let mut denied_resource_uris: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    let mut seen_uris: BTreeMap<String, String> = BTreeMap::new();
    let mut resource_templates: Vec<CatalogResourceTemplate> = Vec::new();
    let mut seen_templates: BTreeMap<String, String> = BTreeMap::new();

    for server in servers {
        for prompt in &server.prompts {
            let key = (server.server_id.clone(), prompt.name.clone());
            // 只取 enabled/alias：prompt 由用户主动触发（slash command），描述影响的
            // 是人的挑选而非模型的自主选择；注解也无此语义。
            let ItemOverride { enabled, alias, .. } =
                overrides.prompts.get(&key).cloned().unwrap_or_default();
            if !enabled {
                continue;
            }
            // alias 规则与 tools 完全一致：非法回退默认名，冲突先到先得。
            let name = match alias {
                Some(alias) if is_valid_alias(&alias) => alias,
                Some(bad) => {
                    tracing::warn!(server = %server.server_slug, prompt = %prompt.name, alias = %bad,
                        "Prompt alias 非法，回退到默认暴露名");
                    exposed_name(&server.server_slug, &prompt.name)
                }
                None => exposed_name(&server.server_slug, &prompt.name),
            };
            if let Some(kept) = seen_prompts.get(&name) {
                tracing::warn!(exposed_name = %name, kept = %kept, skipped = %server.server_slug,
                    "Prompt 暴露名冲突，跳过后者");
                continue;
            }
            let Ok(raw) = serde_json::to_value(prompt) else {
                tracing::warn!(server = %server.server_slug, prompt = %prompt.name,
                    "Prompt 序列化失败，本条缺席 Catalog");
                continue;
            };
            seen_prompts.insert(name.clone(), server.server_slug.clone());
            prompts.push(CatalogPrompt {
                server_id: server.server_id.clone(),
                server_slug: server.server_slug.clone(),
                original_name: prompt.name.clone(),
                exposed_name: name,
                raw,
            });
        }
        for resource in &server.resources {
            let uri = resource.uri.clone();
            let key = (server.server_id.clone(), uri.clone());
            if overrides.resources.get(&key) == Some(&false) {
                // 记入禁用集：read_resource 回落 template 时据此拒绝，
                // 防止禁用被同源 template 字面前缀绕过。
                denied_resource_uris.insert(uri);
                continue;
            }
            if let Some(kept) = seen_uris.get(&uri) {
                tracing::warn!(uri = %uri, kept = %kept, skipped = %server.server_slug,
                    "Resource URI 冲突，跳过后者（URI 不改写，先到先得）");
                continue;
            }
            let Ok(raw) = serde_json::to_value(resource) else {
                tracing::warn!(server = %server.server_slug, uri = %uri,
                    "Resource 序列化失败，本条缺席 Catalog");
                continue;
            };
            seen_uris.insert(uri.clone(), server.server_slug.clone());
            resources.push(CatalogResource {
                server_id: server.server_id.clone(),
                server_slug: server.server_slug.clone(),
                uri,
                raw,
            });
        }
        for template in &server.resource_templates {
            let uri_template = template.uri_template.clone();
            if let Some(kept) = seen_templates.get(&uri_template) {
                tracing::warn!(uri_template = %uri_template, kept = %kept, skipped = %server.server_slug,
                    "Resource Template 冲突，跳过后者");
                continue;
            }
            let Ok(raw) = serde_json::to_value(template) else {
                tracing::warn!(server = %server.server_slug, uri_template = %uri_template,
                    "Resource Template 序列化失败，本条缺席 Catalog");
                continue;
            };
            seen_templates.insert(uri_template.clone(), server.server_slug.clone());
            resource_templates.push(CatalogResourceTemplate {
                server_id: server.server_id.clone(),
                server_slug: server.server_slug.clone(),
                uri_template,
                raw,
            });
        }
    }

    let mut revision = CatalogRevision::new(
        format!("rev_{}", uuid::Uuid::new_v4().simple()),
        workspace_id.to_owned(),
        tools,
    )
    .with_prompts_and_resources(prompts, resources, resource_templates, denied_resource_uris);
    revision.servers = servers
        .iter()
        .map(|s| CatalogServer {
            server_id: s.server_id.clone(),
            server_slug: s.server_slug.clone(),
            instructions: s.instructions.clone(),
        })
        .collect();
    Ok(revision)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn tool(name: &str) -> rmcp::model::Tool {
        rmcp::model::Tool::new(
            name.to_owned(),
            "test tool",
            std::sync::Arc::new(serde_json::Map::from_iter([(
                "type".to_owned(),
                serde_json::json!("object"),
            )])),
        )
    }

    fn server(id: &str, slug: &str, names: &[&str]) -> ServerTools {
        ServerTools {
            server_id: id.into(),
            server_slug: slug.into(),
            tools: names.iter().map(|n| tool(n)).collect(),
            ..Default::default()
        }
    }

    /// 下游 schema 在入库时规范化：布尔 properties 成员与顶层组合子这类
    /// 「规范合法但客户端吃不下」的形态不得流到任何出口（ADR-020）。
    #[test]
    fn downstream_schemas_are_sanitized_at_ingestion() {
        let dirty = rmcp::model::Tool::new(
            "dirty".to_owned(),
            "带脏 schema 的下游工具",
            std::sync::Arc::new(
                serde_json::json!({
                    "anyOf": [{"required": ["id"]}, {"required": ["id", "when"]}],
                    "properties": {"id": {"type": "string"}, "free": true}
                })
                .as_object()
                .cloned()
                .unwrap(),
            ),
        );
        let revision = build_revision(
            "ws1",
            &[ServerTools {
                server_id: "s1".into(),
                server_slug: "demo".into(),
                tools: vec![dirty],
                ..Default::default()
            }],
            &CatalogOverrides::default(),
        )
        .unwrap();
        let schema = &revision.route("demo__dirty").unwrap().input_schema;
        assert_eq!(schema["properties"]["free"], serde_json::json!({}));
        assert!(schema.get("anyOf").is_none(), "{schema}");
        assert_eq!(schema["required"], serde_json::json!(["id"]));
        assert_eq!(schema["type"], "object");
    }

    /// firecrawl 等下游的 input/outputSchema 都声明 draft-07：原样转发会让客户端在
    /// 调用前就以「方言不受支持」整工具拒收，入库必须摘掉（出口全部取自 Catalog）。
    #[test]
    fn draft07_dialect_never_reaches_any_exit() {
        let object =
            |value: serde_json::Value| std::sync::Arc::new(value.as_object().cloned().unwrap());
        let mut scrape = rmcp::model::Tool::new(
            "scrape".to_owned(),
            "带 draft-07 声明的下游工具",
            object(serde_json::json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {"url": {"type": "string"}}
            })),
        );
        scrape.output_schema = Some(object(serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {"markdown": {"type": "string"}}
        })));
        let revision = build_revision(
            "ws1",
            &[ServerTools {
                server_id: "s1".into(),
                server_slug: "demo".into(),
                tools: vec![scrape],
                ..Default::default()
            }],
            &CatalogOverrides::default(),
        )
        .unwrap();
        let entry = revision.route("demo__scrape").unwrap();
        assert!(entry.input_schema.get("$schema").is_none());
        let output = entry.output_schema.as_ref().unwrap();
        assert!(output.get("$schema").is_none());
        assert_eq!(output["properties"]["markdown"]["type"], "string");
    }

    #[test]
    fn same_named_tools_get_distinct_namespaces() {
        let revision = build_revision(
            "ws1",
            &[
                server("s1", "github", &["search"]),
                server("s2", "filesystem", &["search"]),
            ],
            &CatalogOverrides::default(),
        )
        .unwrap();
        let names: Vec<_> = revision
            .tools
            .iter()
            .map(|t| t.exposed_name.as_str())
            .collect();
        assert_eq!(names, vec!["github__search", "filesystem__search"]);
        assert_eq!(revision.route("github__search").unwrap().server_id, "s1");
        assert_eq!(
            revision.route("filesystem__search").unwrap().server_id,
            "s2"
        );
    }

    #[test]
    fn overlong_names_truncate_with_stable_hash() {
        let long = "a".repeat(200);
        let n1 = exposed_name("srv", &long);
        let n2 = exposed_name("srv", &long);
        assert_eq!(n1, n2, "哈希必须稳定");
        assert!(n1.len() <= MAX_TOOL_NAME_LEN);
        let similar = format!("{}b", "a".repeat(199));
        assert_ne!(n1, exposed_name("srv", &similar), "不同输入不得截断碰撞");
    }

    #[test]
    fn illegal_chars_are_sanitized() {
        assert_eq!(exposed_name("srv", "my.tool/v2"), "srv__my_tool_v2");
        assert_eq!(normalize_slug("My Server (Dev)!"), "my-server-dev");
        assert_eq!(normalize_slug("___"), "server");
    }

    /// 描述增强层（ADR-020 闸③）：追加不覆盖，且不碰 schema。
    #[test]
    fn description_suffix_appends_without_replacing_or_touching_schema() {
        let servers = [server("s1", "github", &["search"])];
        let bare = build_revision("ws1", &servers, &CatalogOverrides::default()).unwrap();

        let mut overrides: ToolOverrides = BTreeMap::new();
        overrides.insert(
            ("s1".into(), "search".into()),
            ItemOverride::suffix("Use when: 需要检索仓库时。关键词：搜索、仓库"),
        );
        let enriched =
            build_revision("ws1", &servers, &CatalogOverrides::from_tools(overrides)).unwrap();

        let original = bare.tools[0].description.clone().unwrap_or_default();
        let after = enriched.tools[0].description.clone().unwrap();
        assert!(
            after.starts_with(&original),
            "下游原描述必须原样打头：{after:?}"
        );
        assert!(after.contains("关键词：搜索、仓库"), "{after:?}");
        assert_eq!(
            bare.tools[0].schema_hash, enriched.tools[0].schema_hash,
            "描述不参与 schema，加后缀不得改变 schema_hash"
        );
        assert_eq!(
            bare.tools[0].exposed_name, enriched.tools[0].exposed_name,
            "后缀不该影响暴露名"
        );
    }

    /// 没配后缀就一个字节都别动——增强层默认必须是透明的。
    #[test]
    fn empty_suffix_leaves_description_byte_identical() {
        let servers = [server("s1", "github", &["search"])];
        let bare = build_revision("ws1", &servers, &CatalogOverrides::default()).unwrap();

        for blank in ["", "   ", "\n"] {
            let mut overrides: ToolOverrides = BTreeMap::new();
            overrides.insert(("s1".into(), "search".into()), ItemOverride::suffix(blank));
            let same =
                build_revision("ws1", &servers, &CatalogOverrides::from_tools(overrides)).unwrap();
            assert_eq!(
                bare.tools[0].description, same.tools[0].description,
                "空后缀（{blank:?}）不得改动描述"
            );
        }
    }

    /// 目录渲染要拿得到「不含提示的下游自述」——拿不到就会把拼好的整体当自述，
    /// 截断后残留提示开头、后面再完整追加一遍（真机在 gateway 目录里抓到）。
    #[test]
    fn downstream_description_strips_the_appended_hint() {
        let servers = [server("s1", "github", &["search"])];
        let bare = build_revision("ws1", &servers, &CatalogOverrides::default()).unwrap();
        let original = bare.tools[0].description.clone();

        let mut overrides: ToolOverrides = BTreeMap::new();
        overrides.insert(
            ("s1".into(), "search".into()),
            ItemOverride::suffix("Use when: 需要检索仓库时。关键词：搜索、仓库"),
        );
        let enriched =
            build_revision("ws1", &servers, &CatalogOverrides::from_tools(overrides)).unwrap();
        let tool = &enriched.tools[0];

        assert_eq!(
            tool.downstream_description(),
            original.as_deref(),
            "剥离后应逐字节等于下游原描述"
        );
        assert!(
            !tool.downstream_description().unwrap().contains("Use when"),
            "提示不该残留在下游自述里：{:?}",
            tool.downstream_description()
        );
        // 没配提示时原样返回。
        assert_eq!(bare.tools[0].downstream_description(), original.as_deref());
    }

    /// 下游没写描述时，后缀独立成立：这种工具最需要一句「何时该用」。
    #[test]
    fn suffix_stands_alone_when_downstream_has_no_description() {
        assert_eq!(
            super::append_suffix(None, Some("Use when: X")),
            Some("Use when: X".to_owned())
        );
        assert_eq!(
            super::append_suffix(Some(String::new()), Some("Use when: X")),
            Some("Use when: X".to_owned()),
            "空描述不该留下一个孤零零的换行"
        );
        assert_eq!(super::append_suffix(None, None), None);
    }

    #[test]
    fn alias_applies_and_conflict_rejected() {
        let mut overrides: ToolOverrides = BTreeMap::new();
        overrides.insert(
            ("s1".into(), "search".into()),
            ItemOverride::alias("gh_search"),
        );
        let revision = build_revision(
            "ws1",
            &[server("s1", "github", &["search"])],
            &CatalogOverrides::from_tools(overrides),
        )
        .unwrap();
        assert_eq!(revision.tools[0].exposed_name, "gh_search");

        // alias 撞了另一个 Server 的默认名 → 不再中止整张 Catalog：先到者（github
        // 的 alias）保留该暴露名，后到的 filesystem__search 被跳过。
        let mut overrides: ToolOverrides = BTreeMap::new();
        overrides.insert(
            ("s1".into(), "search".into()),
            ItemOverride::alias("filesystem__search"),
        );
        let revision = build_revision(
            "ws1",
            &[
                server("s1", "github", &["search"]),
                server("s2", "filesystem", &["search"]),
            ],
            &CatalogOverrides::from_tools(overrides),
        )
        .unwrap();
        assert_eq!(revision.tools.len(), 1, "冲突时跳过后者而非整张失败");
        assert_eq!(revision.tools[0].exposed_name, "filesystem__search");
        assert_eq!(revision.tools[0].server_slug, "github", "先到者保留该名");

        // 非法 alias → 回退到默认暴露名，工具仍在（不因此中止整张 Catalog）。
        let mut overrides: ToolOverrides = BTreeMap::new();
        overrides.insert(
            ("s1".into(), "search".into()),
            ItemOverride::alias("bad name!"),
        );
        let revision = build_revision(
            "ws1",
            &[server("s1", "github", &["search"])],
            &CatalogOverrides::from_tools(overrides),
        )
        .unwrap();
        assert_eq!(revision.tools.len(), 1);
        assert_eq!(
            revision.tools[0].exposed_name, "github__search",
            "非法 alias 回退默认名"
        );
    }

    // reserved_namespace 与 TOOL_PREFIX 的同源守卫随实现下沉到
    // mcpane-hub-protocol 的测试里（reserved_namespace_strips_prefix_separator）。

    /// 回归：未经 normalize_slug 的多字节 slug + 超长名，截断必须落在
    /// char 边界上而不是 panic（本函数契约明说要兜住不老实的调用方）。
    #[test]
    fn exposed_name_truncates_multibyte_slug_on_char_boundary() {
        let slug = "工具服务器一二三四五六七八九十甲乙丙丁戊己庚辛壬癸";
        let long_name = "t".repeat(120);
        let name = exposed_name(slug, &long_name);
        assert!(name.len() <= MAX_TOOL_NAME_LEN, "实际 {}", name.len());
    }

    /// 回归：alias 不得让暴露名落进 `mcpane__` 保留前缀。
    ///
    /// 这个前缀是 Hub 内建工具（`mcpane__echo` / `mcpane__tool_discovery` /
    /// `mcpane__tool_execute` / `mcpane__connection_info`，见
    /// `binaries/mcpane-hub/src/hub.rs` 的 `TOOL_PREFIX`，现直接 re-export
    /// 自本 crate）冻结占用的命名空间（ADR-006/ADR-011）。修复前
    /// `is_valid_alias` 只校验字符集/长度，从不检查是否撞上这个保留
    /// 前缀——catalog crate 本身并不知道 `TOOL_PREFIX` 的存在（当时两处
    /// 是分离定义，互不知情）。一旦用户在 Workspace 治理里把某个下游工具的
    /// alias 设成 `mcpane__echo`，`hub.rs::list_tools` 会把内建与下游两个同名
    /// `mcpane__echo` 一起塞进 tools/list（违反 MCP 工具名唯一性），而
    /// `call_tool` 里内建路由永远优先命中，撞名的下游工具从此永久不可调用、
    /// 也没有任何报错提示。
    #[test]
    fn alias_cannot_land_in_the_reserved_builtin_namespace() {
        let mut overrides: ToolOverrides = BTreeMap::new();
        overrides.insert(
            ("s1".into(), "search".into()),
            ItemOverride::alias("mcpane__echo"),
        );
        let revision = build_revision(
            "ws1",
            &[server("s1", "github", &["search"])],
            &CatalogOverrides::from_tools(overrides),
        )
        .unwrap();

        assert!(
            revision
                .tools
                .iter()
                .all(|t| !t.exposed_name.starts_with("mcpane__")),
            "alias 不得把暴露名落进保留前缀 `mcpane__`，实际: {:?}",
            revision
                .tools
                .iter()
                .map(|t| &t.exposed_name)
                .collect::<Vec<_>>()
        );
    }

    /// 回归：即使没有任何 alias override，纯默认命名也不能落进保留前缀。
    /// 一个下游 Server 若被起名 "MCPane"（或近似写法），
    /// [`normalize_slug`] 会把它归一成 "mcpane"——`exposed_name("mcpane",
    /// "echo")` 不做特殊处理的话恰好就是 `mcpane__echo`，与 alias 撞名是
    /// 同一个后果，只是不需要用户手动配置 alias 就能触发。
    #[test]
    fn default_naming_escapes_reserved_prefix_even_without_alias() {
        let revision = build_revision(
            "ws1",
            &[server("s1", "mcpane", &["echo", "search"])],
            &CatalogOverrides::default(),
        )
        .unwrap();
        assert_eq!(revision.tools.len(), 2);
        for tool in &revision.tools {
            assert!(
                !tool.exposed_name.starts_with(TOOL_PREFIX),
                "默认命名也不得落进保留前缀：{}",
                tool.exposed_name
            );
        }
        // 具体形态可预期、可复现——不是随手糊弄，一次转换就彻底摆脱前缀。
        assert_eq!(
            revision.route("srv-mcpane__echo").unwrap().original_name,
            "echo"
        );
    }

    /// 回归：[`normalize_slug`] 单独调用时同样不能产出裸的保留命名空间字，
    /// 这是暴露名之外的第二道防线——万一未来某处直接拿归一化后的 slug
    /// 做别的用途（不经过 `exposed_name`），也不会悄悄传回 `"mcpane"`。
    #[test]
    fn normalize_slug_escapes_the_reserved_namespace_word() {
        // 下面故意把品牌名的错误大小写当作**被测输入**：用户给下游 Server
        // 起什么名字不受品牌规范约束，归一化必须对各种写法都兜住。这不是
        // 对外文案，故对该字面量所在的那一行（且仅那一行）豁免品牌扫描。
        let cased_variants = ["MCPane", "mcpane", "  mcpane  ", "McPane"]; // brand-scan: allow-prohibited-spellings
        for input in cased_variants {
            let slug = normalize_slug(input);
            assert_ne!(slug, "mcpane", "input = {input:?} 不得归一化成裸的保留字");
        }
        // 无关 slug 不受影响。
        assert_eq!(normalize_slug("GitHub"), "github");
    }

    /// 核心保障：一个命名冲突不得殃及无关工具——其余工具必须照常进 Catalog。
    #[test]
    fn name_conflict_skips_only_offender_keeps_rest() {
        // s1 与 s2 都把各自的 search 用 alias 撞到 "dup"；s2 另有一个无关的 build。
        let mut overrides: ToolOverrides = BTreeMap::new();
        overrides.insert(("s1".into(), "search".into()), ItemOverride::alias("dup"));
        overrides.insert(("s2".into(), "search".into()), ItemOverride::alias("dup"));
        let revision = build_revision(
            "ws1",
            &[
                server("s1", "github", &["search"]),
                server("s2", "ci", &["search", "build"]),
            ],
            &CatalogOverrides::from_tools(overrides),
        )
        .unwrap();
        let names: Vec<&str> = revision
            .tools
            .iter()
            .map(|t| t.exposed_name.as_str())
            .collect();
        assert!(names.contains(&"dup"), "先到者保留");
        assert!(names.contains(&"ci__build"), "无关工具不受冲突牵连");
        assert_eq!(revision.tools.len(), 2, "只跳过肇事的那一个");
    }

    #[test]
    fn prompts_get_namespaced_and_conflict_skips_later() {
        let mut s1 = server("s1", "github", &[]);
        s1.prompts = vec![rmcp::model::Prompt::new(
            "review",
            Some("code review"),
            None,
        )];
        let mut s2 = server("s2", "gitlab", &[]);
        s2.prompts = vec![rmcp::model::Prompt::new("review", None::<String>, None)];

        let revision = build_revision("ws1", &[s1, s2], &CatalogOverrides::default()).unwrap();
        let names: Vec<&str> = revision
            .prompts
            .iter()
            .map(|p| p.exposed_name.as_str())
            .collect();
        assert_eq!(names, vec!["github__review", "gitlab__review"]);
        let hit = revision.route_prompt("github__review").unwrap();
        assert_eq!(hit.server_id, "s1");
        assert_eq!(hit.original_name, "review");
        assert_eq!(hit.raw["description"], "code review", "raw 保真透传");
        assert!(
            revision.route_prompt("review").is_none(),
            "未命名空间的名字不可路由"
        );
    }

    #[test]
    fn resource_uris_pass_through_and_dedupe_first_wins() {
        let mut s1 = server("s1", "fs-a", &[]);
        s1.resources = vec![rmcp::model::Resource::new(
            "file:///shared/readme.md",
            "readme",
        )];
        let mut s2 = server("s2", "fs-b", &[]);
        s2.resources = vec![
            rmcp::model::Resource::new("file:///shared/readme.md", "readme-dup"),
            rmcp::model::Resource::new("file:///b/only.txt", "only"),
        ];

        let revision = build_revision("ws1", &[s1, s2], &CatalogOverrides::default()).unwrap();
        assert_eq!(revision.resources.len(), 2, "冲突 URI 先到先得");
        let hit = revision.route_resource("file:///shared/readme.md").unwrap();
        assert_eq!(hit.server_id, "s1", "先到者保留");
        assert_eq!(hit.uri, "file:///shared/readme.md", "URI 不改写");
        assert_eq!(
            revision
                .route_resource("file:///b/only.txt")
                .unwrap()
                .server_id,
            "s2"
        );
    }

    #[test]
    fn resource_template_routes_by_longest_literal_prefix() {
        let mut s1 = server("s1", "fs", &[]);
        s1.resource_templates = vec![rmcp::model::ResourceTemplate::new(
            "file:///logs/{name}",
            "logs",
        )];
        let mut s2 = server("s2", "fs-deep", &[]);
        s2.resource_templates = vec![rmcp::model::ResourceTemplate::new(
            "file:///logs/archive/{name}",
            "archived",
        )];

        let revision = build_revision("ws1", &[s1, s2], &CatalogOverrides::default()).unwrap();
        let hit = revision
            .route_resource_template("file:///logs/archive/2026.log")
            .unwrap();
        assert_eq!(hit.server_id, "s2", "最长字面前缀胜出");
        let hit = revision
            .route_resource_template("file:///logs/today.log")
            .unwrap();
        assert_eq!(hit.server_id, "s1");
        assert!(
            revision
                .route_resource_template("https://other/x")
                .is_none(),
            "前缀不匹配则不路由"
        );
    }

    fn declared() -> DeclaredAnnotations {
        DeclaredAnnotations {
            read_only_hint: true,
            destructive_hint: false,
            idempotent_hint: true,
            open_world_hint: false,
        }
    }

    fn with_declaration(server_id: &str, name: &str) -> CatalogOverrides {
        let mut overrides = CatalogOverrides::default();
        overrides.tools.insert(
            (server_id.into(), name.into()),
            ItemOverride {
                declared_annotations: Some(declared()),
                ..Default::default()
            },
        );
        overrides
    }

    /// 序列化契约：桌面端与上游客户端都按这几个键名读，改名即静默失效。
    ///
    /// 键名错了不会有任何失败信号——桌面端读不到 `declaredAnnotations` 就永远
    /// 显示"无注解"，客户端读不到 `readOnlyHint` 就当没申报，两边都不报错。
    /// 这几个字符串同时是 `apps/desktop/src/lib/ipc.ts` 里 `CatalogTool` /
    /// `DeclaredAnnotations` 两个 interface 的字段名，改一边必须改两边。
    #[test]
    fn catalog_tool_json_keys_match_the_desktop_and_mcp_contract() {
        let revision = build_revision(
            "ws1",
            &[server("s1", "gh", &["search"])],
            &with_declaration("s1", "search"),
        )
        .unwrap();
        let value = serde_json::to_value(revision.route("gh__search").unwrap()).unwrap();
        let object = value.as_object().unwrap();

        for key in [
            "serverId",
            "serverSlug",
            "originalName",
            "exposedName",
            "description",
            "inputSchema",
            "annotations",
            "declaredAnnotations",
            "schemaHash",
        ] {
            assert!(
                object.contains_key(key),
                "CatalogTool 缺少键 {key}：{value}"
            );
        }

        // MCP 规范的 ToolAnnotations 字段名——这份 JSON 会直接进 tools/list。
        assert_eq!(
            value["declaredAnnotations"],
            serde_json::json!({
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false
            })
        );
    }

    /// Hub 自己永远不编注解：下游没申报、用户也没声明，出口就是没有。
    ///
    /// 这条是 ADR-002 Gate 的原判，别的两条都是围着它划边界的。
    #[test]
    fn hub_never_fabricates_annotations() {
        let revision = build_revision(
            "ws1",
            &[server("s1", "gh", &["search"])],
            &Default::default(),
        )
        .unwrap();
        let tool = revision.route("gh__search").unwrap();
        assert_eq!(tool.annotations, None);
        assert_eq!(tool.declared_annotations, None);
        assert!(tool.effective_annotations().is_none());
    }

    /// 下游没申报时，用户声明的注解补上空位，并且认得出是用户标的。
    #[test]
    fn user_declaration_fills_the_gap_when_downstream_is_silent() {
        let revision = build_revision(
            "ws1",
            &[server("s1", "gh", &["search"])],
            &with_declaration("s1", "search"),
        )
        .unwrap();
        let tool = revision.route("gh__search").unwrap();
        assert_eq!(tool.annotations, None, "下游那一栏必须仍然是空的");

        let (value, source) = tool.effective_annotations().unwrap();
        assert_eq!(source, AnnotationSource::User);
        assert_eq!(value["readOnlyHint"], serde_json::json!(true));
        assert_eq!(value["openWorldHint"], serde_json::json!(false));
    }

    /// 交界守卫：触发提示与用户声明的注解是两套独立机制（前者按工作区存在
    /// `workspace_tools`，后者全局存在 `tool_declared_annotations`），同一个
    /// 工具上同时配置时必须互不干扰。
    ///
    /// 两者在 `ItemOverride` 里挤成了一个结构体——合并两条分支时最容易在这里
    /// 让一方吃掉另一方（`..Default::default()` 少写一个字段就够了），而这种
    /// 错误单独测任一功能都发现不了。
    #[test]
    fn trigger_hint_and_declared_annotations_coexist_on_one_tool() {
        let servers = [server("s1", "gh", &["search"])];
        let mut overrides = CatalogOverrides::default();
        overrides.tools.insert(
            ("s1".into(), "search".into()),
            ItemOverride {
                description_suffix: Some("Use when: 需要检索仓库时".into()),
                declared_annotations: Some(declared()),
                ..Default::default()
            },
        );
        let revision = build_revision("ws1", &servers, &overrides).unwrap();
        let tool = revision.route("gh__search").unwrap();

        // 描述增强这一路照常生效。
        let desc = tool.description.as_deref().unwrap_or_default();
        assert!(
            desc.contains("Use when: 需要检索仓库时"),
            "注解声明不该吃掉触发提示：{desc:?}"
        );
        assert_eq!(
            tool.description_suffix.as_deref(),
            Some("Use when: 需要检索仓库时")
        );
        assert!(
            !tool.downstream_description().unwrap().contains("Use when"),
            "剥离仍要能拿到干净的下游自述"
        );

        // 注解声明这一路也照常生效——拿不到就说明触发提示把它吃了。
        let (value, source) = tool.effective_annotations().unwrap();
        assert_eq!(source, AnnotationSource::User);
        assert_eq!(value["readOnlyHint"], serde_json::json!(true));

        // 两者都不该动 schema_hash（路由同一性）。
        let bare = build_revision("ws1", &servers, &CatalogOverrides::default()).unwrap();
        assert_eq!(bare.tools[0].schema_hash, tool.schema_hash);
    }

    /// **不伪造下游**：下游自己申报了注解，用户声明一律让位，出口原样透传下游。
    ///
    /// 这是 ADR-002 Gate 开这个口子时划的边界——补空位可以，改口径不行。
    /// 反向验证：把 `effective_annotations` 的优先级颠倒过来，这条必须转红。
    #[test]
    fn downstream_annotations_are_never_overridden_by_user_declaration() {
        let mut annotated = tool("search");
        // ToolAnnotations 是 non_exhaustive，只能从默认值改起。
        let mut downstream = rmcp::model::ToolAnnotations::default();
        downstream.read_only_hint = Some(false);
        downstream.destructive_hint = Some(true);
        annotated.annotations = Some(downstream);
        let servers = [ServerTools {
            server_id: "s1".into(),
            server_slug: "gh".into(),
            tools: vec![annotated],
            ..Default::default()
        }];

        let revision = build_revision("ws1", &servers, &with_declaration("s1", "search")).unwrap();
        let tool = revision.route("gh__search").unwrap();
        // 两份都留在目录里：谁说的始终分得清，用户才知道自己那份为何没生效。
        assert_eq!(tool.declared_annotations, Some(declared()));

        let (value, source) = tool.effective_annotations().unwrap();
        assert_eq!(source, AnnotationSource::Downstream);
        assert_eq!(
            value["readOnlyHint"],
            serde_json::json!(false),
            "下游说 false 就得是 false——用户声明的 true 不许盖过去"
        );
    }

    /// Prompt 治理：禁用即缺席，alias 改写暴露名（规则与 tools 同）。
    #[test]
    fn prompt_overrides_disable_and_alias() {
        let mut s1 = server("s1", "github", &[]);
        s1.prompts = vec![
            rmcp::model::Prompt::new("review", None::<String>, None),
            rmcp::model::Prompt::new("noisy", None::<String>, None),
        ];
        let mut overrides = CatalogOverrides::default();
        overrides
            .prompts
            .insert(("s1".into(), "noisy".into()), ItemOverride::disabled());
        overrides
            .prompts
            .insert(("s1".into(), "review".into()), ItemOverride::alias("cr"));

        let revision = build_revision("ws1", &[s1], &overrides).unwrap();
        let names: Vec<&str> = revision
            .prompts
            .iter()
            .map(|p| p.exposed_name.as_str())
            .collect();
        assert_eq!(names, vec!["cr"], "noisy 缺席、review 用 alias");
        assert_eq!(revision.route_prompt("cr").unwrap().original_name, "review");
    }

    /// Resource 治理：仅禁用（URI 不改写，无 alias）。
    #[test]
    fn resource_overrides_disable_by_uri() {
        let mut s1 = server("s1", "fs", &[]);
        s1.resources = vec![
            rmcp::model::Resource::new("mem://a", "a"),
            rmcp::model::Resource::new("mem://b", "b"),
        ];
        let mut overrides = CatalogOverrides::default();
        overrides
            .resources
            .insert(("s1".into(), "mem://b".into()), false);

        let revision = build_revision("ws1", &[s1], &overrides).unwrap();
        assert_eq!(revision.resources.len(), 1);
        assert!(revision.route_resource("mem://a").is_some());
        assert!(revision.route_resource("mem://b").is_none(), "禁用即缺席");
    }

    /// 安全回归：禁用一个同时被 template 前缀覆盖的静态 resource——
    /// 它从 route_resource 缺席（因被禁），却仍落在 template 字面前缀内；
    /// is_resource_denied 必须返回 true，供 read_resource 在回落 template
    /// 前拦截（否则禁用被同源 template 兜底放行）。
    #[test]
    fn denied_resource_uri_blocks_template_fallback() {
        let mut s1 = server("s1", "fs", &[]);
        s1.resources = vec![rmcp::model::Resource::new("file:///secret", "secret")];
        s1.resource_templates = vec![rmcp::model::ResourceTemplate::new("file:///{path}", "any")];
        let mut overrides = CatalogOverrides::default();
        overrides
            .resources
            .insert(("s1".into(), "file:///secret".into()), false);

        let revision = build_revision("ws1", &[s1], &overrides).unwrap();
        // 静态条目已缺席，但 template 前缀 "file:///" 仍覆盖它——绕过通道存在。
        assert!(revision.route_resource("file:///secret").is_none());
        assert!(
            revision.route_resource_template("file:///secret").is_some(),
            "前提：template 前缀确实覆盖被禁 URI（否则测不到绕过）"
        );
        // 拦截点：禁用集必须命中。
        assert!(
            revision.is_resource_denied("file:///secret"),
            "被禁 URI 必须进禁用集，read_resource 据此拒绝回落"
        );
        assert!(
            !revision.is_resource_denied("file:///other"),
            "未禁的不受影响"
        );
    }

    #[test]
    fn disabled_tools_are_filtered_out() {
        let mut overrides: ToolOverrides = BTreeMap::new();
        overrides.insert(("s1".into(), "dangerous".into()), ItemOverride::disabled());
        let revision = build_revision(
            "ws1",
            &[server("s1", "github", &["search", "dangerous"])],
            &CatalogOverrides::from_tools(overrides),
        )
        .unwrap();
        assert_eq!(revision.tools.len(), 1);
        assert_eq!(revision.tools[0].original_name, "search");
    }
}
