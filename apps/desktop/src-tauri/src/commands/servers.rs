//! MCP Server 管理（TASK-503 数据层）。

use super::hub::{control_call, require_ok};
use mcpane_hub_protocol::ControlCommand;

// ---------- MCP Server 管理（TASK-503 数据层） ----------

pub(crate) async fn server_repo() -> Result<mcpane_storage::ServerRepo, String> {
    Ok(mcpane_storage::ServerRepo::new(super::db_pool().await?))
}

#[tauri::command]
pub async fn servers_list() -> Result<Vec<mcpane_storage::McpServerDef>, String> {
    server_repo().await?.list().await.map_err(|e| e.to_string())
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInput {
    pub id: Option<String>,
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: std::collections::BTreeMap<String, String>,
    pub cwd: Option<String>,
    pub url: Option<String>,
    /// Bearer Token 直填值（优先于 bearer_env）。
    #[serde(default)]
    pub bearer: Option<String>,
    pub bearer_env: Option<String>,
    /// 自定义 HTTP 请求头。
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
    /// None = 沿用现状（编辑保留原值、新建默认启用）。
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// 解析 transport 并校验其必填字段（纯函数，便于单测）。
/// 必填字段在入库处拦截：stdio 必须有启动命令、http/sse 必须有地址。
/// 缺字段若放行入库，错误会推迟到 Hub spawn 时才炸，且更难定位。
fn validated_transport(input: &ServerInput) -> Result<mcpane_storage::ServerTransport, String> {
    let transport = match input.transport.as_str() {
        "stdio" => mcpane_storage::ServerTransport::Stdio,
        "http" => mcpane_storage::ServerTransport::Http,
        "sse" => mcpane_storage::ServerTransport::Sse,
        other => return Err(format!("未知 transport: {other}")),
    };
    match transport {
        mcpane_storage::ServerTransport::Stdio => {
            if input
                .command
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
            {
                return Err("stdio 传输必须提供启动命令（command）".into());
            }
        }
        mcpane_storage::ServerTransport::Http | mcpane_storage::ServerTransport::Sse => {
            if input.url.as_deref().map(str::trim).unwrap_or("").is_empty() {
                return Err("HTTP/SSE 传输必须提供服务地址（url）".into());
            }
        }
    }
    Ok(transport)
}

/// 新增/编辑下游 Server 定义，随后通知 Hub reload。
/// 编辑（id 非空）时保留原 slug/enabled：slug 是客户端可见的工具前缀，
/// 改名不静默变更；编辑也不应重置启用状态。
#[tauri::command]
pub async fn server_upsert(app: tauri::AppHandle, input: ServerInput) -> Result<(), String> {
    let transport = validated_transport(&input)?;
    let repo = server_repo().await?;
    let existing = match input.id.as_deref().filter(|id| !id.is_empty()) {
        Some(id) => Some(
            repo.get(id)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("服务器不存在: {id}"))?,
        ),
        None => None,
    };
    // 新建时拦截 slug 冲突：重复 slug 会让工具前缀撞车，catalog 层会静默丢工具。
    let slug = match &existing {
        Some(e) => e.slug.clone(),
        None => {
            // slug 规则唯一实现在 mcpane-hub-protocol（与 hub 侧 Catalog 同源）。
            // 此前这里有份"最小复刻"，恰恰漏了 mcpane 保留字转义——用户把
            // Server 起名 "mcpane" 会产出撞上内建工具前缀的 slug。
            let slug = mcpane_hub_protocol::normalize_slug(&input.name);
            let taken = repo
                .list()
                .await
                .map_err(|e| e.to_string())?
                .into_iter()
                .any(|d| d.slug == slug);
            if taken {
                // 按 slug 而非名称判重：不同名称（如 "My Server" / "my-server"）
                // 也可能规范化出相同前缀，文案不说「同名」以免用户困惑。
                return Err(format!(
                    "工具前缀 {slug}__ 已被其他服务器占用（相近名称会生成相同前缀），请换个名称"
                ));
            }
            slug
        }
    };
    let def = mcpane_storage::McpServerDef {
        id: existing.as_ref().map(|e| e.id.clone()).unwrap_or_default(),
        slug,
        name: input.name,
        transport,
        command: input.command,
        args: input.args,
        env: input.env,
        cwd: input.cwd.filter(|cwd| !cwd.is_empty()),
        url: input.url,
        bearer: input.bearer.filter(|b| !b.is_empty()),
        bearer_env: input.bearer_env.filter(|b| !b.is_empty()),
        headers: input.headers,
        // Hub 目前只实现 shared；per-client/per-session 尚未落地，写入层统一 shared
        // 避免 UI/DB 语义与运行时行为不一致（见 control.rs RUNTIME_POLICY_SHARED）。
        runtime_policy: "shared".into(),
        enabled: input
            .enabled
            .unwrap_or_else(|| existing.as_ref().map(|e| e.enabled).unwrap_or(true)),
    };
    repo.upsert(def).await.map_err(|e| e.to_string())?;
    crate::auto_sync::notify_changed();
    super::hub::notify_hub_reload(&app).await
}

/// 测试一份 Server 配置能否连通（不落库）。经 Hub 执行：
/// bearer_env/进程环境的解析环境与真实运行完全一致。
#[tauri::command]
pub async fn server_test(input: ServerInput) -> Result<serde_json::Value, String> {
    require_ok(
        control_call(ControlCommand::TestServer {
            transport: input.transport,
            command: input.command,
            args: input.args,
            env: input.env,
            cwd: input.cwd.filter(|cwd| !cwd.is_empty()),
            url: input.url,
            bearer: input.bearer.filter(|b| !b.is_empty()),
            bearer_env: input.bearer_env.filter(|b| !b.is_empty()),
            headers: input.headers,
        })
        .await?,
        "Server 连通性测试失败",
    )
}

#[tauri::command]
pub async fn server_set_enabled(
    app: tauri::AppHandle,
    id: String,
    enabled: bool,
) -> Result<(), String> {
    server_repo()
        .await?
        .set_enabled(&id, enabled)
        .await
        .map_err(|e| e.to_string())?;
    crate::auto_sync::notify_changed();
    super::hub::notify_hub_reload(&app).await
}

#[tauri::command]
pub async fn server_delete(app: tauri::AppHandle, id: String) -> Result<(), String> {
    server_repo()
        .await?
        .delete(&id)
        .await
        .map_err(|e| e.to_string())?;
    crate::auto_sync::notify_changed();
    super::hub::notify_hub_reload(&app).await
}

/// 声明/撤回入参。键是 `(server_id, original_name)` 而非暴露名——声明存的是
/// 「这个下游工具的行为」，跨工作区恒成立，而暴露名会随 alias 改变。
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAnnotationsInput {
    pub server_id: String,
    pub original_name: String,
    /// null = 撤回声明（该工具回到"下游没申报就没有注解"）。
    pub annotations: Option<mcpane_hub_protocol::DeclaredAnnotations>,
}

/// 声明/撤回某下游工具的 ToolAnnotations（ADR-002 Gate 的例外口）。
///
/// 下游不申报 annotations 时，Codex `exec`（非交互）会把该工具误路由进
/// app-tool 审批路径直接取消调用（openai/codex#16685）；Hub 不替下游编，
/// 但让用户为自己装的工具署名声明一份。声明只在下游缺席时生效，独立成表、
/// 日志留痕，界面上也标着是谁说的。
///
/// 无 workspace 参数：注解陈述的是工具行为，跨工作区恒成立（迁移 0025）。
#[tauri::command]
pub async fn tool_annotations_set(
    app: tauri::AppHandle,
    input: ToolAnnotationsInput,
) -> Result<(), String> {
    server_repo()
        .await?
        .set_tool_annotations(&input.server_id, &input.original_name, input.annotations)
        .await
        .map_err(|e| e.to_string())?;
    crate::auto_sync::notify_changed();
    super::hub::notify_hub_reload(&app).await
}

#[cfg(test)]
mod tests {
    use super::{ServerInput, validated_transport};

    /// slug 现在直接复用 mcpane-hub-protocol 的唯一实现（规则断言在该 crate
    /// 的测试里）。这里只钉一条桌面端最关心的行为：保留字 `mcpane` 必须被
    /// 转义——旧的本地复刻恰恰漏了这个分支。
    #[test]
    fn slug_reuses_protocol_rule_including_reserved_word() {
        assert_eq!(
            mcpane_hub_protocol::normalize_slug("mcpane"),
            "mcpane-server"
        );
    }

    fn input(transport: &str, command: Option<&str>, url: Option<&str>) -> ServerInput {
        ServerInput {
            id: None,
            name: "test".into(),
            transport: transport.into(),
            command: command.map(str::to_owned),
            args: Vec::new(),
            env: Default::default(),
            cwd: None,
            url: url.map(str::to_owned),
            bearer: None,
            bearer_env: None,
            headers: Default::default(),
            enabled: None,
        }
    }

    /// 必填字段在入库前拦截（而不是等 Hub spawn 时才炸）的契约。
    #[test]
    fn transport_required_fields_are_rejected_upfront() {
        // stdio：命令缺失/空白都不放行。
        assert!(validated_transport(&input("stdio", None, None)).is_err());
        assert!(validated_transport(&input("stdio", Some("   "), None)).is_err());
        assert!(validated_transport(&input("stdio", Some("npx"), None)).is_ok());
        // http/sse：地址缺失/空白都不放行。
        assert!(validated_transport(&input("http", None, None)).is_err());
        assert!(validated_transport(&input("sse", Some("npx"), Some("  "))).is_err());
        assert!(validated_transport(&input("http", None, Some("https://x.dev/mcp"))).is_ok());
        // 未知 transport 直接报错，不猜。
        assert!(validated_transport(&input("websocket", None, None)).is_err());
    }
}
