//! 按地址查找 MCP 服务器（ADR-025）：粘贴 GitHub 仓库 / npm / PyPI / 远程 URL，
//! 查官方 MCP Registry，返回可预填「添加服务器」表单的候选。只读，不执行任何东西。

use mcpane_registry::Resolution;

#[tauri::command]
pub async fn server_discover(query: String) -> Result<Resolution, String> {
    match mcpane_registry::discover(&query).await {
        Some(result) => result.map_err(|e| e.to_string()),
        None => Err("请输入 GitHub 仓库、npm / PyPI 包名或远程服务器地址".to_owned()),
    }
}
