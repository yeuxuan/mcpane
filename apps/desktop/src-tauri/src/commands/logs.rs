//! 请求日志查询与清理（TASK-506 数据层）。

use super::mcpane_home;

// ---------- 请求日志（TASK-506 数据层） ----------

/// 读取 Hub 请求日志（与 Hub 共享同一 SQLite，WAL 支持多进程读）。
#[tauri::command]
pub async fn logs_recent(limit: i64) -> Result<Vec<mcpane_storage::RequestLogEntry>, String> {
    if !mcpane_home().join("mcpane.db").exists() {
        return Ok(Vec::new());
    }
    let logs = mcpane_storage::LogRepo::new(super::db_pool().await?)
        .recent(limit.clamp(1, 1000))
        .await
        .map_err(|e| e.to_string())?;
    Ok(logs)
}

/// 单条请求日志详情（含脱敏后的入参/出参摘要）。
#[tauri::command]
pub async fn logs_get(id: String) -> Result<mcpane_storage::RequestLogEntry, String> {
    if id.is_empty() || !mcpane_home().join("mcpane.db").exists() {
        return Err("记录不存在".into());
    }
    mcpane_storage::LogRepo::new(super::db_pool().await?)
        .get(&id)
        .await
        .map_err(|e| e.to_string())
}

/// 最近 N 小时的请求聚合统计（Logs 页仪表）。
#[tauri::command]
pub async fn logs_stats(hours: i64) -> Result<mcpane_storage::LogStats, String> {
    if !mcpane_home().join("mcpane.db").exists() {
        return Ok(mcpane_storage::LogStats {
            total: 0,
            success: 0,
            avg_duration_ms: 0.0,
            p95_duration_ms: 0,
            by_server: Vec::new(),
            by_method: Vec::new(),
            by_tool: Vec::new(),
            by_connection: Vec::new(),
        });
    }
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
        - hours.clamp(1, 24 * 30) * 3600;
    mcpane_storage::LogRepo::new(super::db_pool().await?)
        .stats(since)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn logs_prune(days: i64) -> Result<u64, String> {
    if !mcpane_home().join("mcpane.db").exists() {
        return Ok(0);
    }
    let pool = super::db_pool().await?;
    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
        - days.max(0) * 86400;
    mcpane_storage::LogRepo::new(pool)
        .prune_before(cutoff)
        .await
        .map_err(|e| e.to_string())
}

// ---------- 前端错误落盘 ----------

/// webview 侧错误写入 crash.log（window.onerror / unhandledrejection /
/// ErrorBoundary 三个入口共用）。频控在前端做（去重 + 上限），此处只负责
/// 截断脱敏后落盘；失败静默——日志通道自身不该产生新错误。
#[tauri::command]
pub async fn log_frontend_error(message: String) -> Result<(), String> {
    mcpane_hub_protocol::panic_hook::append_webview_record(env!("CARGO_PKG_VERSION"), &message);
    Ok(())
}
