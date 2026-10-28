//! Hub 请求日志（TASK-407）。
//!
//! 元数据 + 脱敏截断后的入参/出参摘要。Authorization / Token / 常见 Secret
//! 字段在落盘前替换为 `[REDACTED]`；超长字符串与整段 JSON 都会截断。
//! 列表查询不带摘要正文，详情按 id 再取，避免 5s 轮询拖上百万字节。

use sqlx::Row;

use crate::{RetentionStats, SqlitePool, StorageError, new_id};

/// 单段 JSON 摘要上限（UTF-8 字节）。列表不读这两列，详情才加载。
const SUMMARY_MAX_BYTES: usize = 16_384;
/// 单个字符串字段截断阈值，避免 resource 的 base64 把整段摘要撑满。
const STRING_MAX_CHARS: usize = 2_048;
/// 错误原文上限。
const ERROR_MAX_BYTES: usize = 2_048;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestLogEntry {
    pub id: String,
    pub ts: i64,
    pub request_id: String,
    pub client_connection_id: Option<String>,
    pub session_id: Option<String>,
    pub workspace_id: Option<String>,
    pub server_id: Option<String>,
    pub exposed_name: Option<String>,
    pub method: String,
    pub duration_ms: i64,
    pub success: bool,
    pub error_category: Option<String>,
    pub request_size: Option<i64>,
    pub response_size: Option<i64>,
    pub retry_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

impl RequestLogEntry {
    /// 把请求/响应 JSON 写成脱敏截断摘要，并记下截断前的字节数。
    pub fn attach_payloads(
        &mut self,
        request: Option<&serde_json::Value>,
        response: Option<&serde_json::Value>,
    ) {
        if let Some(value) = request {
            let (text, size) = summarize_json(value);
            self.request_summary = text;
            self.request_size = Some(size);
        }
        if let Some(value) = response {
            let (text, size) = summarize_json(value);
            self.response_summary = text;
            self.response_size = Some(size);
        }
    }

    pub fn attach_error(&mut self, message: &str) {
        let redacted = if let Ok(value) = serde_json::from_str::<serde_json::Value>(message) {
            summarize_json(&value)
                .0
                .unwrap_or_else(|| redact_bearer_in_text(message))
        } else {
            redact_bearer_in_text(message)
        };
        self.error_message = Some(truncate_utf8(&redacted, ERROR_MAX_BYTES));
    }
}

/// 时间窗聚合统计（Logs 页仪表）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogStats {
    pub total: i64,
    pub success: i64,
    pub avg_duration_ms: f64,
    pub p95_duration_ms: i64,
    /// 按下游 Server（server_id）的调用量 Top 8。
    pub by_server: Vec<LogGroupCount>,
    /// 按 MCP method（tools/call、prompts/get…）的调用量 Top 8。
    pub by_method: Vec<LogGroupCount>,
    /// 按暴露名的工具调用量 Top 8：哪些工具真被用起来了，哪些接了等于没接。
    pub by_tool: Vec<LogGroupCount>,
    /// 按客户端连接的调用量 Top 8：横向比各客户端的调用密度，
    /// 这是「谁在懒调用」目前唯一的客观读数。
    pub by_connection: Vec<LogGroupCount>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogGroupCount {
    pub key: String,
    pub count: i64,
    pub errors: i64,
}

pub struct LogRepo {
    pool: SqlitePool,
}

impl LogRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn append(&self, entry: &RequestLogEntry) -> Result<(), StorageError> {
        let id = if entry.id.is_empty() {
            new_id("log")
        } else {
            entry.id.clone()
        };
        sqlx::query(
            "INSERT INTO hub_request_logs
             (id, ts, request_id, client_connection_id, session_id, workspace_id, server_id,
              exposed_name, method, duration_ms, success, error_category,
              request_size, response_size, retry_count,
              request_summary, response_summary, error_message)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(entry.ts)
        .bind(&entry.request_id)
        .bind(&entry.client_connection_id)
        .bind(&entry.session_id)
        .bind(&entry.workspace_id)
        .bind(&entry.server_id)
        .bind(&entry.exposed_name)
        .bind(&entry.method)
        .bind(entry.duration_ms)
        .bind(entry.success as i64)
        .bind(&entry.error_category)
        .bind(entry.request_size)
        .bind(entry.response_size)
        .bind(entry.retry_count)
        .bind(&entry.request_summary)
        .bind(&entry.response_summary)
        .bind(&entry.error_message)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 最近 N 条元数据（不含入参/出参正文，给列表轮询用）。
    pub async fn recent(&self, limit: i64) -> Result<Vec<RequestLogEntry>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, ts, request_id, client_connection_id, session_id, workspace_id, server_id,
                    exposed_name, method, duration_ms, success, error_category,
                    request_size, response_size, retry_count, error_message
             FROM hub_request_logs ORDER BY ts DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| map_row(r, false)).collect())
    }

    /// 单条详情（含脱敏摘要）。列表点开后再取，避免把大 JSON 塞进轮询。
    pub async fn get(&self, id: &str) -> Result<RequestLogEntry, StorageError> {
        let row = sqlx::query("SELECT * FROM hub_request_logs WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| StorageError::NotFound(id.to_owned()))?;
        Ok(map_row(&row, true))
    }

    /// 时间窗内的聚合统计（Logs 页仪表用）。SQL 侧聚合而非拉全量到前端：
    /// 统计要对整个窗口负责，不能只算「最近 N 条」。
    pub async fn stats(&self, since_ts: i64) -> Result<LogStats, StorageError> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS total, COALESCE(SUM(success), 0) AS ok,
                    COALESCE(AVG(duration_ms), 0.0) AS avg_ms
             FROM hub_request_logs WHERE ts >= ?",
        )
        .bind(since_ts)
        .fetch_one(&self.pool)
        .await?;
        let total: i64 = row.get("total");
        let success: i64 = row.get("ok");
        let avg_duration_ms: f64 = row.get("avg_ms");

        // P95：SQLite 无内置分位数，按 nearest-rank（第 ceil(0.95N) 个，
        // 1 起）取行；窗口内行数有限，OFFSET 可接受。
        let p95_duration_ms = if total > 0 {
            let offset = (total * 95 + 99) / 100 - 1; // ceil(0.95N) - 1（0 起）
            sqlx::query(
                "SELECT duration_ms FROM hub_request_logs WHERE ts >= ?
                 ORDER BY duration_ms LIMIT 1 OFFSET ?",
            )
            .bind(since_ts)
            .bind(offset)
            .fetch_one(&self.pool)
            .await?
            .get("duration_ms")
        } else {
            0
        };

        // 分组列是编译期常量，仍写成两条静态 SQL——项目 lint 禁动态拼接，
        // 也避免给后来者留下「这里可以拼列名」的错误示范。
        const BY_SERVER_SQL: &str = "SELECT COALESCE(server_id, '-') AS key, COUNT(*) AS count,
                    COALESCE(SUM(1 - success), 0) AS errors
             FROM hub_request_logs WHERE ts >= ?
             GROUP BY key ORDER BY count DESC LIMIT 8";
        const BY_METHOD_SQL: &str = "SELECT COALESCE(method, '-') AS key, COUNT(*) AS count,
                    COALESCE(SUM(1 - success), 0) AS errors
             FROM hub_request_logs WHERE ts >= ?
             GROUP BY key ORDER BY count DESC LIMIT 8";
        // 按 method 过滤，不能只看 exposed_name 非空：prompts/get 与
        // resources/read 也写这一列（后者写的是 URI），只判非空会让「按工具」
        // 的读数混进 prompt 名和一串 mem:// URI。
        const BY_TOOL_SQL: &str = "SELECT exposed_name AS key, COUNT(*) AS count,
                    COALESCE(SUM(1 - success), 0) AS errors
             FROM hub_request_logs
             WHERE ts >= ? AND method = 'tools/call' AND exposed_name IS NOT NULL
             GROUP BY key ORDER BY count DESC LIMIT 8";
        const BY_CONNECTION_SQL: &str = "SELECT COALESCE(client_connection_id, '-') AS key,
                    COUNT(*) AS count, COALESCE(SUM(1 - success), 0) AS errors
             FROM hub_request_logs WHERE ts >= ?
             GROUP BY key ORDER BY count DESC LIMIT 8";
        let group = |sql: &'static str| {
            let pool = self.pool.clone();
            async move {
                let rows = sqlx::query(sql).bind(since_ts).fetch_all(&pool).await?;
                Ok::<_, StorageError>(
                    rows.iter()
                        .map(|r| LogGroupCount {
                            key: r.get("key"),
                            count: r.get("count"),
                            errors: r.get("errors"),
                        })
                        .collect::<Vec<_>>(),
                )
            }
        };
        Ok(LogStats {
            total,
            success,
            avg_duration_ms,
            p95_duration_ms,
            by_server: group(BY_SERVER_SQL).await?,
            by_method: group(BY_METHOD_SQL).await?,
            by_tool: group(BY_TOOL_SQL).await?,
            by_connection: group(BY_CONNECTION_SQL).await?,
        })
    }

    /// 保留窗口概览（设置页数据分区）：总条数 + 最早一条时间 + 早于 cutoff 的条数。
    /// 三个值一次查出——分开查会在写入频繁时给出互相矛盾的快照。
    pub async fn retention_stats(&self, cutoff: i64) -> Result<RetentionStats, StorageError> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS total, MIN(ts) AS oldest, \
             COALESCE(SUM(CASE WHEN ts < ? THEN 1 ELSE 0 END), 0) AS prunable \
             FROM hub_request_logs",
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

    /// 保留策略：删除早于 ts 的日志，返回删除条数。
    pub async fn prune_before(&self, ts: i64) -> Result<u64, StorageError> {
        let result = sqlx::query("DELETE FROM hub_request_logs WHERE ts < ?")
            .bind(ts)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

fn map_row(r: &sqlx::sqlite::SqliteRow, include_payloads: bool) -> RequestLogEntry {
    RequestLogEntry {
        id: r.get("id"),
        ts: r.get("ts"),
        request_id: r.get("request_id"),
        client_connection_id: r.get("client_connection_id"),
        session_id: r.get("session_id"),
        workspace_id: r.get("workspace_id"),
        server_id: r.get("server_id"),
        exposed_name: r.get("exposed_name"),
        method: r.get("method"),
        duration_ms: r.get("duration_ms"),
        success: r.get::<i64, _>("success") == 1,
        error_category: r.get("error_category"),
        request_size: r.get("request_size"),
        response_size: r.get("response_size"),
        retry_count: r.get("retry_count"),
        request_summary: if include_payloads {
            r.try_get("request_summary").ok().flatten()
        } else {
            None
        },
        response_summary: if include_payloads {
            r.try_get("response_summary").ok().flatten()
        } else {
            None
        },
        error_message: r.try_get("error_message").ok().flatten(),
    }
}

/// 脱敏 + 截断后的 JSON 文本，以及截断前的 UTF-8 字节数。
pub fn summarize_json(value: &serde_json::Value) -> (Option<String>, i64) {
    let mut redacted = value.clone();
    redact_value(&mut redacted);
    truncate_strings(&mut redacted, STRING_MAX_CHARS);
    let compact = serde_json::to_string(&redacted).unwrap_or_else(|_| "null".into());
    let original_bytes = compact.len() as i64;
    let text = truncate_utf8(&compact, SUMMARY_MAX_BYTES);
    (Some(text), original_bytes)
}

fn redact_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                if is_sensitive_key(&key) {
                    map.insert(key, serde_json::Value::String("[REDACTED]".into()));
                } else if let Some(child) = map.get_mut(&key) {
                    redact_value(child);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                redact_value(item);
            }
        }
        _ => {}
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let n: String = key
        .chars()
        .filter(|c| *c != '_' && *c != '-')
        .flat_map(char::to_lowercase)
        .collect();
    const EXACT: &[&str] = &[
        "password",
        "passwd",
        "secret",
        "authorization",
        "cookie",
        "bearer",
        "credential",
        "credentials",
        "apikey",
        "accesstoken",
        "refreshtoken",
        "privatekey",
        "clientsecret",
        "xapikey",
        "auth",
        "token",
    ];
    if EXACT.contains(&n.as_str()) {
        return true;
    }
    // token_count / max_tokens 是用量字段，不是密钥。
    const TOKEN_ALLOW: &[&str] = &[
        "tokencount",
        "maxtokens",
        "inputtokens",
        "outputtokens",
        "cachedtokens",
        "totaltokens",
        "prompttokens",
        "completiontokens",
    ];
    if n.ends_with("token") && !TOKEN_ALLOW.contains(&n.as_str()) {
        return true;
    }
    n.ends_with("secret") || n.ends_with("password") || n.ends_with("apikey")
}

fn truncate_strings(value: &mut serde_json::Value, max_chars: usize) {
    match value {
        serde_json::Value::String(s) if s.chars().count() > max_chars => {
            let kept: String = s.chars().take(max_chars).collect();
            let omitted = s.len().saturating_sub(kept.len());
            *s = format!("{kept}…(+{omitted} bytes)");
        }
        serde_json::Value::Array(items) => {
            for item in items {
                truncate_strings(item, max_chars);
            }
        }
        serde_json::Value::Object(map) => {
            for child in map.values_mut() {
                truncate_strings(child, max_chars);
            }
        }
        _ => {}
    }
}

fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = text[..end].to_owned();
    out.push_str("…(truncated)");
    out
}

/// 纯文本错误里的 `Bearer …` 令牌（JSON 走 `summarize_json` 的 key 脱敏）。
fn redact_bearer_in_text(input: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let bytes = input.as_bytes();
    let lower_bytes = lower.as_bytes();
    while i < bytes.len() {
        if lower_bytes.get(i..i + 7) == Some(b"bearer ") {
            out.push_str("Bearer [REDACTED]");
            i += 7;
            while i < bytes.len()
                && !bytes[i].is_ascii_whitespace()
                && bytes[i] != b'"'
                && bytes[i] != b'\''
            {
                i += 1;
            }
        } else {
            let Some(ch) = input[i..].chars().next() else {
                break;
            };
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::open_in_memory;

    fn entry(
        ts: i64,
        server: &str,
        method: &str,
        duration_ms: i64,
        success: bool,
    ) -> RequestLogEntry {
        RequestLogEntry {
            id: String::new(),
            ts,
            request_id: format!("req_{ts}_{duration_ms}"),
            client_connection_id: None,
            session_id: None,
            workspace_id: None,
            server_id: Some(server.to_owned()),
            exposed_name: None,
            method: method.to_owned(),
            duration_ms,
            success,
            error_category: (!success).then(|| "call".to_owned()),
            request_size: None,
            response_size: None,
            retry_count: 0,
            request_summary: None,
            response_summary: None,
            error_message: None,
        }
    }

    #[tokio::test]
    async fn stats_aggregates_window_groups_and_percentile() {
        let pool = open_in_memory().await.unwrap();
        let repo = LogRepo::new(pool);

        // 窗口外的旧日志不计入。
        repo.append(&entry(100, "srv_old", "tools/call", 999, true))
            .await
            .unwrap();
        // 窗口内：15×10ms + 4×20ms + 1×5000ms；nearest-rank P95 对 20 个
        // 样本取第 19 位（升序）= 20ms，极端慢尾只推高 avg 不撬动 P95。
        // srv_a 15 次（1 次失败）、srv_b 5 次；method 两种。
        for i in 0..15 {
            repo.append(&entry(1_000 + i, "srv_a", "tools/call", 10, i != 0))
                .await
                .unwrap();
        }
        for i in 0..4 {
            repo.append(&entry(2_000 + i, "srv_b", "prompts/get", 20, true))
                .await
                .unwrap();
        }
        repo.append(&entry(3_000, "srv_b", "prompts/get", 5_000, true))
            .await
            .unwrap();

        let stats = repo.stats(1_000).await.unwrap();
        assert_eq!(stats.total, 20);
        assert_eq!(stats.success, 19);
        assert_eq!(stats.p95_duration_ms, 20, "nearest-rank P95");
        assert!(stats.avg_duration_ms > 10.0 && stats.avg_duration_ms < 300.0);

        assert_eq!(stats.by_server[0].key, "srv_a");
        assert_eq!(stats.by_server[0].count, 15);
        assert_eq!(stats.by_server[0].errors, 1);
        assert_eq!(stats.by_server[1].key, "srv_b");
        assert_eq!(stats.by_method[0].key, "tools/call");

        // 空窗口：全零不报错。
        let empty = repo.stats(10_000).await.unwrap();
        assert_eq!(empty.total, 0);
        assert_eq!(empty.p95_duration_ms, 0);

        // N=10 界值：nearest-rank 取第 ceil(9.5)=10 个（最大值）——
        // floor((N-1)*0.95) 的实现在这里会取第 9 个，两种算法在 N=20 恰好
        // 重合，必须用 N=10 才锁得住语义。
        for i in 0..9 {
            repo.append(&entry(50_000 + i, "srv_c", "tools/call", 10, true))
                .await
                .unwrap();
        }
        repo.append(&entry(50_100, "srv_c", "tools/call", 700, true))
            .await
            .unwrap();
        let ten = repo.stats(50_000).await.unwrap();
        assert_eq!(ten.total, 10);
        assert_eq!(ten.p95_duration_ms, 700, "nearest-rank 必须捕获第 10 个");
    }

    /// 设置页的保留窗口靠这三个数画：总数、最早一条、超期条数。空表时 oldest 必须
    /// 是 None 而不是 0——0 会被前端当成 1970-01-01，把刻度线拉出五十多年。
    /// `by_tool` 必须只数工具调用。prompts/get 与 resources/read 也往
    /// `exposed_name` 里写东西（后者写的是 URI），只判非空会让「按工具」这张
    /// 读数混进 prompt 名和一串 mem:// URI——而这张表存在的意义正是回答
    /// 「接进来的工具到底有没有被用」。
    #[tokio::test]
    async fn by_tool_counts_only_tool_calls() {
        let pool = open_in_memory().await.unwrap();
        let repo = LogRepo::new(pool);
        let named = |ts: i64, method: &str, name: &str| RequestLogEntry {
            exposed_name: Some(name.to_owned()),
            ..entry(ts, "srv", method, 10, true)
        };

        repo.append(&named(1_000, "tools/call", "gh__search"))
            .await
            .unwrap();
        repo.append(&named(1_001, "prompts/get", "gh__review"))
            .await
            .unwrap();
        repo.append(&named(1_002, "resources/read", "mem://gh/readme"))
            .await
            .unwrap();

        let stats = repo.stats(0).await.unwrap();
        let keys: Vec<&str> = stats.by_tool.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(keys, vec!["gh__search"], "只有工具调用该进这张读数");
        // 连接维度不设这个限制——它数的是「这个客户端发了多少请求」。
        assert_eq!(stats.by_method.len(), 3, "method 维度仍应看到三类");
    }

    #[tokio::test]
    async fn retention_stats_counts_total_oldest_and_prunable() {
        let pool = open_in_memory().await.unwrap();
        let repo = LogRepo::new(pool);

        let empty = repo.retention_stats(1_000).await.unwrap();
        assert_eq!(empty.rows, 0);
        assert_eq!(empty.oldest_ts, None);
        assert_eq!(empty.prunable, 0);

        for ts in [100, 500, 900, 1_500, 2_000] {
            repo.append(&entry(ts, "srv", "tools/call", 10, true))
                .await
                .unwrap();
        }

        let stats = repo.retention_stats(1_000).await.unwrap();
        assert_eq!(stats.rows, 5);
        assert_eq!(stats.oldest_ts, Some(100));
        // cutoff 是开区间下界：ts < cutoff 才算超期，与 prune_before 同一条件，
        // 否则按钮上报的数字和真正删掉的数量会差一条。
        assert_eq!(stats.prunable, 3);
        assert_eq!(
            repo.prune_before(1_000).await.unwrap(),
            3,
            "报出的可清理数必须与实际删除数一致"
        );
    }

    #[test]
    fn summarize_redacts_secrets_and_keeps_usage_tokens() {
        let value = serde_json::json!({
            "query": "hello",
            "api_key": "sk-live-secret",
            "Authorization": "Bearer abc",
            "token_count": 42,
            "nested": { "access_token": "tok_xxx", "path": "/tmp" }
        });
        let (text, size) = summarize_json(&value);
        let text = text.unwrap();
        assert!(size > 0);
        assert!(!text.contains("sk-live-secret"));
        assert!(!text.contains("Bearer abc"));
        assert!(!text.contains("tok_xxx"));
        assert!(text.contains("[REDACTED]"));
        assert!(text.contains("hello"));
        assert!(text.contains("42"));
        assert!(text.contains("/tmp"));
    }

    #[test]
    fn summarize_truncates_long_strings_but_stays_json() {
        let long = "a".repeat(3_000);
        let value = serde_json::json!({ "blob": long });
        let (text, _) = summarize_json(&value);
        let text = text.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        let blob = parsed["blob"].as_str().unwrap();
        assert!(blob.contains("…(+"));
        assert!(blob.len() < 3_000);
    }

    #[tokio::test]
    async fn recent_omits_payload_get_returns_it() {
        let pool = open_in_memory().await.unwrap();
        let repo = LogRepo::new(pool);
        let mut row = entry(1_000, "srv_a", "tools/call", 12, true);
        row.id = "log_detail_1".into();
        row.attach_payloads(
            Some(&serde_json::json!({"q": "search", "token": "secret-value"})),
            Some(&serde_json::json!({"content": [{"text": "ok"}]})),
        );
        repo.append(&row).await.unwrap();

        let list = repo.recent(10).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "log_detail_1");
        assert_eq!(list[0].request_summary, None, "列表不带摘要正文");
        assert_eq!(list[0].response_summary, None);
        assert!(
            list[0].request_size.unwrap_or(0) > 0,
            "列表仍带字节数，方便扫一眼体量"
        );

        let detail = repo.get("log_detail_1").await.unwrap();
        let req = detail.request_summary.unwrap();
        assert!(req.contains("search"));
        assert!(!req.contains("secret-value"));
        assert!(req.contains("[REDACTED]"));
        assert!(detail.response_summary.unwrap().contains("ok"));
    }

    #[test]
    fn attach_error_redacts_json_secrets_and_bearer() {
        let mut json_err = entry(1, "srv", "tools/call", 1, false);
        json_err.attach_error(r#"{"api_key":"sk-live-secret","reason":"nope"}"#);
        let json_msg = json_err.error_message.unwrap();
        assert!(!json_msg.contains("sk-live-secret"));
        assert!(json_msg.contains("[REDACTED]"));
        assert!(json_msg.contains("nope"));

        let mut bearer_err = entry(1, "srv", "tools/call", 1, false);
        bearer_err.attach_error("upstream said Bearer super-secret-token");
        let bearer_msg = bearer_err.error_message.unwrap();
        assert!(!bearer_msg.contains("super-secret-token"));
        assert!(bearer_msg.contains("Bearer [REDACTED]"));
    }
}
