//! 用量成本统计命令（Usage 页数据层）。
//!
//! 数据源是 Claude/Codex 本地会话文件（mcpane-usage 解析），落库与聚合在
//! mcpane-storage::UsageRepo。成本为本地定价表估算，非官方账单，UI 需标注。

use std::path::PathBuf;

use mcpane_storage::{UsageCursorRow, UsageEventRow, UsageRepo};
use mcpane_usage::{SyncCursor, UsageEvent};

use super::connect::user_home;

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn since_ts(hours: i64) -> i64 {
    // 上限一年：Usage 页最大窗口是 365 天
    now_secs() - hours.clamp(1, 24 * 366) * 3600
}

/// 解析产出 → 存储行。ts=0（原始 timestamp 缺失/坏）回落导入时刻。
fn to_row(ev: UsageEvent, imported_at: i64) -> UsageEventRow {
    UsageEventRow {
        request_id: ev.request_id,
        ts: if ev.ts > 0 { ev.ts } else { imported_at },
        client_app: ev.client_app.to_string(),
        model: ev.model,
        model_key: ev.model_key,
        project: ev.project,
        session_id: ev.session_id,
        input_tokens: ev.input_tokens,
        output_tokens: ev.output_tokens,
        cache_read_tokens: ev.cache_read_tokens,
        cache_creation_tokens: ev.cache_creation_tokens,
    }
}

#[derive(Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSyncResult {
    pub imported: u64,
    pub files_scanned: u64,
    pub errors: Vec<String>,
}

/// Codex 目录遵循 `CODEX_HOME` 语义（与 adapter-codex 的 discovery 一致）。
fn codex_dir() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join(".codex"))
}

/// 增量同步两个客户端的会话文件到 usage_events。
///
/// 每文件流程：查游标 → 解析（mtime 未变整文件跳过）→ 写事件 → 存新游标。
/// 单文件失败只记 error 不中断；request_id 主键 + INSERT OR IGNORE 保证
/// 重复扫描不双算。
/// Codex 解析器版本（mcpane-usage 计费口径变更时同步递增）：
/// 2 = thread-v2（last 优先 + limit_id 分源去重 + high-water，2026-08）。
const CODEX_PARSER_VERSION: i64 = 2;

#[tauri::command]
pub async fn usage_sync() -> Result<UsageSyncResult, String> {
    let repo = UsageRepo::new(super::db_pool().await?);
    let mut result = UsageSyncResult::default();

    // 解析算法升级后旧 Codex 事件是污染数据（多计/序号漂移），清掉重建；
    // 游标一并清空，本轮即全量重扫。
    if repo
        .reset_codex_if_parser_outdated(CODEX_PARSER_VERSION)
        .await
        .map_err(|e| e.to_string())?
    {
        eprintln!("[mcpane] Codex 用量解析器升级 → v{CODEX_PARSER_VERSION}，已清旧数据全量重建");
    }

    // 会话目录随 CLAUDE_CONFIG_DIR 走（用户设了 env 时目录整体搬家）。
    let claude_projects = mcpane_adapter_claude::claude_config_dir(&user_home()).join("projects");
    let claude_files = mcpane_usage::collect_claude_files(&claude_projects);
    for file in claude_files {
        let path_str = file.path.to_string_lossy().to_string();
        let parsed = {
            let prev = load_cursor(&repo, &path_str).await?;
            // 解析是纯阻塞 IO，大会话文件不该占 async runtime
            tauri::async_runtime::spawn_blocking(move || {
                mcpane_usage::parse_claude_file(&file, prev.as_ref())
            })
            .await
            .map_err(|e| e.to_string())?
        };
        commit_parse(&repo, &path_str, parsed, &mut result).await?;
    }

    let codex_files = mcpane_usage::collect_codex_files(&codex_dir());
    // fork/子代理文件要按父 rollout 的真实用量比对出重放段，先建 thread_id →
    // 路径索引（纯按文件名，不读内容）。整批共用一份，逐文件解析时借用。
    let rollout_index = std::sync::Arc::new(mcpane_usage::build_rollout_index(&codex_files));
    for path in codex_files {
        let path_str = path.to_string_lossy().to_string();
        let parsed = {
            let prev = load_cursor(&repo, &path_str).await?;
            let index = std::sync::Arc::clone(&rollout_index);
            tauri::async_runtime::spawn_blocking(move || {
                mcpane_usage::parse_codex_file(&path, prev.as_ref(), &index)
            })
            .await
            .map_err(|e| e.to_string())?
        };
        commit_parse(&repo, &path_str, parsed, &mut result).await?;
    }

    let gemini_files = mcpane_usage::collect_gemini_files(&user_home().join(".gemini"));
    for path in gemini_files {
        let path_str = path.to_string_lossy().to_string();
        let parsed = {
            let prev = load_cursor(&repo, &path_str).await?;
            tauri::async_runtime::spawn_blocking(move || {
                mcpane_usage::parse_gemini_file(&path, prev.as_ref())
            })
            .await
            .map_err(|e| e.to_string())?
        };
        commit_parse(&repo, &path_str, parsed, &mut result).await?;
    }

    Ok(result)
}

async fn load_cursor(repo: &UsageRepo, path_str: &str) -> Result<Option<SyncCursor>, String> {
    Ok(repo
        .get_cursor(path_str)
        .await
        .map_err(|e| e.to_string())?
        .map(|c| SyncCursor {
            last_modified_nanos: c.last_modified,
            last_line_offset: c.last_line_offset,
            last_size: c.last_size,
        }))
}

async fn commit_parse(
    repo: &UsageRepo,
    path_str: &str,
    parsed: Result<Option<mcpane_usage::FileParse>, mcpane_usage::UsageError>,
    result: &mut UsageSyncResult,
) -> Result<(), String> {
    result.files_scanned += 1;
    let parse = match parsed {
        Ok(Some(p)) => p,
        Ok(None) => return Ok(()), // mtime+size 未变，整文件跳过
        Err(e) => {
            result.errors.push(format!("{path_str}: {e}"));
            return Ok(());
        }
    };

    let now = now_secs();
    let rows: Vec<UsageEventRow> = parse.events.into_iter().map(|ev| to_row(ev, now)).collect();
    // 先写事件、后存游标：中途失败下轮重扫，靠主键去重兜底
    result.imported += repo.upsert_events(&rows).await.map_err(|e| e.to_string())?;
    repo.set_cursor(
        path_str,
        UsageCursorRow {
            last_modified: parse.cursor.last_modified_nanos,
            last_line_offset: parse.cursor.last_line_offset,
            last_size: parse.cursor.last_size,
        },
        now,
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 最近 N 小时汇总（token 四维 + 估算成本 + 无定价事件数）。
#[tauri::command]
pub async fn usage_summary(hours: i64) -> Result<mcpane_storage::UsageSummary, String> {
    UsageRepo::new(super::db_pool().await?)
        .summary(since_ts(hours))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn usage_by_model(hours: i64) -> Result<Vec<mcpane_storage::UsageGroupRow>, String> {
    UsageRepo::new(super::db_pool().await?)
        .by_model(since_ts(hours), 50)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn usage_by_project(hours: i64) -> Result<Vec<mcpane_storage::UsageGroupRow>, String> {
    UsageRepo::new(super::db_pool().await?)
        .by_project(since_ts(hours), 50)
        .await
        .map_err(|e| e.to_string())
}

/// 某模型的官方价与自定义价（「设置价格」弹窗用）。
#[tauri::command]
pub async fn usage_price_info(model_id: String) -> Result<mcpane_storage::ModelPriceInfo, String> {
    UsageRepo::new(super::db_pool().await?)
        .price_info(&model_id)
        .await
        .map_err(|e| e.to_string())
}

/// 设置某模型的自定义价格（USD / 百万 token）。按模型生效，不区分供应商（ADR-026）。
#[tauri::command]
pub async fn usage_price_override_set(
    model_id: String,
    prices: mcpane_storage::ModelPrices,
) -> Result<(), String> {
    UsageRepo::new(super::db_pool().await?)
        .set_price_override(&model_id, prices)
        .await
        .map_err(|e| e.to_string())
}

/// 清除自定义价格，回到官方价。
#[tauri::command]
pub async fn usage_price_override_clear(model_id: String) -> Result<(), String> {
    UsageRepo::new(super::db_pool().await?)
        .clear_price_override(&model_id)
        .await
        .map_err(|e| e.to_string())
}

/// provider 维度的降级形态：会话文件不含 provider，切换历史不足以可靠回溯
/// 「事件发生时生效的 provider」，MVP 按客户端（claude/codex）聚合，
/// 前端标注口径。
#[tauri::command]
pub async fn usage_by_provider(hours: i64) -> Result<Vec<mcpane_storage::UsageGroupRow>, String> {
    UsageRepo::new(super::db_pool().await?)
        .by_app(since_ts(hours))
        .await
        .map_err(|e| e.to_string())
}

/// 按本地日分桶的趋势。tz_offset_minutes 来自前端
/// `-new Date().getTimezoneOffset()`（东八区 +480）。
#[tauri::command]
pub async fn usage_trend(
    hours: i64,
    tz_offset_minutes: i64,
) -> Result<Vec<mcpane_storage::UsageTrendPoint>, String> {
    UsageRepo::new(super::db_pool().await?)
        .trend(
            since_ts(hours),
            tz_offset_minutes.clamp(-14 * 60, 14 * 60) * 60,
        )
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn usage_prune(days: i64) -> Result<u64, String> {
    UsageRepo::new(super::db_pool().await?)
        .prune_before(now_secs() - days.max(0) * 86400)
        .await
        .map_err(|e| e.to_string())
}
