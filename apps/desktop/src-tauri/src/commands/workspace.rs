//! Workspace 管理与 Tool 开关（TASK-504）。

use super::hub::{control_call, notify_hub_reload, require_ok};
use mcpane_hub_protocol::ControlCommand;

// ---------- Workspace Tool 开关（TASK-504 数据层，P0 单 Workspace） ----------

#[tauri::command]
pub async fn catalog_list(workspace_id: Option<String>) -> Result<serde_json::Value, String> {
    require_ok(
        control_call(ControlCommand::ListCatalog { workspace_id }).await?,
        "Catalog 查询失败",
    )
}

// ---------- Workspace 管理（TASK-504） ----------

pub(super) async fn workspace_repo() -> Result<mcpane_storage::WorkspaceRepo, String> {
    Ok(mcpane_storage::WorkspaceRepo::new(super::db_pool().await?))
}

#[tauri::command]
pub async fn workspaces_list() -> Result<Vec<mcpane_storage::Workspace>, String> {
    workspace_repo()
        .await?
        .list()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn workspace_create(
    app: tauri::AppHandle,
    name: String,
) -> Result<mcpane_storage::Workspace, String> {
    let workspace = workspace_repo()
        .await?
        .create(&name)
        .await
        .map_err(|e| e.to_string())?;
    // notify_changed 先于 Hub 通知：库已经变了，自动同步不能因 Hub 故障漏传。
    crate::auto_sync::notify_changed();
    notify_hub_reload(&app).await?;
    Ok(workspace)
}

/// Workspace 内 Server 成员开关。
#[tauri::command]
pub async fn workspace_server_set(
    app: tauri::AppHandle,
    workspace_id: String,
    server_id: String,
    enabled: bool,
) -> Result<(), String> {
    workspace_repo()
        .await?
        .set_server_enabled(&workspace_id, &server_id, enabled)
        .await
        .map_err(|e| e.to_string())?;
    crate::auto_sync::notify_changed();
    notify_hub_reload(&app).await
}

#[tauri::command]
pub async fn workspace_servers(workspace_id: String) -> Result<Vec<String>, String> {
    workspace_repo()
        .await?
        .enabled_servers(&workspace_id)
        .await
        .map_err(|e| e.to_string())
}

/// 连接绑定默认 Workspace（对新会话生效，ADR-004）。
#[tauri::command]
pub async fn connection_set_workspace(
    client_connection_id: String,
    workspace_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let result = control_call(ControlCommand::SetConnectionWorkspace {
        client_connection_id,
        workspace_id,
    })
    .await;
    crate::auto_sync::notify_changed();
    require_ok(result?, "设置连接的 Workspace 失败")
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolToggle {
    pub workspace_id: Option<String>,
    pub server_id: String,
    pub original_name: String,
    pub enabled: bool,
    /// 可选 alias（冲突由 Catalog 构建拒绝，reload 结果里体现）。
    pub alias: Option<String>,
    /// 追加到工具描述后的触发式提示（ADR-020 闸③）：告诉模型「何时该用」，
    /// 下游描述通常只说「能做什么」。
    pub description_suffix: Option<String>,
}

/// 开关某工具（写 workspace_tools 后通知 Hub 重建 Catalog；
/// 旧 Session 由二次权限校验兜底，新 Session 拿新 Revision）。
#[tauri::command]
pub async fn workspace_tool_toggle(app: tauri::AppHandle, input: ToolToggle) -> Result<(), String> {
    let pool = super::db_pool().await?;
    let workspace = input
        .workspace_id
        .clone()
        .unwrap_or_else(|| "ws_default".into());
    mcpane_storage::WorkspaceRepo::new(pool)
        .set_tool_override(
            &workspace,
            &mcpane_storage::ToolOverride {
                server_id: input.server_id,
                original_name: input.original_name,
                enabled: input.enabled,
                alias: input.alias.filter(|a| !a.trim().is_empty()),
                // 不 filter 空串：None 是「不改动」，空串才是「清空」（见 storage 注释）。
                description_suffix: input.description_suffix,
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    crate::auto_sync::notify_changed();
    notify_hub_reload(&app).await
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemToggle {
    /// "tool" / "prompt" / "resource"。
    pub kind: String,
    pub workspace_id: Option<String>,
    pub server_id: String,
    /// resource 传 URI。
    pub original_name: String,
    pub enabled: bool,
    /// 仅 tool/prompt 有意义（resource 的 URI 不改写）。
    pub alias: Option<String>,
    /// 追加到工具描述后的触发式提示；仅 tool 有意义。
    pub description_suffix: Option<String>,
}

/// 开关某条目（tool/prompt/resource 三类统一入口；写库后通知 Hub 重建）。
#[tauri::command]
pub async fn workspace_item_toggle(app: tauri::AppHandle, input: ItemToggle) -> Result<(), String> {
    let kind = mcpane_storage::ItemKind::parse(&input.kind)
        .ok_or_else(|| format!("未知条目类别：{}", input.kind))?;
    let workspace = input.workspace_id.unwrap_or_else(|| "ws_default".into());
    let alias = if kind == mcpane_storage::ItemKind::Resource {
        None
    } else {
        input.alias.filter(|a| !a.trim().is_empty())
    };
    workspace_repo()
        .await?
        .set_item_override(
            &workspace,
            kind,
            &mcpane_storage::ToolOverride {
                server_id: input.server_id,
                original_name: input.original_name,
                enabled: input.enabled,
                alias,
                description_suffix: if kind == mcpane_storage::ItemKind::Tool {
                    input.description_suffix
                } else {
                    None // prompt/resource 没有这一维，恒为「不改动」。
                },
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    crate::auto_sync::notify_changed();
    notify_hub_reload(&app).await
}

/// 列出某类条目的全部覆盖（含已禁用项，供恢复 UI 使用）。
#[tauri::command]
pub async fn workspace_item_overrides(
    kind: String,
    workspace_id: Option<String>,
) -> Result<Vec<mcpane_storage::ToolOverride>, String> {
    let kind =
        mcpane_storage::ItemKind::parse(&kind).ok_or_else(|| format!("未知条目类别：{kind}"))?;
    let workspace = workspace_id.unwrap_or_else(|| "ws_default".into());
    workspace_repo()
        .await?
        .item_overrides(&workspace, kind)
        .await
        .map_err(|e| e.to_string())
}

/// 工具动态发现开关（tools/list 只暴露 meta 工具，客户端按需检索）。
/// workspace_id 为 None 时作用于默认工作区（库里由 0011 的种子行承载）。
#[tauri::command]
pub async fn workspace_tool_discovery_set(
    app: tauri::AppHandle,
    workspace_id: Option<String>,
    enabled: bool,
) -> Result<(), String> {
    let workspace = workspace_id.unwrap_or_else(|| "ws_default".into());
    workspace_repo()
        .await?
        .set_tool_discovery(&workspace, enabled)
        .await
        .map_err(|e| e.to_string())?;
    crate::auto_sync::notify_changed();
    notify_hub_reload(&app).await
}

/// 查询发现开关（默认工作区不在 workspaces_list 里，需要单查）。
#[tauri::command]
pub async fn workspace_tool_discovery(workspace_id: Option<String>) -> Result<bool, String> {
    let workspace = workspace_id.unwrap_or_else(|| "ws_default".into());
    workspace_repo()
        .await?
        .tool_discovery(&workspace)
        .await
        .map_err(|e| e.to_string())
}

/// 工作区使用准则：用户自定义的工具路由规则，Hub 注入 instructions 让所有
/// 客户端共享（ADR-020 主动性轴）。workspace_id 为 None 时作用于默认工作区。
#[tauri::command]
pub async fn workspace_usage_guidance(workspace_id: Option<String>) -> Result<String, String> {
    let workspace = workspace_id.unwrap_or_else(|| "ws_default".into());
    workspace_repo()
        .await?
        .usage_guidance(&workspace)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn workspace_usage_guidance_set(
    app: tauri::AppHandle,
    workspace_id: Option<String>,
    guidance: String,
) -> Result<(), String> {
    let workspace = workspace_id.unwrap_or_else(|| "ws_default".into());
    workspace_repo()
        .await?
        .set_usage_guidance(&workspace, &guidance)
        .await
        .map_err(|e| e.to_string())?;
    crate::auto_sync::notify_changed();
    notify_hub_reload(&app).await
}

/// 列出当前 Workspace 的全部 Tool 覆盖（含已禁用项，供恢复 UI 使用）。
#[tauri::command]
pub async fn workspace_tool_overrides(
    workspace_id: Option<String>,
) -> Result<Vec<mcpane_storage::ToolOverride>, String> {
    let pool = super::db_pool().await?;
    let workspace = workspace_id.unwrap_or_else(|| "ws_default".into());
    mcpane_storage::WorkspaceRepo::new(pool)
        .tool_overrides(&workspace)
        .await
        .map_err(|e| e.to_string())
}
