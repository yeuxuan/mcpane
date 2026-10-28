//! 配置操作历史与回滚（TASK-506 数据层）。

use mcpane_config::ConfigFormat;
use mcpane_config::history::JsonlHistoryStore;

use super::connect::engine;
use super::mcpane_home;

// ---------- 配置操作历史与回滚（TASK-506 数据层） ----------

#[tauri::command]
pub async fn config_history_list() -> Result<Vec<mcpane_config::OperationRecord>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        use mcpane_config::HistoryStore;
        let store = JsonlHistoryStore::new(mcpane_home());
        let mut records = store.list().map_err(|e| e.to_string())?;
        records.reverse(); // 最新在前
        Ok(records)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 取走"配置被外部改动"的待读提醒（取走即清空）。
///
/// 前端挂载时拉一次：窗口关着（轻量模式已销毁 WebView）期间发生的改动，
/// 其 emit 没有接收方，只能靠这份留存补齐。
#[tauri::command]
pub async fn config_external_changes() -> Result<Vec<String>, String> {
    Ok(crate::config_watch::take_pending())
}

/// 回滚到某次操作的写前快照（回滚前引擎自动再备份当前文件）。
///
/// 只收记录 id，快照与目标路径由后端从历史库回查——此前直接信任前端传来的
/// 完整 OperationRecord（含 target_path 与 snapshot），渲染进程一旦被注入
/// 就能用任意快照覆盖任意路径的文件（IPC 信任边界破口）。
#[tauri::command]
pub async fn config_rollback(record_id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        use mcpane_config::HistoryStore;
        let store = JsonlHistoryStore::new(mcpane_home());
        let record = store
            .list()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|r| r.id == record_id)
            .ok_or("历史记录不存在（可能已被轮转清理）")?;
        let format = ConfigFormat::from_path(&record.target_path).ok_or("无法识别目标文件格式")?;
        let snapshot = record.snapshot.as_ref().ok_or("该操作没有快照（no-op）")?;
        engine()
            .rollback(snapshot, format)
            .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}
