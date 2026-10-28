//! 一键接入（TASK-600/601）+ 快捷配置：Cursor / VS Code / Cline / Devin。
//!
//! 凭据投递（ADR-015 + ADR-016）：两种模式都零手动步骤——
//! - EnvVar（默认）：apply 时经 Hub 发放 Token，由 envsync 持久化为环境变量
//!   （配置只写变量引用，明文存 MCPane 数据目录，不进客户端配置）；
//! - Inline（可选）：明文 Token 直写客户端配置。
//!
//! 两种模式的唯一残留动作一致：重启客户端。

use std::path::PathBuf;

use super::hub::control_call;
use super::mcpane_home;
use crate::envsync;
use mcpane_hub_protocol::ControlCommand;

// ---------- 一键接入（TASK-600/601） ----------

use mcpane_config::history::JsonlHistoryStore;
use mcpane_config::snapshot::FsSnapshotStore;
use mcpane_config::{
    ConfigDocument, ConfigEngine, ConfigFormat, CredentialDelivery, CredentialMode, LoadedConfig,
    PatchPlan, RealFs, is_usable_inline_bearer,
};

/// Inline 模式预览阶段的 Token 占位串（apply 时替换为真实 Token；
/// 带尖括号使 `is_usable_inline_bearer` 视其为不可用，杜绝占位串被误写为凭据）。
const TOKEN_PLACEHOLDER: &str = "<接入时自动发放>";

type Engine = ConfigEngine<FsSnapshotStore, JsonlHistoryStore, RealFs>;

pub(crate) fn engine() -> Engine {
    let home = mcpane_home();
    ConfigEngine::new(
        FsSnapshotStore::new(&home),
        JsonlHistoryStore::new(&home),
        RealFs,
    )
}

pub(crate) fn user_home() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into()))
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectPreview {
    pub target_path: String,
    pub unified_diff: String,
    /// 写入前配置全文（对比弹窗左栏；文件不存在时为空串）。
    pub before_text: String,
    /// 应用写入计划后的配置全文（对比弹窗右栏，不落盘）。
    pub after_text: String,
    /// 凭据模式：`env`（默认）或 `inline`（可选）。
    pub credential_mode: String,
    /// EnvVar 模式的环境变量名（inline 模式为 None）。写入时由 envsync 自动设置。
    pub env_var: Option<String>,
}

/// 把 CredentialMode 映射为前端字符串。
fn mode_str(mode: CredentialMode) -> String {
    match mode {
        CredentialMode::Inline => "inline".to_owned(),
        CredentialMode::EnvVar => "env".to_owned(),
    }
}

/// apply 返回前对 diff 文本脱敏：完整 Token → 前 8 位 + …。
fn redact_token(text: &str, token: &str) -> String {
    let prefix: String = token.chars().take(8).collect();
    text.replace(token, &format!("{prefix}…"))
}

/// Inline 接入前经 Hub 控制面确保连接并拿到明文 Token。
///
/// 算法（ADR-015）：该 clientType 已有 enabled 连接 → 轮换第一个（保 workspace
/// 绑定，旧 Token 立即作废=自愈）；没有 → 新建（不复活 disabled，撤销是用户
/// 显式安全动作）。任一步失败都不进入写配置阶段——写入的配置必须可用。
async fn ensure_connection_token(
    client_type: &str,
    display: &str,
) -> Result<(String, String), String> {
    let status = control_call(ControlCommand::Status)
        .await
        .map_err(|e| format!("接入需要 Hub 在线发放 Token（可在「概览」启动 Hub）：{e}"))?;
    let existing = status["connections"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| c["clientType"] == client_type && c["enabled"] == true)
        .and_then(|c| c["clientConnectionId"].as_str().map(str::to_owned));

    let response = match &existing {
        Some(id) => {
            control_call(ControlCommand::RotateToken {
                client_connection_id: id.clone(),
            })
            .await?
        }
        None => {
            control_call(ControlCommand::CreateConnection {
                name: display.to_owned(),
                client_type: client_type.to_owned(),
            })
            .await?
        }
    };
    if response["ok"] != true {
        return Err(format!(
            "Hub 发放 Token 失败：{}",
            response["error"].as_str().unwrap_or("未知错误")
        ));
    }
    let token = response["token"]
        .as_str()
        .ok_or("控制面响应缺少 token 字段")?
        .to_owned();
    let connection_id = existing
        .or_else(|| response["connection"]["id"].as_str().map(str::to_owned))
        .unwrap_or_default();
    Ok((token, connection_id))
}

/// 计算对比弹窗所需的前后全文。
///
/// 两侧都用解析后重新序列化的归一化文本，而非左栏磁盘原文：JSON 序列化会
/// 重排键序/统一缩进（serde_json 无 preserve_order），左栏用原文会把整份
/// 文件渲染成变更噪声——归一化后对比只剩语义 diff。落盘文本本就是归一化
/// 结果（engine 写 `after.serialize()`），右栏与实际写入内容一致。
fn preview_texts(loaded: &LoadedConfig, plan: &PatchPlan) -> Result<(String, String), String> {
    let before =
        ConfigDocument::parse(&loaded.content, loaded.format).map_err(|e| e.to_string())?;
    // try_clone 而非 clone：JSON5 这一路的克隆要绕「序列化 → 重解析」，真的会失败。
    // 预览与写入必须用同一判据——预览若靠兜底"成功"了，用户点确认才在写入阶段
    // 报错，等于让他先看一份不存在的 diff。
    let mut after = before.try_clone().map_err(|e| e.to_string())?;
    plan.apply(&mut after).map_err(|e| e.to_string())?;
    Ok((before.serialize(), after.serialize()))
}

/// 在阻塞线程上执行 envsync 持久化（文件 IO + 系统调用，不能占用异步线程）。
async fn persist_env_token(
    env_var: &'static str,
    token: String,
) -> Result<envsync::EnvSyncReport, String> {
    tauri::async_runtime::spawn_blocking(move || envsync::persist_env_var(env_var, &token))
        .await
        .map_err(|e| e.to_string())?
}

/// 客户端类型 → Token 环境变量名（Cline 不支持环境变量展开，为 None）。
fn client_token_env(client_type: &str) -> Option<&'static str> {
    match client_type {
        "claude" => Some(mcpane_adapter_claude::TOKEN_ENV_VAR),
        "codex" => Some(mcpane_adapter_codex::TOKEN_ENV_VAR),
        other => crate::mcp_clients::spec(other).and_then(|spec| spec.token_env),
    }
}

/// 配置文件中是否已存在 mcpane Hub 条目（「已接入」状态；文件缺失/解析失败视为未接入）。
pub(crate) fn config_has_hub_entry(
    path: &std::path::Path,
    format: ConfigFormat,
    segs: &[&str],
) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    ConfigDocument::parse(&content, format).is_ok_and(|doc| doc.get(segs).is_some())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectOutcome {
    pub target_path: String,
    pub unified_diff: String,
    pub snapshot_id: String,
    /// 凭据模式：`inline` 或 `env`。
    pub credential_mode: String,
    /// 本次自动发放的 Token 前缀（两种模式都返回，凭证页可对照识别）。
    pub token_prefix: Option<String>,
    /// 收尾指引：两种模式都只剩「重启客户端」，env 模式附持久化明细与降级警告。
    pub next_steps: Vec<String>,
}

/// Inline 模式 apply 的公共收尾：diff 脱敏 + 组装 Outcome。
fn inline_outcome(
    target_path: String,
    unified_diff: &str,
    snapshot_id: String,
    token: &str,
    client_display: &str,
) -> ConnectOutcome {
    let prefix: String = token.chars().take(8).collect();
    ConnectOutcome {
        target_path,
        unified_diff: redact_token(unified_diff, token),
        snapshot_id,
        credential_mode: mode_str(CredentialMode::Inline),
        token_prefix: Some(prefix),
        next_steps: vec![format!(
            "Token 已签发并写入配置，重启 {client_display} 即可使用，无需其他步骤"
        )],
    }
}

/// EnvVar 模式 apply 的公共收尾：Token 已由 envsync 自动持久化，
/// next_steps 由持久化报告生成（成功项 + 降级警告 + 唯一手动步骤=重启）。
fn env_outcome(
    target_path: String,
    unified_diff: String,
    snapshot_id: String,
    token: &str,
    report: &envsync::EnvSyncReport,
    client_display: &str,
    terminal_client: bool,
) -> ConnectOutcome {
    let prefix: String = token.chars().take(8).collect();
    ConnectOutcome {
        target_path,
        unified_diff,
        snapshot_id,
        credential_mode: mode_str(CredentialMode::EnvVar),
        token_prefix: Some(prefix),
        next_steps: report.next_steps(client_display, terminal_client),
    }
}

/// Claude Code 一键接入预览（User scope：~/.claude.json）。
#[tauri::command]
pub async fn claude_connect_preview(mode: Option<String>) -> Result<ConnectPreview, String> {
    let mode = CredentialMode::parse(mode.as_deref())?;
    tauri::async_runtime::spawn_blocking(move || {
        let paths = mcpane_adapter_claude::discover_config_paths(&user_home(), None);
        let engine = engine();
        let loaded = engine
            .load(&paths.user_config, ConfigFormat::Json)
            .map_err(|e| e.to_string())?;
        let prefix = vec!["mcpServers".to_owned()];
        let credential = match mode {
            CredentialMode::Inline => CredentialDelivery::Inline(TOKEN_PLACEHOLDER),
            CredentialMode::EnvVar => CredentialDelivery::EnvVar,
        };
        let plan = mcpane_adapter_claude::hub_entry_patch(&prefix, None, credential);
        let diff = engine.preview(&loaded, &plan).map_err(|e| e.to_string())?;
        let (before_text, after_text) = preview_texts(&loaded, &plan)?;
        Ok(ConnectPreview {
            target_path: paths.user_config.display().to_string(),
            unified_diff: diff.unified,
            before_text,
            after_text,
            credential_mode: mode_str(mode),
            env_var: (mode == CredentialMode::EnvVar)
                .then(|| mcpane_adapter_claude::TOKEN_ENV_VAR.to_owned()),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Claude Code 一键接入：写入 Hub Entry（Diff→Snapshot→原子写→验证）。
/// EnvVar 默认：只写引用；Inline 可选：先经 Hub 发放 Token 再写。
#[tauri::command]
pub async fn claude_connect_apply(mode: Option<String>) -> Result<ConnectOutcome, String> {
    let mode = CredentialMode::parse(mode.as_deref())?;
    match mode {
        CredentialMode::Inline => {
            let (token, _) = ensure_connection_token("claude", "Claude Code").await?;
            let (path, diff, snapshot_id) = claude_apply_core(
                Some(token.clone()),
                "一键接入 MCP Hub（Claude Code）".to_owned(),
            )
            .await?;
            Ok(inline_outcome(
                path,
                &diff,
                snapshot_id,
                &token,
                "Claude Code",
            ))
        }
        CredentialMode::EnvVar => {
            // 一键化（ADR-016）：发放 Token → 持久化环境变量 → 写配置。
            // 顺序不可反：保证「配置引用了变量 ⇒ 变量已持久化」。
            let (token, _) = ensure_connection_token("claude", "Claude Code").await?;
            let report =
                persist_env_token(mcpane_adapter_claude::TOKEN_ENV_VAR, token.clone()).await?;
            let (path, diff, snapshot_id) =
                claude_apply_core(None, "一键接入 MCP Hub（Claude Code）".to_owned())
                    .await
                    .map_err(|e| {
                        format!("Token 已保存为环境变量，但写入配置失败：{e}。重新连接即可修复")
                    })?;
            Ok(env_outcome(
                path,
                diff,
                snapshot_id,
                &token,
                &report,
                "Claude Code",
                true,
            ))
        }
    }
}

/// Claude 写入核心：token=Some 走 Inline，None 走 EnvVar。
/// 返回 (target_path, unified_diff, snapshot_id)。
async fn claude_apply_core(
    token: Option<String>,
    desc: String,
) -> Result<(String, String, String), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let paths = mcpane_adapter_claude::discover_config_paths(&user_home(), None);
        let engine = engine();
        let loaded = engine
            .load(&paths.user_config, ConfigFormat::Json)
            .map_err(|e| e.to_string())?;
        let prefix = vec!["mcpServers".to_owned()];
        let credential = match &token {
            Some(t) => CredentialDelivery::Inline(t),
            None => CredentialDelivery::EnvVar,
        };
        let mode = credential.mode();
        let plan = mcpane_adapter_claude::hub_entry_patch(&prefix, None, credential);
        let validate_prefix = prefix.clone();
        let outcome = engine
            .apply(
                &loaded,
                &plan,
                &desc,
                Some(&move |doc| {
                    mcpane_adapter_claude::validate_hub_entry(doc, &validate_prefix, mode)
                }),
                None,
            )
            .map_err(|e| e.to_string())?;
        Ok((
            paths.user_config.display().to_string(),
            outcome.diff.unified,
            outcome
                .record
                .snapshot
                .as_ref()
                .map(|s| s.id.clone())
                .unwrap_or_default(),
        ))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Claude Code 解绑：删除 mcpane Entry（其余配置保留）。
/// 返回 warnings：环境变量清理失败时 shell 配置里仍留着**有效的** Token，
/// 用户必须知情（此前只 eprintln，前端毫无感知）。
#[tauri::command]
pub async fn claude_disconnect() -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let paths = mcpane_adapter_claude::discover_config_paths(&user_home(), None);
        let engine = engine();
        let loaded = engine
            .load(&paths.user_config, ConfigFormat::Json)
            .map_err(|e| e.to_string())?;
        let prefix = vec!["mcpServers".to_owned()];
        engine
            .apply(
                &loaded,
                &mcpane_adapter_claude::remove_hub_entry_patch(&prefix),
                "解绑 MCP Hub（Claude Code）",
                None,
                None,
            )
            .map_err(|e| e.to_string())?;
        // 凭据引用已删，同步清理明文（清理失败不阻塞解绑，但要让用户知情）。
        Ok(env_cleanup_warnings(envsync::remove_env_var(
            mcpane_adapter_claude::TOKEN_ENV_VAR,
        )))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 环境变量清理结果 → warnings（解绑三入口共用）。
fn env_cleanup_warnings<T, E: std::fmt::Display>(result: Result<T, E>) -> Vec<String> {
    match result {
        Ok(_) => Vec::new(),
        Err(e) => vec![format!(
            "清理环境变量失败：{e}。shell 配置里仍留有有效的 Hub Token，\
             建议手动删除该行，或在「连接」页撤销此 Token"
        )],
    }
}

/// Codex 一键接入预览（~/.codex/config.toml，尊重 CODEX_HOME）。
#[tauri::command]
pub async fn codex_connect_preview(mode: Option<String>) -> Result<ConnectPreview, String> {
    let mode = CredentialMode::parse(mode.as_deref())?;
    tauri::async_runtime::spawn_blocking(move || {
        let codex_home = std::env::var("CODEX_HOME").ok().map(PathBuf::from);
        let paths =
            mcpane_adapter_codex::discover_config_paths(&user_home(), codex_home.as_deref(), None);
        let engine = engine();
        let loaded = engine
            .load(&paths.user_config, ConfigFormat::Toml)
            .map_err(|e| e.to_string())?;
        let credential = match mode {
            CredentialMode::Inline => CredentialDelivery::Inline(TOKEN_PLACEHOLDER),
            CredentialMode::EnvVar => CredentialDelivery::EnvVar,
        };
        let plan = mcpane_adapter_codex::hub_entry_patch(None, credential);
        let diff = engine.preview(&loaded, &plan).map_err(|e| e.to_string())?;
        let (before_text, after_text) = preview_texts(&loaded, &plan)?;
        Ok(ConnectPreview {
            target_path: paths.user_config.display().to_string(),
            unified_diff: diff.unified,
            before_text,
            after_text,
            credential_mode: mode_str(mode),
            env_var: (mode == CredentialMode::EnvVar)
                .then(|| mcpane_adapter_codex::TOKEN_ENV_VAR.to_owned()),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Codex 一键接入：写入 [mcp_servers.mcpane]。
/// EnvVar 默认：bearer_token_env_var；Inline 可选：http_headers.Authorization 明文。
#[tauri::command]
pub async fn codex_connect_apply(mode: Option<String>) -> Result<ConnectOutcome, String> {
    let mode = CredentialMode::parse(mode.as_deref())?;
    match mode {
        CredentialMode::Inline => {
            let (token, _) = ensure_connection_token("codex", "Codex").await?;
            let (path, diff, snapshot_id) =
                codex_apply_core(Some(token.clone()), "一键接入 MCP Hub（Codex）".to_owned())
                    .await?;
            Ok(inline_outcome(path, &diff, snapshot_id, &token, "Codex"))
        }
        CredentialMode::EnvVar => {
            let (token, _) = ensure_connection_token("codex", "Codex").await?;
            let report =
                persist_env_token(mcpane_adapter_codex::TOKEN_ENV_VAR, token.clone()).await?;
            let (path, diff, snapshot_id) =
                codex_apply_core(None, "一键接入 MCP Hub（Codex）".to_owned())
                    .await
                    .map_err(|e| {
                        format!("Token 已保存为环境变量，但写入配置失败：{e}。重新连接即可修复")
                    })?;
            Ok(env_outcome(
                path,
                diff,
                snapshot_id,
                &token,
                &report,
                "Codex",
                true,
            ))
        }
    }
}

/// Codex 写入核心：token=Some 走 Inline，None 走 EnvVar。
async fn codex_apply_core(
    token: Option<String>,
    desc: String,
) -> Result<(String, String, String), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let codex_home = std::env::var("CODEX_HOME").ok().map(PathBuf::from);
        let paths =
            mcpane_adapter_codex::discover_config_paths(&user_home(), codex_home.as_deref(), None);
        let engine = engine();
        let loaded = engine
            .load(&paths.user_config, ConfigFormat::Toml)
            .map_err(|e| e.to_string())?;
        let credential = match &token {
            Some(t) => CredentialDelivery::Inline(t),
            None => CredentialDelivery::EnvVar,
        };
        let mode = credential.mode();
        let outcome = engine
            .apply(
                &loaded,
                &mcpane_adapter_codex::hub_entry_patch(None, credential),
                &desc,
                Some(&move |doc| mcpane_adapter_codex::validate_hub_entry(doc, mode)),
                None,
            )
            .map_err(|e| e.to_string())?;
        Ok((
            paths.user_config.display().to_string(),
            outcome.diff.unified,
            outcome
                .record
                .snapshot
                .as_ref()
                .map(|s| s.id.clone())
                .unwrap_or_default(),
        ))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Codex 解绑。返回 warnings，语义同 claude_disconnect。
#[tauri::command]
pub async fn codex_disconnect() -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let codex_home = std::env::var("CODEX_HOME").ok().map(PathBuf::from);
        let paths =
            mcpane_adapter_codex::discover_config_paths(&user_home(), codex_home.as_deref(), None);
        let engine = engine();
        let loaded = engine
            .load(&paths.user_config, ConfigFormat::Toml)
            .map_err(|e| e.to_string())?;
        engine
            .apply(
                &loaded,
                &mcpane_adapter_codex::remove_hub_entry_patch(),
                "解绑 MCP Hub（Codex）",
                None,
                None,
            )
            .map_err(|e| e.to_string())?;
        Ok(env_cleanup_warnings(envsync::remove_env_var(
            mcpane_adapter_codex::TOKEN_ENV_VAR,
        )))
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- 快捷配置：Cursor / VS Code / Cline / Devin ----------

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpClientStatus {
    id: String,
    display: String,
    installed: bool,
    /// 配置文件里已存在 mcpane Hub 条目。
    connected: bool,
    /// Token 环境变量名；null 表示该客户端只能明文（Cline）。
    token_env: Option<String>,
    config_path: String,
    /// 用户手动添加的客户端：MCPane 不认识它的配置文件，接入靠用户自己把
    /// 配置片段粘过去，因此没有"接入/断开"，只有凭证与访问控制。
    custom: bool,
}

/// 列出全部快捷配置客户端及其安装状态，末尾接上用户手动添加的自定义客户端。
#[tauri::command]
pub async fn mcp_clients_status() -> Result<Vec<McpClientStatus>, String> {
    // join 失败要如实上抛：此前 unwrap_or_default 会把探测故障渲染成
    // 「一个客户端都没有」，用户看到的是错的空列表而非错误。
    let mut builtin = tauri::async_runtime::spawn_blocking(|| {
        let home = user_home();
        crate::mcp_clients::CLIENTS
            .iter()
            .map(|spec| McpClientStatus {
                id: spec.id.to_owned(),
                display: spec.display.to_owned(),
                installed: spec.is_installed(&home),
                connected: spec.is_connected(&home),
                token_env: spec.token_env.map(str::to_owned),
                config_path: spec.config_path(&home).display().to_string(),
                custom: false,
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| format!("客户端状态探测失败：{e}"))?;

    builtin.extend(
        custom_clients_repo()
            .await?
            .list()
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|client| McpClientStatus {
                id: client.id,
                display: client.display,
                // 自定义客户端没有可探测的安装痕迹，也没有我们能读的配置文件：
                // 它"在不在"由用户添加这个动作本身回答。
                installed: true,
                connected: false,
                token_env: None,
                config_path: String::new(),
                custom: true,
            }),
    );
    Ok(builtin)
}

/// 自定义客户端的入库校验，返回规整后的 (id, display)。
///
/// 抽成纯函数是为了能单测——校验错了的后果是两个客户端共用一个 client_type，
/// 凭证与访问控制从此互相打架，而那要等用户真发了 Token 才看得出来。
fn validate_custom_client(id: &str, display: &str) -> Result<(String, String), String> {
    let id = id.trim().to_owned();
    let display = display.trim().to_owned();
    if id.is_empty() || display.is_empty() {
        return Err("标识与名称都不能为空".into());
    }
    // id 会成为 connections.client_type 的取值，限制成 URL 安全字符。
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("标识只能用字母、数字、连字符与下划线".into());
    }
    // 与内置客户端重名会让两边的连接混在一起，凭证与访问控制互相打架。
    // claude / codex 走专用 adapter，不在 CLIENTS 表里，得单独挡一道。
    if crate::mcp_clients::spec(&id).is_some() || matches!(id.as_str(), "claude" | "codex") {
        return Err(format!("标识 {id} 与内置客户端重名"));
    }
    Ok((id, display))
}

/// 自定义客户端的通用配置片段：`mcpServers.mcpane` + Streamable HTTP + Bearer。
///
/// Token 直接写占位串而不是 `${env:...}`：环境变量展开各家语法不同（编辑器系
/// 是 `${env:X}`、Gemini 是 `${X}`、还有一批压根不展开），对一个我们不认识的
/// 客户端猜错语法，用户拿到的就是一个连不上且看不出为什么的配置。
fn custom_client_snippet_patch() -> mcpane_config::PatchPlan {
    mcpane_config::PatchPlan::new(vec![mcpane_config::PatchOp::Set {
        path: vec!["mcpServers".to_owned(), "mcpane".to_owned()],
        value: serde_json::json!({
            "type": "http",
            "url": mcpane_hub_protocol::HUB_MCP_URL,
            "headers": { "Authorization": "Bearer <在「凭证」里发放的 Token>" },
        }),
    }])
}

async fn custom_clients_repo() -> Result<mcpane_storage::CustomClientRepo, String> {
    Ok(mcpane_storage::CustomClientRepo::new(
        crate::commands::db_pool().await?,
    ))
}

/// 用户手动添加的 MCP 客户端。
#[tauri::command]
pub async fn custom_mcp_clients_list() -> Result<Vec<mcpane_storage::CustomMcpClient>, String> {
    custom_clients_repo()
        .await?
        .list()
        .await
        .map_err(|e| e.to_string())
}

/// 新增（或改名）一个自定义 MCP 客户端。
#[tauri::command]
pub async fn custom_mcp_client_upsert(id: String, display: String) -> Result<(), String> {
    let (id, display) = validate_custom_client(&id, &display)?;
    custom_clients_repo()
        .await?
        .upsert(&id, &display)
        .await
        .map_err(|e| e.to_string())
}

/// 删除一个自定义 MCP 客户端，并吊销它名下的全部 Token。
///
/// 撤销必须走 Hub 控制面：只删库的话，Hub 内存里的 Token 注册表还认那些
/// 凭据，已经发出去的 Token 会继续可用到下次重启。
#[tauri::command]
pub async fn custom_mcp_client_remove(id: String) -> Result<(), String> {
    // 先查这个客户端名下有哪些连接（直读库，不依赖 Hub 在跑），再逐个吊销。
    let repo = mcpane_storage::ConnectionRepo::new(crate::commands::db_pool().await?);
    let connections = repo.list().await.map_err(|e| e.to_string())?;
    for connection in connections.into_iter().filter(|c| c.client_type == id) {
        // 优先走控制面，让 Hub 内存里的 Token 注册表当场失效。
        // Hub 没在跑时降级为直接删库：注册表本就不存在，而 Hub 下次启动是
        // 先 clear 再从库水合的，删掉的连接不会回来——等价于已吊销。
        // 不降级的话，Hub 停着时用户根本删不掉这个客户端。
        if crate::commands::connection_delete(connection.id.clone())
            .await
            .is_err()
        {
            repo.delete(&connection.id)
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    let removed = custom_clients_repo()
        .await?
        .remove(&id)
        .await
        .map_err(|e| e.to_string())?;
    if !removed {
        return Err(format!("未找到自定义客户端：{id}"));
    }
    Ok(())
}

fn mcp_client_spec(client: &str) -> Result<&'static crate::mcp_clients::McpClientSpec, String> {
    crate::mcp_clients::spec(client).ok_or_else(|| format!("未知客户端：{client}"))
}

/// 快捷配置接入预览。
#[tauri::command]
pub async fn mcp_client_connect_preview(
    client: String,
    mode: Option<String>,
) -> Result<ConnectPreview, String> {
    let mode = CredentialMode::parse(mode.as_deref())?;
    tauri::async_runtime::spawn_blocking(move || {
        let spec = mcp_client_spec(&client)?;
        if mode == CredentialMode::EnvVar && spec.token_env.is_none() {
            return Err(format!(
                "{} 不支持环境变量模式，请使用「直接写入 Token」",
                spec.display
            ));
        }
        let path = spec.config_path(&user_home());
        let engine = engine();
        let loaded = engine
            .load(&path, ConfigFormat::Json)
            .map_err(|e| e.to_string())?;
        let credential = match mode {
            CredentialMode::Inline => CredentialDelivery::Inline(TOKEN_PLACEHOLDER),
            CredentialMode::EnvVar => CredentialDelivery::EnvVar,
        };
        let plan = spec.hub_entry_patch(None, credential);
        let diff = engine.preview(&loaded, &plan).map_err(|e| e.to_string())?;
        let (before_text, after_text) = preview_texts(&loaded, &plan)?;
        Ok(ConnectPreview {
            target_path: path.display().to_string(),
            unified_diff: diff.unified,
            before_text,
            after_text,
            credential_mode: mode_str(mode),
            env_var: (mode == CredentialMode::EnvVar)
                .then(|| spec.token_env.map(str::to_owned))
                .flatten(),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 快捷配置一键写入（Diff→Snapshot→原子写→校验；父目录缺失时先创建）。
#[tauri::command]
pub async fn mcp_client_connect_apply(
    client: String,
    mode: Option<String>,
) -> Result<ConnectOutcome, String> {
    let mode = CredentialMode::parse(mode.as_deref())?;
    let spec = mcp_client_spec(&client)?;
    let (display, terminal) = (spec.display, spec.terminal);
    match mode {
        CredentialMode::Inline => {
            let (token, _) = ensure_connection_token(&client, display).await?;
            let (path, diff, snapshot_id) = mcp_client_apply_core(
                client,
                Some(token.clone()),
                format!("一键接入 MCP Hub（{display}）"),
            )
            .await?;
            Ok(inline_outcome(path, &diff, snapshot_id, &token, display))
        }
        CredentialMode::EnvVar => {
            let Some(env_var) = spec.token_env else {
                return Err(format!(
                    "{display} 不支持环境变量模式，请使用「直接写入 Token」"
                ));
            };
            let (token, _) = ensure_connection_token(&client, display).await?;
            let report = persist_env_token(env_var, token.clone()).await?;
            let (path, diff, snapshot_id) =
                mcp_client_apply_core(client, None, format!("一键接入 MCP Hub（{display}）"))
                    .await
                    .map_err(|e| {
                        format!("Token 已保存为环境变量，但写入配置失败：{e}。重新连接即可修复")
                    })?;
            Ok(env_outcome(
                path,
                diff,
                snapshot_id,
                &token,
                &report,
                display,
                terminal,
            ))
        }
    }
}

/// 快捷配置写入核心：token=Some 走 Inline，None 走 EnvVar。
async fn mcp_client_apply_core(
    client: String,
    token: Option<String>,
    desc: String,
) -> Result<(String, String, String), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let spec = mcp_client_spec(&client)?;
        let path = spec.config_path(&user_home());
        // 首次接入时配置文件父目录可能不存在（如 Cline 的 settings/），先兜底创建。
        if let Some(parent) = path.parent()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建配置目录失败：{e}"))?;
        }
        let engine = engine();
        let loaded = engine
            .load(&path, ConfigFormat::Json)
            .map_err(|e| e.to_string())?;
        let credential = match &token {
            Some(t) => CredentialDelivery::Inline(t),
            None => CredentialDelivery::EnvVar,
        };
        let mode = credential.mode();
        let outcome = engine
            .apply(
                &loaded,
                &spec.hub_entry_patch(None, credential),
                &desc,
                Some(&move |doc| spec.validate_hub_entry(doc, mode)),
                None,
            )
            .map_err(|e| e.to_string())?;
        Ok((
            path.display().to_string(),
            outcome.diff.unified,
            outcome
                .record
                .snapshot
                .as_ref()
                .map(|s| s.id.clone())
                .unwrap_or_default(),
        ))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 客户端配置片段（「使用说明」手动配置用）：
/// 对空文档应用 Hub Entry 写入计划后的完整序列化文本。
/// 固定 EnvVar 形态（默认模式）——静态片段不可能携带真实 Token。
#[tauri::command]
pub async fn client_config_snippet(client: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (format, plan) = match client.as_str() {
            "claude" => (
                ConfigFormat::Json,
                mcpane_adapter_claude::hub_entry_patch(
                    &["mcpServers".to_owned()],
                    None,
                    CredentialDelivery::EnvVar,
                ),
            ),
            "codex" => (
                ConfigFormat::Toml,
                mcpane_adapter_codex::hub_entry_patch(None, CredentialDelivery::EnvVar),
            ),
            // 自定义客户端没有 spec：给一段通用的 Streamable HTTP 条目，
            // 这是绝大多数 MCP 客户端认的形态，用户照着改自己的顶层键即可。
            other if crate::mcp_clients::spec(other).is_none() => {
                (ConfigFormat::Json, custom_client_snippet_patch())
            }
            other => (
                ConfigFormat::Json,
                mcp_client_spec(other)?.hub_entry_patch(None, CredentialDelivery::EnvVar),
            ),
        };
        let empty = if format == ConfigFormat::Toml {
            ""
        } else {
            "{}"
        };
        let mut doc = ConfigDocument::parse(empty, format).map_err(|e| e.to_string())?;
        plan.apply(&mut doc).map_err(|e| e.to_string())?;
        Ok(doc.serialize())
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- 轮换 Token 联动同步（ADR-015 + ADR-016） ----------

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RotateSyncOutcome {
    pub token_prefix: String,
    /// 仅在完全未接入时返回明文供一次性展示；已联动同步时不回传。
    pub token: Option<String>,
    /// Inline 形态：配置文件已就地改写。
    pub synced: Option<SyncedConfig>,
    /// EnvVar 形态：环境变量已由 envsync 重写。
    pub env_synced: Option<envsync::EnvSyncReport>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncedConfig {
    pub target_path: String,
    pub snapshot_id: String,
}

/// 该客户端配置中是否存在 mcpane Hub 条目（任一凭据形态）。
/// 有条目而非 Inline ⇒ env 形态，轮换走 envsync 接管。
fn client_config_has_entry(client_type: &str) -> bool {
    let home = user_home();
    match client_type {
        "claude" => {
            let paths = mcpane_adapter_claude::discover_config_paths(&home, None);
            config_has_hub_entry(
                &paths.user_config,
                ConfigFormat::Json,
                &["mcpServers", "mcpane"],
            )
        }
        "codex" => {
            let codex_home = std::env::var("CODEX_HOME").ok().map(PathBuf::from);
            let paths =
                mcpane_adapter_codex::discover_config_paths(&home, codex_home.as_deref(), None);
            config_has_hub_entry(
                &paths.user_config,
                ConfigFormat::Toml,
                &["mcp_servers", "mcpane"],
            )
        }
        other => crate::mcp_clients::spec(other).is_some_and(|spec| spec.is_connected(&home)),
    }
}

/// 该客户端配置中的 mcpane 条目当前是否为 Inline 凭据形态。
fn client_config_uses_inline(client_type: &str) -> bool {
    let home = user_home();
    let read = |path: &std::path::Path, format: ConfigFormat, segs: &[&str]| -> bool {
        let Ok(content) = std::fs::read_to_string(path) else {
            return false;
        };
        ConfigDocument::parse(&content, format).is_ok_and(|doc| {
            doc.get(segs)
                .and_then(|v| v.as_str().map(is_usable_inline_bearer))
                .unwrap_or(false)
        })
    };
    match client_type {
        "claude" => {
            let paths = mcpane_adapter_claude::discover_config_paths(&home, None);
            read(
                &paths.user_config,
                ConfigFormat::Json,
                &["mcpServers", "mcpane", "headers", "Authorization"],
            )
        }
        "codex" => {
            let codex_home = std::env::var("CODEX_HOME").ok().map(PathBuf::from);
            let paths =
                mcpane_adapter_codex::discover_config_paths(&home, codex_home.as_deref(), None);
            read(
                &paths.user_config,
                ConfigFormat::Toml,
                &["mcp_servers", "mcpane", "http_headers", "Authorization"],
            )
        }
        other => crate::mcp_clients::spec(other).is_some_and(|spec| {
            let Ok(content) = std::fs::read_to_string(spec.config_path(&home)) else {
                return false;
            };
            ConfigDocument::parse(&content, ConfigFormat::Json)
                .is_ok_and(|doc| spec.uses_inline_credential(&doc))
        }),
    }
}

/// 轮换 Token 并联动同步（凭证弹窗用）。
///
/// 明文只在 rotate 响应出现一次（ADR-003），必须在这一刻落到引用它的地方：
/// - Inline 形态 → 就地改写客户端配置；
/// - EnvVar 形态（有 Hub 条目且非 Inline）→ envsync 重写环境变量；
/// - 完全未接入 → 明文返回前端一次性展示。
#[tauri::command]
pub async fn connection_rotate_sync(
    client_connection_id: String,
    client_type: String,
) -> Result<RotateSyncOutcome, String> {
    let response = control_call(ControlCommand::RotateToken {
        client_connection_id,
    })
    .await?;
    if response["ok"] != true {
        return Err(format!(
            "轮换失败：{}",
            response["error"].as_str().unwrap_or("未知错误")
        ));
    }
    let token = response["token"]
        .as_str()
        .ok_or("控制面响应缺少 token 字段")?
        .to_owned();
    let token_prefix: String = token.chars().take(8).collect();
    // token_hash 已在库中轮换，属同步范围的业务数据变更。
    crate::auto_sync::notify_changed();

    let ct = client_type.clone();
    let (inline, has_entry) = tauri::async_runtime::spawn_blocking(move || {
        (client_config_uses_inline(&ct), client_config_has_entry(&ct))
    })
    .await
    .map_err(|e| e.to_string())?;

    if inline {
        let desc = format!("轮换 Token 并同步客户端配置（{client_type}）");
        let sync = match client_type.as_str() {
            "claude" => claude_apply_core(Some(token.clone()), desc).await,
            "codex" => codex_apply_core(Some(token.clone()), desc).await,
            other => mcp_client_apply_core(other.to_owned(), Some(token.clone()), desc).await,
        };
        return match sync {
            Ok((target_path, _diff, snapshot_id)) => Ok(RotateSyncOutcome {
                token_prefix,
                token: None,
                synced: Some(SyncedConfig {
                    target_path,
                    snapshot_id,
                }),
                env_synced: None,
            }),
            Err(e) => Err(format!(
                "Token 已轮换，但同步配置失败：{e}。请在客户端页的「…」菜单中选择「重新连接」修复"
            )),
        };
    }

    if has_entry && let Some(env_var) = client_token_env(&client_type) {
        let report = persist_env_token(env_var, token).await.map_err(|e| {
            format!("Token 已轮换，但保存环境变量失败：{e}。请在客户端页的「…」菜单中选择「重新连接」修复")
        })?;
        return Ok(RotateSyncOutcome {
            token_prefix,
            token: None,
            synced: None,
            env_synced: Some(report),
        });
    }

    // 未接入：明文一次性展示（展示后不落任何本地状态存储）。
    Ok(RotateSyncOutcome {
        token_prefix,
        token: Some(token),
        synced: None,
        env_synced: None,
    })
}

/// 快捷配置解绑。返回 warnings，语义同 claude_disconnect。
#[tauri::command]
pub async fn mcp_client_disconnect(client: String) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let spec = mcp_client_spec(&client)?;
        let path = spec.config_path(&user_home());
        let engine = engine();
        let loaded = engine
            .load(&path, ConfigFormat::Json)
            .map_err(|e| e.to_string())?;
        engine
            .apply(
                &loaded,
                &spec.remove_hub_entry_patch(),
                &format!("解绑 MCP Hub（{}）", spec.display),
                None,
                None,
            )
            .map_err(|e| e.to_string())?;
        Ok(match spec.token_env {
            Some(env_var) => env_cleanup_warnings(envsync::remove_env_var(env_var)),
            None => Vec::new(),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// JSON 预览必须双侧归一化：磁盘原文的键序/缩进与 serde_json 序列化不同，
    /// 若左栏用原文，整份文件都会被渲染成变更噪声（回归：接入预览渲染 json diff）。
    #[test]
    fn preview_texts_normalizes_both_sides_for_json() {
        // 键序刻意乱序 + 紧凑格式，模拟真实 ~/.claude.json 的任意排版。
        let content =
            r#"{"zeta":1,"mcpServers":{"other":{"url":"https://x/mcp"}},"alpha":{"b":2,"a":1}}"#;
        let loaded = LoadedConfig {
            path: std::path::PathBuf::from("/tmp/test.json"),
            format: ConfigFormat::Json,
            content: content.to_owned(),
            content_hash: mcpane_config::content_hash(content),
            mtime_unix_nanos: None,
            missing: false,
            formatting: mcpane_config::FileFormatting::detect(content),
        };
        let plan = mcpane_adapter_claude::hub_entry_patch(
            &["mcpServers".to_owned()],
            None,
            CredentialDelivery::EnvVar,
        );
        let (before, after) = preview_texts(&loaded, &plan).unwrap();

        // 双侧格式一致：未变更的行必须逐字相同，diff 只剩 mcpane 条目。
        let before_lines: std::collections::HashSet<&str> = before.lines().collect();
        let changed: Vec<&str> = after
            .lines()
            .filter(|line| !before_lines.contains(line))
            .collect();
        assert!(!changed.is_empty());
        for line in &changed {
            assert!(
                line.contains("mcpane")
                    || line.contains("mcp")
                    || line.trim_start().starts_with('"')
                    || line.trim() == "}"
                    || line.trim() == "},",
                "非 mcpane 相关行不应出现在变更中：{line}"
            );
        }
        // 原有键在归一化后的 before 里完整保留
        assert!(before.contains("\"zeta\""));
        assert!(before.contains("\"alpha\""));
        // after 含新写入的 mcpane 条目（env 引用形态）
        assert!(after.contains("\"mcpane\""));
        assert!(after.contains("MCPANE_CLAUDE_TOKEN"));
    }

    /// TOML（toml_edit 保留原格式）：归一化序列化与原文一致，行为不回退。
    #[test]
    fn preview_texts_keeps_toml_layout() {
        let content = "[mcp_servers.context7]\nurl = \"https://mcp.context7.com/mcp\"\n";
        let loaded = LoadedConfig {
            path: std::path::PathBuf::from("/tmp/config.toml"),
            format: ConfigFormat::Toml,
            content: content.to_owned(),
            content_hash: mcpane_config::content_hash(content),
            mtime_unix_nanos: None,
            missing: false,
            formatting: mcpane_config::FileFormatting::detect(content),
        };
        let plan = mcpane_adapter_codex::hub_entry_patch(None, CredentialDelivery::EnvVar);
        let (before, after) = preview_texts(&loaded, &plan).unwrap();
        assert!(before.starts_with(content.trim_start()));
        assert!(after.contains("bearer_token_env_var = \"MCPANE_CODEX_TOKEN\""));
    }
}

#[cfg(test)]
mod custom_client_tests {
    #![allow(clippy::unwrap_used)]

    use super::validate_custom_client;

    #[test]
    fn rejects_names_that_would_collide_with_builtin_clients() {
        // 专用 adapter 的两个（不在 CLIENTS 表里，最容易漏）。
        for id in ["claude", "codex"] {
            assert!(
                validate_custom_client(id, "X").is_err(),
                "{id} 与内置客户端重名，必须挡住"
            );
        }
        // CLIENTS 表整表遍历，而不是抄一份清单——新接一个客户端时，
        // 抄漏的那一条正是会静默放行的那一条。
        for spec in crate::mcp_clients::CLIENTS {
            assert!(
                validate_custom_client(spec.id, "X").is_err(),
                "{} 应被拒",
                spec.id
            );
        }
        assert!(
            crate::mcp_clients::CLIENTS.len() >= 10,
            "表被清空的话上面那圈遍历就什么都没验"
        );
    }

    #[test]
    fn rejects_empty_and_unsafe_ids() {
        assert!(validate_custom_client("", "X").is_err());
        assert!(validate_custom_client("  ", "X").is_err());
        assert!(validate_custom_client("my-cli", "").is_err());
        assert!(validate_custom_client("my-cli", "   ").is_err());
        // id 会成为 client_type 与 URL 片段，空格与符号一律拒。
        for bad in ["my cli", "trae!", "a/b", "中文"] {
            assert!(validate_custom_client(bad, "X").is_err(), "{bad} 应被拒");
        }
    }

    #[test]
    fn accepts_and_trims_valid_input() {
        let (id, display) = validate_custom_client("  trae-cn_2  ", "  Trae CN  ").unwrap();
        assert_eq!(id, "trae-cn_2");
        assert_eq!(display, "Trae CN");
    }
}
