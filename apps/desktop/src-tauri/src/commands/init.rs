//! 数据库初始化失败的恢复面（对齐 cc-switch init_status / DatabaseUpgrade 模式）。
//!
//! `db_pool()` 保持懒加载与"失败不缓存、下次重试"语义；本模块只提供：
//! - `init_status`：主动探针一次（成功 = null；失败 = 分类 + 备份列表），
//!   前端据此在 App 顶层渲染全屏恢复页而非让每条命令各自裸报错；
//! - `init_replace_db_from_backup`：db 打不开时的文件级换库
//!   （`restore_backup` 是 ATTACH 数据级导入，前提是主库能打开，此处不适用）。

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitErrorInfo {
    /// version-too-new | corrupt | locked | other
    pub kind: &'static str,
    pub message: String,
    pub db_path: String,
    pub backups: Vec<mcpane_storage::BackupEntry>,
}

/// 按错误文本分类（sqlx/SQLite 的错误信息形态，测试锁住关键词）。
fn classify_init_error(message: &str) -> &'static str {
    let lower = message.to_lowercase();
    // sqlx MigrateError::VersionMissing：db 里记录的迁移在本程序中不存在
    // ——典型于降级安装（新版本建的库拿旧版本打开）。
    if lower.contains("previously applied") && lower.contains("missing") {
        return "version-too-new";
    }
    if lower.contains("malformed") || lower.contains("not a database") {
        return "corrupt";
    }
    if lower.contains("locked") {
        return "locked";
    }
    "other"
}

#[tauri::command]
pub async fn init_status() -> Result<Option<InitErrorInfo>, String> {
    match super::db_pool().await {
        Ok(_) => Ok(None),
        Err(message) => {
            let home = super::mcpane_home();
            Ok(Some(InitErrorInfo {
                kind: classify_init_error(&message),
                message,
                db_path: home.join("mcpane.db").display().to_string(),
                // 备份列表不依赖 db（纯目录扫描），恢复页才有东西可选。
                backups: mcpane_storage::list_backups(&home),
            }))
        }
    }
}

/// 前端根组件已挂载、首帧已提交：可以把启动时隐藏的主窗口亮出来了。
#[tauri::command]
pub fn window_ready(app: tauri::AppHandle) {
    crate::window::on_page_ready(&app);
}

/// 文件级换库（详见 `mcpane_storage::replace_db_with_backup` 的语义注释）。
/// 成功后进程内旧池/OnceCell 无法复用，前端引导用户重启应用。
#[tauri::command]
pub async fn init_replace_db_from_backup(file_name: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        mcpane_storage::replace_db_with_backup(&super::mcpane_home(), &file_name)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 分类关键词锁定：这些字符串来自 sqlx/SQLite 的真实错误文案，
    /// 上游换词会让分类退化为 other（恢复页仍可用，只是文案不那么对症）。
    #[test]
    fn init_error_classification_keywords() {
        assert_eq!(
            classify_init_error(
                "迁移错误：migration 20 was previously applied but is missing in the resolved migrations"
            ),
            "version-too-new"
        );
        assert_eq!(
            classify_init_error("数据库错误：database disk image is malformed"),
            "corrupt"
        );
        assert_eq!(
            classify_init_error("数据库错误：file is not a database"),
            "corrupt"
        );
        assert_eq!(
            classify_init_error("数据库错误：database is locked"),
            "locked"
        );
        assert_eq!(classify_init_error("莫名其妙"), "other");
    }
}
