//! 设置页「数据」分区的概览（磁盘占用 + 两张时间表的保留窗口）。
//!
//! 存在的理由：清理按钮此前只写死一句「清理 N 天前的日志」，用户既看不出
//! 现在攒了多少、也看不出点下去会掉多少——按钮成了没有反馈的动作。这里把
//! 「有多少 / 最早到什么时候 / 会清掉多少」一次算出来，界面才画得出保留窗口。

use super::mcpane_home;

/// 请求日志保留天数。**启动时的自动清理与设置页的手动清理共用它**——
/// 两处各写一个数就会出现界面承诺 7 天、实际按 30 天清的错位（曾经如此）。
pub const LOG_RETAIN_DAYS: i64 = 30;

/// 用量事件保留天数。比日志长得多：用量页默认看的是月度/季度趋势，
/// 按日志那套 30 天清会把趋势图的左半边削掉。
pub const USAGE_RETAIN_DAYS: i64 = 90;

/// 单张时间表的保留视图。`retain_days` 一并回传，前端的文案与保留条分界
/// 都读它——保留期的事实源只在 Rust 这一份。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionView {
    pub rows: i64,
    pub oldest_ts: Option<i64>,
    pub prunable: i64,
    pub retain_days: i64,
}

impl RetentionView {
    fn empty(retain_days: i64) -> Self {
        Self {
            rows: 0,
            oldest_ts: None,
            prunable: 0,
            retain_days,
        }
    }

    fn of(stats: mcpane_storage::RetentionStats, retain_days: i64) -> Self {
        Self {
            rows: stats.rows,
            oldest_ts: stats.oldest_ts,
            prunable: stats.prunable,
            retain_days,
        }
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataOverview {
    /// mcpane.db 连同 WAL/SHM 边车的磁盘占用（字节）。边车要算进去：
    /// WAL 在写入频繁时能长到主库的数倍，只报主库会低报实际占用。
    pub db_bytes: u64,
    pub logs: RetentionView,
    pub usage: RetentionView,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[tauri::command]
pub async fn data_overview() -> Result<DataOverview, String> {
    let db_path = mcpane_home().join("mcpane.db");
    let db_bytes = ["", "-wal", "-shm"]
        .iter()
        .filter_map(|suffix| {
            let mut name = db_path.as_os_str().to_owned();
            name.push(suffix);
            std::fs::metadata(std::path::PathBuf::from(name)).ok()
        })
        .map(|meta| meta.len())
        .sum();

    // 库还没建（全新安装、Hub 从未跑过）：报零而不是报错——数据分区
    // 这时候依然要能打开，只是每一格都是空的。
    if !db_path.exists() {
        return Ok(DataOverview {
            db_bytes,
            logs: RetentionView::empty(LOG_RETAIN_DAYS),
            usage: RetentionView::empty(USAGE_RETAIN_DAYS),
        });
    }

    let pool = super::db_pool().await?;
    let now = now_secs();
    let logs = mcpane_storage::LogRepo::new(pool.clone())
        .retention_stats(now - LOG_RETAIN_DAYS * 86400)
        .await
        .map_err(|e| e.to_string())?;
    let usage = mcpane_storage::UsageRepo::new(pool)
        .retention_stats(now - USAGE_RETAIN_DAYS * 86400)
        .await
        .map_err(|e| e.to_string())?;

    Ok(DataOverview {
        db_bytes,
        logs: RetentionView::of(logs, LOG_RETAIN_DAYS),
        usage: RetentionView::of(usage, USAGE_RETAIN_DAYS),
    })
}
