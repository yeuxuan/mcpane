//! 客户端检测（TASK-501）。

use std::path::PathBuf;

use mcpane_config::ConfigFormat;
use mcpane_provider::ClientApp;

use super::connect::{config_has_hub_entry, user_home};
use super::provider::config_dir_for_app;

// ---------- 客户端检测（TASK-501） ----------

/// Claude Code / Codex 单客户端状态（安装 + 接入 + How To Use 所需信息）。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DedicatedClientStatus {
    installed: bool,
    version: Option<String>,
    /// 配置文件里已存在 mcpane Hub 条目。
    connected: bool,
    config_path: String,
    token_env: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientsStatus {
    claude: DedicatedClientStatus,
    codex: DedicatedClientStatus,
}

#[tauri::command]
pub async fn detect_clients() -> Result<ClientsStatus, String> {
    // 检测会执行外部命令 + 读配置，放到阻塞线程池。
    tauri::async_runtime::spawn_blocking(|| {
        let home = user_home();
        let claude_dir = config_dir_for_app(ClientApp::Claude);
        let codex_dir = config_dir_for_app(ClientApp::Codex);

        // 安装：与设置页 / 供应商页同一套判定（PATH + 常见 bin → 痕迹）。
        // 版本：走 adapter detect（要跑 --version）。只靠配置痕迹认出来的安装
        // 找不到可执行文件，version 仍可为 None——但不因此把 installed 打成 false。
        let claude_detect = mcpane_adapter_claude::detect_claude();
        let codex_detect = mcpane_adapter_codex::detect_codex();

        let claude_config =
            mcpane_adapter_claude::discover_config_paths_with(&home, None, claude_dir.as_deref())
                .user_config;
        let codex_home = codex_dir
            .clone()
            .or_else(|| std::env::var("CODEX_HOME").ok().map(PathBuf::from));
        let codex_config =
            mcpane_adapter_codex::discover_config_paths(&home, codex_home.as_deref(), None)
                .user_config;

        Ok(ClientsStatus {
            claude: DedicatedClientStatus {
                installed: mcpane_provider::is_installed_with_dir(
                    ClientApp::Claude,
                    claude_dir.as_deref(),
                ),
                version: claude_detect.and_then(|c| c.version),
                connected: config_has_hub_entry(
                    &claude_config,
                    ConfigFormat::Json,
                    &["mcpServers", mcpane_adapter_claude::MCP_ENTRY_NAME],
                ),
                config_path: claude_config.display().to_string(),
                token_env: mcpane_adapter_claude::TOKEN_ENV_VAR.to_owned(),
            },
            codex: DedicatedClientStatus {
                installed: mcpane_provider::is_installed_with_dir(
                    ClientApp::Codex,
                    codex_dir.as_deref(),
                ),
                version: codex_detect.and_then(|c| c.version),
                connected: config_has_hub_entry(
                    &codex_config,
                    ConfigFormat::Toml,
                    &["mcp_servers", mcpane_adapter_codex::MCP_ENTRY_NAME],
                ),
                config_path: codex_config.display().to_string(),
                token_env: mcpane_adapter_codex::TOKEN_ENV_VAR.to_owned(),
            },
        })
    })
    .await
    .map_err(|e: tauri::Error| e.to_string())?
}
