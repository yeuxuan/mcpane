//! MCPane SQLite 存储（TASK-401 / TASK-407 持久层）。
//!
//! - SQLite WAL 模式，`mcpane.db`（冻结文件名）；
//! - 所有 Schema 变更走 sqlx migrations；
//! - Token 只存 SHA-256 哈希 + 前缀，验证走常量时间比较（ADR-003）。

pub mod backup;
pub mod connections;
pub mod custom_clients;
pub mod logs;
pub mod servers;
pub mod settings;
pub mod usage;
pub mod workspaces;

use std::path::Path;

pub use backup::{
    BackupEntry, RESTORE_PRESERVE_TABLES, SYNC_LOCAL_TABLES, backup_due, backup_now,
    db_migration_version, delete_backup, export_snapshot, import_snapshot, list_backups,
    prune_backups, replace_db_with_backup, restore_backup,
};
pub use connections::{ClientConnection, ConnectionRepo, NewConnection, generate_token};
pub use custom_clients::{CustomClientRepo, CustomMcpClient};
pub use logs::{LogGroupCount, LogRepo, LogStats, RequestLogEntry, summarize_json};
pub use servers::{McpServerDef, ServerRepo, ServerTransport};
pub use settings::SettingsRepo;
pub use sqlx::SqlitePool;
pub use usage::{
    MAX_PRICE_PER_MILLION, ModelPriceInfo, ModelPrices, PricingRow, UsageCursorRow, UsageEventRow,
    UsageGroupRow, UsageRepo, UsageSummary, UsageTrendPoint,
};
pub use workspaces::{ItemKind, ToolOverride, Workspace, WorkspaceRepo};

/// 保留窗口概览：一张按时间增长的表现在攒了多少、最早一条有多早、
/// 按当前保留期还剩多少会被清掉。日志与用量两张表共用同一形状。
///
/// 设置页拿它把「清理 30 天前的日志」从一句承诺变成看得见的量——
/// 用户点清理之前就知道会掉多少、掉的是哪一段时间。
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionStats {
    pub rows: i64,
    /// 最早一条的时间戳（秒）；表为空时为 None。
    pub oldest_ts: Option<i64>,
    /// 早于 cutoff 的条数（= 此刻点清理会删掉的数量）。
    pub prunable: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("数据库错误：{0}")]
    Db(#[from] sqlx::Error),
    #[error("{}", describe_migrate_error(.0))]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("记录不存在：{0}")]
    NotFound(String),
    /// 调用方给的值不合法（原因已是给用户看的文案）。
    #[error("{0}")]
    Invalid(String),
}

/// 本地库的 schema 比当前应用认识的还新（migration 记录存在但当前版本解析不出来）
/// 时，sqlx 报的是英文内部错误——多半发生在装了比之前用过的版本更旧的安装包。
/// 换成一句用户能看懂、能照做的话，而不是原样甩一句英文报错。
fn describe_migrate_error(error: &sqlx::migrate::MigrateError) -> String {
    use sqlx::migrate::MigrateError;
    match error {
        MigrateError::VersionMissing(v) | MigrateError::VersionMismatch(v) => format!(
            "本地数据库的结构版本（migration {v}）比当前 MCPane 版本新，通常是装了比之前用过的更旧的安装包。\
             请更新到最新版本；如果问题持续，可在「设置 → 本机备份」里恢复到更早的备份，或联系支持处理数据库文件。"
        ),
        MigrateError::VersionTooNew(applied, latest) => format!(
            "本地数据库的结构版本（{applied}）比当前 MCPane 认识的最新版本（{latest}）还新，通常是装了比之前用过的更旧的安装包。\
             请更新到最新版本后再打开。"
        ),
        other => format!("迁移错误：{other}"),
    }
}

/// 打开（或创建）数据库并执行迁移。WAL 开启。
pub async fn open_database(db_path: &Path) -> Result<SqlitePool, StorageError> {
    if let Some(parent) = db_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(std::time::Duration::from_secs(5));
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await?;
    // 迁移前保险：已有数据的旧库在 schema 升级前先落一份备份（best-effort），
    // 一次写坏的迁移否则无回退点。全新库不备份。
    let migrator = sqlx::migrate!("./migrations");
    let target_version = migrator.iter().map(|m| m.version).max().unwrap_or(0);
    let backup_home = db_path.parent().unwrap_or(db_path);
    backup::backup_before_migration(&pool, backup_home, target_version).await;
    migrator.run(&pool).await?;
    // 下游 HTTP Server 的 Bearer 明文入库（migrations/0003），db 必须 0600，
    // 不能以 umask 默认世界可读落盘。WAL/SHM 边车同样收紧（存在才夹）。
    harden_db_permissions(db_path);
    Ok(pool)
}

/// 把 SQLite 主库/快照文件及 WAL/SHM 边车文件权限收紧为仅属主可读写
/// （unix 0600；Windows 走显式 DACL）。`backup.rs` 的 `export_snapshot` 对每一份
/// 导出产物（本地备份、迁移前备份、云同步临时快照）复用同一份实现——它们与
/// 主库同属一个威胁模型：都可能含 `mcp_servers` 的下游 Bearer/API Key 明文（P0-2）。
///
/// 收紧动作统一委托 `mcpane_config::permissions`：那里的 Windows 分支已实现
/// DACL 收紧（默认安全描述符会额外给 Everyone 读权限，不能依赖），本 crate 的
/// `backup.rs` 早已在用同一入口，主库这条路径此前却还留着 unix-only 的
/// `set_permissions`，Windows 上的主库因此完全没有这层保护。
///
/// best-effort；失败不阻断启动（记一条 warn 即可）。
///
/// `pub`（而非 `pub(crate)`）：`mcpane-sync` 的 `download()` 把远端字节直接
/// `fs::write` 落盘，不经过 `export_snapshot`，需要独立调用这份实现收紧同一
/// 类文件，不重复一份权限逻辑。
pub fn harden_db_permissions(db_path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let target = if suffix.is_empty() {
            db_path.to_path_buf()
        } else {
            let mut name = db_path.as_os_str().to_owned();
            name.push(suffix);
            std::path::PathBuf::from(name)
        };
        if !target.exists() {
            continue;
        }
        if let Err(error) = mcpane_config::permissions::harden_to_owner_only(&target) {
            tracing::warn!(path = %target.display(), %error, "收紧 SQLite 文件权限失败");
        }
    }
}

/// 内存数据库（测试用，同样执行迁移）。
pub async fn open_in_memory() -> Result<SqlitePool, StorageError> {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

pub(crate) fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

#[cfg(all(test, unix))]
mod perm_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// 下游 Bearer 明文入库（migrations/0003），db 落盘必须 0600。
    #[tokio::test]
    async fn open_database_hardens_db_to_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("mcpane.db");
        let pool = open_database(&db_path).await.unwrap();
        let mode = std::fs::metadata(&db_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "SQLite 主库必须 0600");
        pool.close().await;
    }
}
