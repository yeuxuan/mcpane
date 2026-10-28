//! 用量事件持久化与聚合（migrations/0012_usage.sql）。
//!
//! 事件由 mcpane-usage 解析产出、命令层转成 [`UsageEventRow`] 落库；本模块
//! 不依赖 mcpane-usage——存储层只认表结构，解析细节留在解析层。
//!
//! 成本不落库：查询时按 model_key LEFT JOIN model_pricing 动态估算，
//! 定价表更新后历史成本自动重算。计费口径（对齐 cc-switch）：
//! - Claude 的 input_tokens 已是 fresh input，直接计费；
//! - Codex/Gemini 的 input_tokens 含 cache，计费前扣除 cache_read + cache_creation。
//!
//! 展示口径同样归一：input_tokens 列走 [`FRESH_INPUT_EXPR`]，把 Codex/Gemini 的
//! 含 cache 值扣成 fresh，跨客户端汇总数字才不虚高（cache/output 列保持原值）。
//!
//! 未命中定价的事件成本记 0，由 unpriced/priced 字段暴露给 UI 标注。
//! 用户可对单个模型设价格覆盖（`model_pricing_override`，整行生效、优先于官方价、按模型不分供应商）。

use sqlx::Row;

use crate::{RetentionStats, SqlitePool, StorageError};

/// usage_events 一行。字段语义见 0012_usage.sql。
#[derive(Debug, Clone)]
pub struct UsageEventRow {
    pub request_id: String,
    pub ts: i64,
    pub client_app: String,
    pub model: String,
    pub model_key: String,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
}

/// model_pricing 一行（价格列为十进制字符串，与迁移种子同形态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PricingRow {
    pub model_id: String,
    pub display_name: String,
    pub input_per_million: String,
    pub output_per_million: String,
    pub cache_read_per_million: String,
    pub cache_creation_per_million: String,
}

/// 一个模型的四项价格（USD / 百万 token）。
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPrices {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_creation: f64,
}

/// 价格上限（USD / 百万 token）：挡住手滑多打的零，远高于任何真实模型的标价。
pub const MAX_PRICE_PER_MILLION: f64 = 1_000_000.0;

/// 设置价格弹窗要的两份价格：官方价（定价表）与当前覆盖。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPriceInfo {
    pub official: Option<ModelPrices>,
    pub custom: Option<ModelPrices>,
}

/// usage_sync_state 一行（增量同步游标）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageCursorRow {
    /// 文件 mtime 纳秒。
    pub last_modified: i64,
    /// 已处理行数高水位。
    pub last_line_offset: i64,
    /// 上次扫描时的文件字节数（0 = 未记录）。
    pub last_size: i64,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub events: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    /// 估算成本（USD）。定价缺失的事件按 0 计入。
    pub cost_usd: f64,
    /// 未命中定价表的事件数（>0 时 UI 标注「部分模型无定价」）。
    pub unpriced_events: i64,
}

/// 按 model / project / client_app 聚合的一行。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageGroupRow {
    pub key: String,
    /// 仅按 model 聚合时有值（取定价表 display_name）。
    pub display_name: Option<String>,
    pub events: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cost_usd: f64,
    /// 该组是否命中定价或有自定义价格（false 时成本为 0，UI 标注「无定价」）。
    pub priced: bool,
    /// 该组（仅按模型聚合时）是否在用用户自定义价格。
    pub overridden: bool,
}

/// 趋势图一个时间桶（本地时区日边界，由 tz_offset_secs 决定）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTrendPoint {
    /// 桶起点的 UTC 秒（= 本地日 00:00 对应的 UTC 时刻）。
    pub bucket_ts: i64,
    pub events: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cost_usd: f64,
}

/// 单事件估算成本（USD）的 SQL 表达式。要求 FROM 子句里
/// usage_events 别名 e、model_pricing LEFT JOIN 别名 p、model_pricing_override 别名 o。
/// 定价 TEXT 存储防浮点漂移，聚合时 CAST 成 REAL；价格取 自定义 → 官方 → 0。
/// 所有聚合（总览/趋势/按模型/按应用/按项目）共用这一处，覆盖才不会只在某个维度生效。
const COST_EXPR: &str = "(CASE WHEN e.client_app IN ('codex', 'gemini')
        THEN MAX(e.input_tokens - e.cache_read_tokens - e.cache_creation_tokens, 0)
        ELSE e.input_tokens END
        * CAST(COALESCE(o.input_per_million, p.input_per_million, '0') AS REAL)
     + e.output_tokens
        * CAST(COALESCE(o.output_per_million, p.output_per_million, '0') AS REAL)
     + e.cache_read_tokens
        * CAST(COALESCE(o.cache_read_per_million, p.cache_read_per_million, '0') AS REAL)
     + e.cache_creation_tokens
        * CAST(COALESCE(o.cache_creation_per_million, p.cache_creation_per_million, '0') AS REAL)
    ) / 1000000.0";

const FROM_JOINED: &str = "FROM usage_events e
     LEFT JOIN model_pricing p ON p.model_id = e.model_key
     LEFT JOIN model_pricing_override o ON o.model_id = e.model_key";

/// 展示口径归一的 input_tokens 表达式（对齐 cc-switch fresh_input_sql）。
/// Claude 的 input_tokens 本就是 fresh（不含 cache），直接取值；
/// Codex/Gemini 的 input_tokens 含 cache（cache_read ⊆ input），跨客户端汇总
/// 前扣除 cache_read + cache_creation 归一到 fresh，否则 cache 命中越多数字越
/// 虚高。扣减为负（异常数据）回退原值，避免虚低。要求 usage_events 别名 e。
const FRESH_INPUT_EXPR: &str = "(CASE
        WHEN e.client_app = 'claude' THEN e.input_tokens
        WHEN e.input_tokens - e.cache_read_tokens - e.cache_creation_tokens < 0
            THEN e.input_tokens
        ELSE e.input_tokens - e.cache_read_tokens - e.cache_creation_tokens
     END)";

/// 分组/趋势共用的聚合列（token 四维 + 事件数 + 估算成本）。
fn group_agg() -> String {
    format!(
        "COUNT(*) AS events,
         COALESCE(SUM({FRESH_INPUT_EXPR}), 0) AS input_tokens,
         COALESCE(SUM(e.output_tokens), 0) AS output_tokens,
         COALESCE(SUM(e.cache_read_tokens), 0) AS cache_read_tokens,
         COALESCE(SUM(e.cache_creation_tokens), 0) AS cache_creation_tokens,
         COALESCE(SUM({COST_EXPR}), 0.0) AS cost_usd"
    )
}

pub struct UsageRepo {
    pool: SqlitePool,
}

impl UsageRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 批量写入事件，INSERT OR IGNORE 按 request_id 去重（重复扫描不双算）。
    /// 返回真实新增条数。
    pub async fn upsert_events(&self, events: &[UsageEventRow]) -> Result<u64, StorageError> {
        let mut tx = self.pool.begin().await?;
        let mut inserted = 0u64;
        for ev in events {
            let result = sqlx::query(
                "INSERT OR IGNORE INTO usage_events
                 (request_id, ts, client_app, model, model_key, project, session_id,
                  input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&ev.request_id)
            .bind(ev.ts)
            .bind(&ev.client_app)
            .bind(&ev.model)
            .bind(&ev.model_key)
            .bind(&ev.project)
            .bind(&ev.session_id)
            .bind(ev.input_tokens)
            .bind(ev.output_tokens)
            .bind(ev.cache_read_tokens)
            .bind(ev.cache_creation_tokens)
            .execute(&mut *tx)
            .await?;
            inserted += result.rows_affected();
        }
        tx.commit().await?;
        Ok(inserted)
    }

    /// 定价同步的目标键集：实际用过的 model_key ∪ 定价表既有 model_id。
    /// 同步只更新这个集合（用户真用到的模型），不整表灌入 models.dev 全量。
    pub async fn pricing_target_keys(&self) -> Result<Vec<String>, StorageError> {
        let rows = sqlx::query(
            "SELECT model_key AS k FROM usage_events
             UNION SELECT model_id AS k FROM model_pricing",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| r.get("k")).collect())
    }

    /// Codex 解析器版本闸门：落后即清 Codex 事件 + 全部游标（单事务），
    /// 触发下一轮全量重扫重建。返回是否发生了重建。
    ///
    /// 只删 Codex 事件——旧算法的多计/序号漂移无法原地修正；游标全清是
    /// 安全的：Claude/Gemini 事件的 request_id 稳定，重扫经 INSERT OR IGNORE
    /// 幂等去重，只多付一次扫描 IO。
    pub async fn reset_codex_if_parser_outdated(
        &self,
        current_version: i64,
    ) -> Result<bool, StorageError> {
        let mut tx = self.pool.begin().await?;
        let stored: i64 = sqlx::query("SELECT codex_parser_version FROM app_settings WHERE id = 1")
            .fetch_one(&mut *tx)
            .await?
            .get("codex_parser_version");
        if stored >= current_version {
            tx.commit().await?;
            return Ok(false);
        }
        sqlx::query("DELETE FROM usage_events WHERE client_app = 'codex'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM usage_sync_state")
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE app_settings SET codex_parser_version = ? WHERE id = 1")
            .bind(current_version)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// 定价表既有 model_id（同步时区分「可覆写」与「只补缺」用）。
    pub async fn pricing_model_ids(&self) -> Result<Vec<String>, StorageError> {
        let rows = sqlx::query("SELECT model_id FROM model_pricing")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| r.get("model_id")).collect())
    }

    /// 批量覆写定价行（INSERT OR REPLACE，单事务）。返回写入条数。
    pub async fn pricing_upsert_batch(&self, rows: &[PricingRow]) -> Result<u64, StorageError> {
        let mut tx = self.pool.begin().await?;
        let mut written = 0u64;
        for row in rows {
            let result = sqlx::query(
                "INSERT OR REPLACE INTO model_pricing
                 (model_id, display_name, input_per_million, output_per_million,
                  cache_read_per_million, cache_creation_per_million)
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(&row.model_id)
            .bind(&row.display_name)
            .bind(&row.input_per_million)
            .bind(&row.output_per_million)
            .bind(&row.cache_read_per_million)
            .bind(&row.cache_creation_per_million)
            .execute(&mut *tx)
            .await?;
            written += result.rows_affected();
        }
        tx.commit().await?;
        Ok(written)
    }

    /// 某模型的官方价与当前自定义价（设置价格弹窗用）。
    pub async fn price_info(&self, model_id: &str) -> Result<ModelPriceInfo, StorageError> {
        Ok(ModelPriceInfo {
            official: self.read_prices("model_pricing", model_id).await?,
            custom: self.read_prices("model_pricing_override", model_id).await?,
        })
    }

    async fn read_prices(
        &self,
        table: &'static str,
        model_id: &str,
    ) -> Result<Option<ModelPrices>, StorageError> {
        // 表名只来自上面两个字面量，不是用户输入。
        let sql = format!(
            "SELECT input_per_million, output_per_million,
                    cache_read_per_million, cache_creation_per_million
             FROM {table} WHERE model_id = ?"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(model_id)
            .fetch_optional(&self.pool)
            .await?;
        let parse = |row: &sqlx::sqlite::SqliteRow, column: &str| -> f64 {
            row.get::<String, _>(column).parse().unwrap_or(0.0)
        };
        Ok(row.map(|r| ModelPrices {
            input: parse(&r, "input_per_million"),
            output: parse(&r, "output_per_million"),
            cache_read: parse(&r, "cache_read_per_million"),
            cache_creation: parse(&r, "cache_creation_per_million"),
        }))
    }

    /// 设置某模型的自定义价格（整行覆盖官方价）。
    ///
    /// 只接受用量里出现过的 `model_key`：覆盖是对「这个模型的成本显示」的修正，
    /// 不开放任意键，免得写进去一个永远命不中的死行。
    pub async fn set_price_override(
        &self,
        model_id: &str,
        prices: ModelPrices,
    ) -> Result<(), StorageError> {
        for (label, value) in [
            ("输入", prices.input),
            ("输出", prices.output),
            ("缓存读取", prices.cache_read),
            ("缓存写入", prices.cache_creation),
        ] {
            if !value.is_finite() || !(0.0..=MAX_PRICE_PER_MILLION).contains(&value) {
                return Err(StorageError::Invalid(format!(
                    "{label}价格需要是 0 到 {MAX_PRICE_PER_MILLION} 之间的数字（美元 / 百万 token）"
                )));
            }
        }
        let seen = sqlx::query("SELECT 1 FROM usage_events WHERE model_key = ? LIMIT 1")
            .bind(model_id)
            .fetch_optional(&self.pool)
            .await?;
        if seen.is_none() {
            return Err(StorageError::NotFound(format!(
                "用量里没有出现过模型 {model_id}"
            )));
        }
        // f64 的 Display 是最短往返表示且不带指数，写成十进制字符串与官方价同形态。
        sqlx::query(
            "INSERT OR REPLACE INTO model_pricing_override
             (model_id, input_per_million, output_per_million,
              cache_read_per_million, cache_creation_per_million)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(model_id)
        .bind(prices.input.to_string())
        .bind(prices.output.to_string())
        .bind(prices.cache_read.to_string())
        .bind(prices.cache_creation.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 清除自定义价格，回到官方价（没有覆盖时也算成功）。
    pub async fn clear_price_override(&self, model_id: &str) -> Result<(), StorageError> {
        sqlx::query("DELETE FROM model_pricing_override WHERE model_id = ?")
            .bind(model_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_cursor(
        &self,
        file_path: &str,
    ) -> Result<Option<UsageCursorRow>, StorageError> {
        let row = sqlx::query(
            "SELECT last_modified, last_line_offset, last_size FROM usage_sync_state WHERE file_path = ?",
        )
        .bind(file_path)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| UsageCursorRow {
            last_modified: r.get("last_modified"),
            last_line_offset: r.get("last_line_offset"),
            last_size: r.get("last_size"),
        }))
    }

    pub async fn set_cursor(
        &self,
        file_path: &str,
        cursor: UsageCursorRow,
        synced_at: i64,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT OR REPLACE INTO usage_sync_state
             (file_path, last_modified, last_line_offset, last_size, last_synced_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(file_path)
        .bind(cursor.last_modified)
        .bind(cursor.last_line_offset)
        .bind(cursor.last_size)
        .bind(synced_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 时间窗汇总。SQL 侧聚合，不拉全量到应用层。
    pub async fn summary(&self, since_ts: i64) -> Result<UsageSummary, StorageError> {
        let sql = format!(
            "SELECT COUNT(*) AS events,
                    COALESCE(SUM({FRESH_INPUT_EXPR}), 0) AS input_tokens,
                    COALESCE(SUM(e.output_tokens), 0) AS output_tokens,
                    COALESCE(SUM(e.cache_read_tokens), 0) AS cache_read_tokens,
                    COALESCE(SUM(e.cache_creation_tokens), 0) AS cache_creation_tokens,
                    COALESCE(SUM({COST_EXPR}), 0.0) AS cost_usd,
                    COALESCE(SUM(CASE WHEN p.model_id IS NULL AND o.model_id IS NULL
                                      THEN 1 ELSE 0 END), 0) AS unpriced
             {FROM_JOINED}
             WHERE e.ts >= ?"
        );
        // SQL 由编译期常量拼接（COST_EXPR/FROM_JOINED），无用户输入，断言安全。
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(since_ts)
            .fetch_one(&self.pool)
            .await?;
        Ok(UsageSummary {
            events: row.get("events"),
            input_tokens: row.get("input_tokens"),
            output_tokens: row.get("output_tokens"),
            cache_read_tokens: row.get("cache_read_tokens"),
            cache_creation_tokens: row.get("cache_creation_tokens"),
            cost_usd: row.get("cost_usd"),
            unpriced_events: row.get("unpriced"),
        })
    }

    /// 按归一化 model 聚合（成本降序）。display_name 取定价表；未命中定价的
    /// 组 priced=false。
    pub async fn by_model(
        &self,
        since_ts: i64,
        limit: i64,
    ) -> Result<Vec<UsageGroupRow>, StorageError> {
        let agg = group_agg();
        let sql = format!(
            "SELECT e.model_key AS key, MAX(p.display_name) AS display_name,
                    (MAX(p.model_id) IS NOT NULL OR MAX(o.model_id) IS NOT NULL) AS priced,
                    (MAX(o.model_id) IS NOT NULL) AS overridden, {agg}
             {FROM_JOINED}
             WHERE e.ts >= ?
             GROUP BY e.model_key ORDER BY cost_usd DESC, input_tokens DESC LIMIT ?"
        );
        self.fetch_groups(sql, since_ts, limit, true).await
    }

    /// 按 Claude 项目聚合（project 为 NULL 的 Codex 事件不在此维度）。
    pub async fn by_project(
        &self,
        since_ts: i64,
        limit: i64,
    ) -> Result<Vec<UsageGroupRow>, StorageError> {
        let agg = group_agg();
        let sql = format!(
            "SELECT e.project AS key, NULL AS display_name, 1 AS priced, 0 AS overridden, {agg}
             {FROM_JOINED}
             WHERE e.ts >= ? AND e.project IS NOT NULL
             GROUP BY e.project ORDER BY cost_usd DESC, input_tokens DESC LIMIT ?"
        );
        self.fetch_groups(sql, since_ts, limit, false).await
    }

    /// 按客户端（claude/codex）聚合。provider 维度的降级形态：会话文件不含
    /// provider，切换历史（config-operations.jsonl，自由文本 + 500 条轮转）
    /// 不足以可靠回溯「当时生效的 provider」，MVP 按客户端聚合并由前端标注。
    pub async fn by_app(&self, since_ts: i64) -> Result<Vec<UsageGroupRow>, StorageError> {
        let agg = group_agg();
        let sql = format!(
            "SELECT e.client_app AS key, NULL AS display_name, 1 AS priced, 0 AS overridden, {agg}
             {FROM_JOINED}
             WHERE e.ts >= ?
             GROUP BY e.client_app ORDER BY cost_usd DESC LIMIT ?"
        );
        self.fetch_groups(sql, since_ts, 16, false).await
    }

    async fn fetch_groups(
        &self,
        sql: String,
        since_ts: i64,
        limit: i64,
        with_pricing_meta: bool,
    ) -> Result<Vec<UsageGroupRow>, StorageError> {
        // SQL 由编译期常量拼接，无用户输入，断言安全。
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(since_ts)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| UsageGroupRow {
                key: r.get("key"),
                display_name: if with_pricing_meta {
                    r.get("display_name")
                } else {
                    None
                },
                events: r.get("events"),
                input_tokens: r.get("input_tokens"),
                output_tokens: r.get("output_tokens"),
                cache_read_tokens: r.get("cache_read_tokens"),
                cache_creation_tokens: r.get("cache_creation_tokens"),
                cost_usd: r.get("cost_usd"),
                priced: r.get::<i64, _>("priced") == 1,
                overridden: r.get::<i64, _>("overridden") == 1,
            })
            .collect())
    }

    /// 按「本地日」分桶的趋势。tz_offset_secs 为本地时区相对 UTC 的秒偏移
    /// （东八区 +28800），桶起点返回该本地日 00:00 对应的 UTC 秒。
    pub async fn trend(
        &self,
        since_ts: i64,
        tz_offset_secs: i64,
    ) -> Result<Vec<UsageTrendPoint>, StorageError> {
        let agg = group_agg();
        let sql = format!(
            "SELECT ((e.ts + ?) / 86400) * 86400 - ? AS bucket_ts,
                    {agg}
             {FROM_JOINED}
             WHERE e.ts >= ?
             GROUP BY bucket_ts ORDER BY bucket_ts"
        );
        // SQL 由编译期常量拼接，无用户输入，断言安全。
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(tz_offset_secs)
            .bind(tz_offset_secs)
            .bind(since_ts)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| UsageTrendPoint {
                bucket_ts: r.get("bucket_ts"),
                events: r.get("events"),
                input_tokens: r.get("input_tokens"),
                output_tokens: r.get("output_tokens"),
                cache_read_tokens: r.get("cache_read_tokens"),
                cache_creation_tokens: r.get("cache_creation_tokens"),
                cost_usd: r.get("cost_usd"),
            })
            .collect())
    }

    /// 保留窗口概览（设置页数据分区），语义同 [`crate::LogRepo::retention_stats`]。
    pub async fn retention_stats(&self, cutoff: i64) -> Result<RetentionStats, StorageError> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS total, MIN(ts) AS oldest, \
             COALESCE(SUM(CASE WHEN ts < ? THEN 1 ELSE 0 END), 0) AS prunable \
             FROM usage_events",
        )
        .bind(cutoff)
        .fetch_one(&self.pool)
        .await?;
        Ok(RetentionStats {
            rows: row.get("total"),
            oldest_ts: row.get("oldest"),
            prunable: row.get("prunable"),
        })
    }

    /// 保留策略：删除早于 ts 的事件，返回删除事件数。
    /// **不动 usage_sync_state 游标**：游标按文件路径记高水位，删了会让文件下轮
    /// 从头重扫、把刚 prune 掉的旧事件（源文件仍在）重新插回，等于撤销本次 prune。
    pub async fn prune_before(&self, ts: i64) -> Result<u64, StorageError> {
        let result = sqlx::query("DELETE FROM usage_events WHERE ts < ?")
            .bind(ts)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::open_in_memory;

    fn ev(request_id: &str, ts: i64, app: &str, model_key: &str) -> UsageEventRow {
        UsageEventRow {
            request_id: request_id.to_string(),
            ts,
            client_app: app.to_string(),
            model: model_key.to_string(),
            model_key: model_key.to_string(),
            project: (app == "claude").then(|| "-Users-me-proj".to_string()),
            session_id: Some("sess".to_string()),
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_read_tokens: 400_000,
            cache_creation_tokens: 0,
        }
    }

    #[tokio::test]
    async fn upsert_dedups_by_request_id() {
        let repo = UsageRepo::new(open_in_memory().await.unwrap());
        let events = vec![
            ev("session:a", 100, "claude", "claude-sonnet-5"),
            ev("session:a", 100, "claude", "claude-sonnet-5"),
            ev("session:b", 100, "claude", "claude-sonnet-5"),
        ];
        assert_eq!(repo.upsert_events(&events).await.unwrap(), 2);
        // 重放同一批：全部 IGNORE
        assert_eq!(repo.upsert_events(&events).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn cursor_roundtrip() {
        let repo = UsageRepo::new(open_in_memory().await.unwrap());
        assert!(repo.get_cursor("/a.jsonl").await.unwrap().is_none());
        let cursor = UsageCursorRow {
            last_modified: 123_456_789,
            last_line_offset: 42,
            last_size: 4_096,
        };
        repo.set_cursor("/a.jsonl", cursor, 1_000).await.unwrap();
        assert_eq!(repo.get_cursor("/a.jsonl").await.unwrap(), Some(cursor));
        // 覆盖更新
        let newer = UsageCursorRow {
            last_modified: 223_456_789,
            last_line_offset: 99,
            last_size: 8_192,
        };
        repo.set_cursor("/a.jsonl", newer, 2_000).await.unwrap();
        assert_eq!(repo.get_cursor("/a.jsonl").await.unwrap(), Some(newer));
    }

    #[tokio::test]
    async fn cost_semantics_claude_full_input_codex_deducts_cache() {
        let repo = UsageRepo::new(open_in_memory().await.unwrap());
        // Claude sonnet-5：input 3 + output 15 + cache_read 0.4*0.30 = 18.12
        let claude = ev("session:c", 100, "claude", "claude-sonnet-5");
        // Codex gpt-5：billable input = 1M - 0.4M = 0.6M → 0.6*1.25 = 0.75
        //             + output 1M*10 = 10 + cache_read 0.4M*0.125 = 0.05 → 10.8
        let codex = ev("codex_session:thread-v1:t:1", 100, "codex", "gpt-5");
        repo.upsert_events(&[claude, codex]).await.unwrap();

        let summary = repo.summary(0).await.unwrap();
        assert_eq!(summary.events, 2);
        assert_eq!(summary.unpriced_events, 0);
        let expected_claude = 3.0 + 15.0 + 0.4 * 0.30;
        let expected_codex = 0.6 * 1.25 + 10.0 + 0.4 * 0.125;
        assert!((summary.cost_usd - (expected_claude + expected_codex)).abs() < 1e-9);

        // 展示口径归一：Claude input 取原值 1M，Codex input 扣 cache → 0.6M。
        // 汇总 input = 1M + 0.6M = 1.6M（若不归一则虚高为 2M）。
        assert_eq!(summary.input_tokens, 1_000_000 + 600_000);
        // cache/output 列不受归一影响，仍是原值累加。
        assert_eq!(summary.output_tokens, 2_000_000);
        assert_eq!(summary.cache_read_tokens, 800_000);

        let by_model = repo.by_model(0, 10).await.unwrap();
        assert_eq!(by_model.len(), 2);
        // 成本降序：claude-sonnet-5 (18.12) > gpt-5 (10.8)
        assert_eq!(by_model[0].key, "claude-sonnet-5");
        assert_eq!(by_model[0].display_name.as_deref(), Some("Claude Sonnet 5"));
        assert!(by_model[0].priced);
        assert!((by_model[0].cost_usd - expected_claude).abs() < 1e-9);
        assert!((by_model[1].cost_usd - expected_codex).abs() < 1e-9);
        // 分组展示列同样归一：Claude 组 input 原值，gpt-5 组 input 扣 cache。
        assert_eq!(
            by_model[0].input_tokens, 1_000_000,
            "Claude 展示 fresh=原值"
        );
        assert_eq!(
            by_model[1].input_tokens, 600_000,
            "Codex 展示 fresh=扣 cache"
        );
    }

    #[tokio::test]
    async fn fresh_input_falls_back_to_raw_when_deduction_negative() {
        let repo = UsageRepo::new(open_in_memory().await.unwrap());
        // 异常数据：Codex 事件 cache_read 超过 input（扣减为负）→ 展示回退原值，
        // 不出现负数 token。
        let mut bad = ev("codex_session:x:1", 100, "codex", "gpt-5");
        bad.input_tokens = 100;
        bad.cache_read_tokens = 500;
        bad.cache_creation_tokens = 0;
        repo.upsert_events(&[bad]).await.unwrap();
        let summary = repo.summary(0).await.unwrap();
        assert_eq!(summary.input_tokens, 100, "扣减为负回退原值");
    }

    #[tokio::test]
    async fn gemini_billable_input_deducts_cache_like_codex() {
        let repo = UsageRepo::new(open_in_memory().await.unwrap());
        // Gemini 2.5 Pro：billable input = 1M - 0.4M = 0.6M → 0.6*1.25 = 0.75
        //               + output 1M*10 = 10 + cache_read 0.4M*0.125 = 0.05 → 10.8
        repo.upsert_events(&[ev("gemini_session:s:m1", 100, "gemini", "gemini-2.5-pro")])
            .await
            .unwrap();
        let summary = repo.summary(0).await.unwrap();
        assert_eq!(
            summary.unpriced_events, 0,
            "0013 迁移的 gemini 定价必须命中"
        );
        let expected = 0.6 * 1.25 + 10.0 + 0.4 * 0.125;
        assert!((summary.cost_usd - expected).abs() < 1e-9);
    }

    #[tokio::test]
    async fn unpriced_model_costs_zero_and_is_flagged() {
        let repo = UsageRepo::new(open_in_memory().await.unwrap());
        repo.upsert_events(&[ev("session:x", 100, "claude", "mystery-model-9000")])
            .await
            .unwrap();
        let summary = repo.summary(0).await.unwrap();
        assert_eq!(summary.unpriced_events, 1);
        assert_eq!(summary.cost_usd, 0.0);
        let by_model = repo.by_model(0, 10).await.unwrap();
        assert!(!by_model[0].priced);
        assert_eq!(by_model[0].cost_usd, 0.0);
    }

    #[tokio::test]
    async fn codex_parser_gate_resets_only_codex_events_and_all_cursors() {
        let repo = UsageRepo::new(open_in_memory().await.unwrap());
        repo.upsert_events(&[
            ev("codex_session:thread-v1:t:1", 100, "codex", "gpt-5.6"),
            ev("session:c1", 100, "claude", "claude-sonnet-5"),
        ])
        .await
        .unwrap();
        repo.set_cursor(
            "/tmp/rollout-x.jsonl",
            UsageCursorRow {
                last_modified: 1,
                last_line_offset: 9,
                last_size: 0,
            },
            100,
        )
        .await
        .unwrap();

        // 版本相同：无动作。
        assert!(!repo.reset_codex_if_parser_outdated(1).await.unwrap());
        assert_eq!(repo.summary(0).await.unwrap().events, 2);

        // 版本落后：codex 事件清零、claude 保留、游标全清；且只重建一次。
        assert!(repo.reset_codex_if_parser_outdated(2).await.unwrap());
        let by_app = repo.by_app(0).await.unwrap();
        assert!(by_app.iter().all(|row| row.key != "codex"));
        assert!(by_app.iter().any(|row| row.key == "claude"));
        assert!(
            repo.get_cursor("/tmp/rollout-x.jsonl")
                .await
                .unwrap()
                .is_none()
        );
        assert!(!repo.reset_codex_if_parser_outdated(2).await.unwrap());
    }

    #[tokio::test]
    async fn pricing_sync_targets_and_batch_replace() {
        let repo = UsageRepo::new(open_in_memory().await.unwrap());
        repo.upsert_events(&[ev("session:x", 100, "claude", "mystery-model-9000")])
            .await
            .unwrap();
        // 目标集 = 用过的键 ∪ 定价表既有键（种子行）。
        let targets = repo.pricing_target_keys().await.unwrap();
        assert!(targets.iter().any(|k| k == "mystery-model-9000"));
        assert!(targets.iter().any(|k| k == "claude-sonnet-5"));

        // 覆写补价后 unpriced 清零、成本按新价计。
        repo.pricing_upsert_batch(&[PricingRow {
            model_id: "mystery-model-9000".into(),
            display_name: "Mystery 9000".into(),
            input_per_million: "1".into(),
            output_per_million: "2".into(),
            cache_read_per_million: "0.1".into(),
            cache_creation_per_million: "0".into(),
        }])
        .await
        .unwrap();
        let summary = repo.summary(0).await.unwrap();
        assert_eq!(summary.unpriced_events, 0);
        assert!(summary.cost_usd > 0.0);
    }

    fn prices(input: f64, output: f64, cache_read: f64, cache_creation: f64) -> ModelPrices {
        ModelPrices {
            input,
            output,
            cache_read,
            cache_creation,
        }
    }

    #[tokio::test]
    async fn override_beats_official_price_and_clearing_falls_back() {
        let repo = UsageRepo::new(open_in_memory().await.unwrap());
        repo.upsert_events(&[ev("session:o1", 100, "claude", "claude-sonnet-5")])
            .await
            .unwrap();
        let official = 3.0 + 15.0 + 0.4 * 0.30;
        assert!((repo.summary(0).await.unwrap().cost_usd - official).abs() < 1e-9);

        // 整行生效：缓存两项没填就是 0，不会回落到官方价。
        repo.set_price_override("claude-sonnet-5", prices(1.0, 2.0, 0.0, 0.0))
            .await
            .unwrap();
        assert!((repo.summary(0).await.unwrap().cost_usd - 3.0).abs() < 1e-9);
        let by_model = repo.by_model(0, 10).await.unwrap();
        assert!(by_model[0].overridden && by_model[0].priced);
        // 展示名仍取官方定价表。
        assert_eq!(by_model[0].display_name.as_deref(), Some("Claude Sonnet 5"));

        let info = repo.price_info("claude-sonnet-5").await.unwrap();
        assert_eq!(info.custom, Some(prices(1.0, 2.0, 0.0, 0.0)));
        assert_eq!(info.official.unwrap().input, 3.0);

        repo.clear_price_override("claude-sonnet-5").await.unwrap();
        assert!((repo.summary(0).await.unwrap().cost_usd - official).abs() < 1e-9);
        assert!(!repo.by_model(0, 10).await.unwrap()[0].overridden);
        assert_eq!(
            repo.price_info("claude-sonnet-5").await.unwrap().custom,
            None
        );
    }

    #[tokio::test]
    async fn override_prices_a_model_the_table_does_not_know() {
        let repo = UsageRepo::new(open_in_memory().await.unwrap());
        repo.upsert_events(&[ev("session:o2", 100, "claude", "mystery-model-9000")])
            .await
            .unwrap();
        assert_eq!(repo.summary(0).await.unwrap().unpriced_events, 1);

        repo.set_price_override("mystery-model-9000", prices(1.0, 2.0, 0.5, 0.0))
            .await
            .unwrap();
        let summary = repo.summary(0).await.unwrap();
        assert_eq!(summary.unpriced_events, 0, "自定义价格也算有定价");
        assert!((summary.cost_usd - (1.0 + 2.0 + 0.4 * 0.5)).abs() < 1e-9);
        let row = &repo.by_model(0, 10).await.unwrap()[0];
        assert!(row.priced && row.overridden);
        assert_eq!(
            repo.price_info("mystery-model-9000")
                .await
                .unwrap()
                .official,
            None
        );

        repo.clear_price_override("mystery-model-9000")
            .await
            .unwrap();
        assert_eq!(repo.summary(0).await.unwrap().unpriced_events, 1);
        assert!(!repo.by_model(0, 10).await.unwrap()[0].priced);
    }

    /// 成本表达式只有一处：总览 / 趋势 / 按模型 / 按应用 / 按项目必须同时用上覆盖价，
    /// 否则会出现「总览是 $3、按模型是 $18」的自相矛盾。
    #[tokio::test]
    async fn every_aggregation_uses_the_override() {
        let repo = UsageRepo::new(open_in_memory().await.unwrap());
        repo.upsert_events(&[ev("session:o3", 100, "claude", "claude-sonnet-5")])
            .await
            .unwrap();
        repo.set_price_override("claude-sonnet-5", prices(1.0, 2.0, 0.0, 0.0))
            .await
            .unwrap();
        let expected = 3.0;
        assert!((repo.summary(0).await.unwrap().cost_usd - expected).abs() < 1e-9);
        assert!((repo.by_model(0, 10).await.unwrap()[0].cost_usd - expected).abs() < 1e-9);
        assert!((repo.by_app(0).await.unwrap()[0].cost_usd - expected).abs() < 1e-9);
        assert!((repo.by_project(0, 10).await.unwrap()[0].cost_usd - expected).abs() < 1e-9);
        let trend = repo.trend(0, 0).await.unwrap();
        assert!((trend.iter().map(|p| p.cost_usd).sum::<f64>() - expected).abs() < 1e-9);
    }

    #[tokio::test]
    async fn override_keeps_the_codex_cache_deduction() {
        let repo = UsageRepo::new(open_in_memory().await.unwrap());
        repo.upsert_events(&[ev("codex_session:t:1", 100, "codex", "gpt-5")])
            .await
            .unwrap();
        repo.set_price_override("gpt-5", prices(2.0, 4.0, 1.0, 0.0))
            .await
            .unwrap();
        // 计费输入 = 1M - 0.4M；output 1M；cache_read 0.4M。
        let expected = 0.6 * 2.0 + 4.0 + 0.4 * 1.0;
        assert!((repo.summary(0).await.unwrap().cost_usd - expected).abs() < 1e-9);
    }

    /// models.dev 同步会 INSERT OR REPLACE 官方价；覆盖放在独立表，不能被冲掉。
    #[tokio::test]
    async fn pricing_sync_does_not_touch_the_override() {
        let repo = UsageRepo::new(open_in_memory().await.unwrap());
        repo.upsert_events(&[ev("session:o4", 100, "claude", "claude-sonnet-5")])
            .await
            .unwrap();
        repo.set_price_override("claude-sonnet-5", prices(1.0, 2.0, 0.0, 0.0))
            .await
            .unwrap();
        repo.pricing_upsert_batch(&[PricingRow {
            model_id: "claude-sonnet-5".into(),
            display_name: "Claude Sonnet 5".into(),
            input_per_million: "30".into(),
            output_per_million: "150".into(),
            cache_read_per_million: "3".into(),
            cache_creation_per_million: "0".into(),
        }])
        .await
        .unwrap();
        assert!((repo.summary(0).await.unwrap().cost_usd - 3.0).abs() < 1e-9);
        // 官方价确实被同步改了，只是被覆盖挡在后面。
        let info = repo.price_info("claude-sonnet-5").await.unwrap();
        assert_eq!(info.official.unwrap().input, 30.0);
        assert_eq!(info.custom.unwrap().input, 1.0);
    }

    #[tokio::test]
    async fn override_rejects_bad_numbers_and_unknown_models() {
        let repo = UsageRepo::new(open_in_memory().await.unwrap());
        repo.upsert_events(&[ev("session:o5", 100, "claude", "claude-sonnet-5")])
            .await
            .unwrap();
        for bad in [
            prices(f64::NAN, 1.0, 0.0, 0.0),
            prices(1.0, f64::INFINITY, 0.0, 0.0),
            prices(-0.01, 1.0, 0.0, 0.0),
            prices(1.0, 1.0, MAX_PRICE_PER_MILLION + 1.0, 0.0),
            prices(1.0, 1.0, 0.0, -1.0),
        ] {
            let error = repo.set_price_override("claude-sonnet-5", bad).await;
            assert!(matches!(error, Err(StorageError::Invalid(_))), "{bad:?}");
        }
        // 上限本身合法；全部失败的尝试没有留下任何行。
        assert_eq!(
            repo.price_info("claude-sonnet-5").await.unwrap().custom,
            None
        );
        repo.set_price_override(
            "claude-sonnet-5",
            prices(MAX_PRICE_PER_MILLION, 0.0, 0.0, 0.0),
        )
        .await
        .unwrap();

        // 用量里没出现过的模型：不开放任意键。
        let unknown = repo
            .set_price_override("never-used-model", prices(1.0, 1.0, 0.0, 0.0))
            .await;
        assert!(matches!(unknown, Err(StorageError::NotFound(_))));
        // 清除不存在的覆盖不报错。
        repo.clear_price_override("never-used-model").await.unwrap();
    }

    #[tokio::test]
    async fn override_values_roundtrip_as_plain_decimals() {
        let repo = UsageRepo::new(open_in_memory().await.unwrap());
        repo.upsert_events(&[ev("session:o6", 100, "claude", "claude-sonnet-5")])
            .await
            .unwrap();
        repo.set_price_override("claude-sonnet-5", prices(0.6, 0.0000001, 1234.5, 0.0))
            .await
            .unwrap();
        let stored: String = sqlx::query_scalar(
            "SELECT input_per_million || '|' || output_per_million || '|' || cache_read_per_million
             FROM model_pricing_override WHERE model_id = 'claude-sonnet-5'",
        )
        .fetch_one(&repo.pool)
        .await
        .unwrap();
        assert_eq!(
            stored, "0.6|0.0000001|1234.5",
            "不带指数记法，与官方价同形态"
        );
    }

    #[tokio::test]
    async fn grouping_and_trend_and_prune() {
        let repo = UsageRepo::new(open_in_memory().await.unwrap());
        const DAY: i64 = 86_400;
        repo.upsert_events(&[
            ev("session:d1", DAY + 100, "claude", "claude-sonnet-5"),
            ev("session:d2", 2 * DAY + 100, "claude", "claude-sonnet-5"),
            ev(
                "codex_session:thread-v1:t:1",
                2 * DAY + 200,
                "codex",
                "gpt-5",
            ),
        ])
        .await
        .unwrap();

        // project 维度只含 Claude
        let by_project = repo.by_project(0, 10).await.unwrap();
        assert_eq!(by_project.len(), 1);
        assert_eq!(by_project[0].key, "-Users-me-proj");
        assert_eq!(by_project[0].events, 2);

        let by_app = repo.by_app(0).await.unwrap();
        assert_eq!(by_app.len(), 2);

        // UTC 分桶：两天各一桶
        let trend = repo.trend(0, 0).await.unwrap();
        assert_eq!(trend.len(), 2);
        assert_eq!(trend[0].bucket_ts, DAY);
        assert_eq!(trend[0].events, 1);
        assert_eq!(trend[1].bucket_ts, 2 * DAY);
        assert_eq!(trend[1].events, 2);

        // 东八区分桶：桶起点是本地日 00:00 对应的 UTC 时刻
        let trend_cn = repo.trend(0, 28_800).await.unwrap();
        assert_eq!(
            trend_cn[0].bucket_ts,
            ((DAY + 100 + 28_800) / DAY) * DAY - 28_800
        );

        // prune：删除第 1 天
        assert_eq!(repo.prune_before(2 * DAY).await.unwrap(), 1);
        assert_eq!(repo.summary(0).await.unwrap().events, 2);
    }
}
