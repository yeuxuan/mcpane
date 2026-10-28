//! 路由请求日志（ADR-022 二期）：每个请求记一条（供应商、故障转移经过、耗时、用量），
//! 内存环形保留最近 500 条，并按供应商累计用量——补上会话文件缺失的「按供应商」维度。
//!
//! 用量从**客户端侧**响应里旁路解析（按客户端协议识别），不改动字节流；
//! 因此透传与协议转换两条路径口径一致。

use std::collections::{BTreeMap, VecDeque};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::body::{Body, Bytes};
use futures_util::Stream;
use serde_json::Value;

use mcpane_provider::ClientApp;

const CAPACITY: usize = 500;
/// 非流式响应最多缓冲这么多字节用于解析用量（超出只记耗时）。
const MAX_JSON_BYTES: usize = 16 * 1024 * 1024;

/// 标准化用量：`input_tokens` 含缓存命中（各协议口径不同，统一到「总输入」）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
}

/// 一次候选尝试的结果（故障转移经过）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Attempt {
    pub provider_id: String,
    pub provider_name: String,
    /// 如 "HTTP 503"、"连接失败"、"整流后重试"。
    pub outcome: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestRecord {
    pub id: u64,
    /// Unix 毫秒。
    pub at: u64,
    pub app: ClientApp,
    pub model: Option<String>,
    pub stream: bool,
    /// 最终服务（或最后尝试）的供应商。
    pub provider_id: Option<String>,
    pub provider_name: Option<String>,
    /// 最终尝试之前的失败经过（空 = 一次成功）。
    pub attempts: Vec<Attempt>,
    pub status: Option<u16>,
    /// 首字节（响应头）耗时。
    pub first_byte_ms: Option<u64>,
    /// 响应体读完的总耗时（流式为流结束时）。
    pub duration_ms: Option<u64>,
    pub usage: Option<Usage>,
    pub error: Option<String>,
}

/// 每个供应商的累计（本次运行以来）。
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTotals {
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
}

#[derive(Default)]
pub(crate) struct RequestLog {
    next_id: AtomicU64,
    records: Mutex<VecDeque<RequestRecord>>,
    totals: Mutex<BTreeMap<String, ProviderTotals>>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

impl RequestLog {
    pub fn begin(&self, app: ClientApp, model: Option<String>, stream: bool) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let record = RequestRecord {
            id,
            at: now_ms(),
            app,
            model,
            stream,
            provider_id: None,
            provider_name: None,
            attempts: Vec::new(),
            status: None,
            first_byte_ms: None,
            duration_ms: None,
            usage: None,
            error: None,
        };
        if let Ok(mut records) = self.records.lock() {
            if records.len() == CAPACITY {
                records.pop_front();
            }
            records.push_back(record);
        }
        id
    }

    pub fn update(&self, id: u64, f: impl FnOnce(&mut RequestRecord)) {
        if let Ok(mut records) = self.records.lock()
            && let Some(record) = records.iter_mut().rev().find(|r| r.id == id)
        {
            f(record);
        }
    }

    /// 请求结束：补齐耗时与用量，并计入所属供应商的累计。
    fn finish(&self, id: u64, duration_ms: u64, usage: Option<Usage>) {
        let mut key = None;
        self.update(id, |record| {
            record.duration_ms = Some(duration_ms);
            record.usage = usage;
            key = record
                .provider_id
                .as_ref()
                .map(|p| format!("{}:{p}", record.app.as_str()));
        });
        if let Some(key) = key
            && let Ok(mut totals) = self.totals.lock()
        {
            let entry = totals.entry(key).or_default();
            entry.requests += 1;
            if let Some(usage) = usage {
                entry.input_tokens += usage.input_tokens;
                entry.output_tokens += usage.output_tokens;
                entry.cache_read_tokens += usage.cache_read_tokens;
            }
        }
    }

    /// 请求开始即布防的守卫：拿到响应交给 [`tap`] 前处理函数被丢弃（客户端先断开），
    /// 由它把记录收尾，免得日志里永远挂着「进行中」。
    pub fn guard(self: &Arc<Self>, id: u64, started: Instant) -> PendingGuard {
        PendingGuard {
            log: self.clone(),
            id,
            started,
            armed: true,
        }
    }

    /// 最近的记录（新 → 旧），可按客户端过滤。
    pub fn recent(&self, app: Option<ClientApp>, limit: usize) -> Vec<RequestRecord> {
        self.records
            .lock()
            .map(|records| {
                records
                    .iter()
                    .rev()
                    .filter(|r| app.is_none_or(|a| r.app == a))
                    .take(limit)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 清空请求记录（`app` 为空清全部）；供应商累计用量保留。进行中的请求被清掉后，
    /// 其后续更新找不到记录会被忽略。
    pub fn clear(&self, app: Option<ClientApp>) {
        if let Ok(mut records) = self.records.lock() {
            records.retain(|r| app.is_some_and(|a| r.app != a));
        }
    }

    pub fn totals(&self, app: ClientApp, provider_id: &str) -> ProviderTotals {
        self.totals
            .lock()
            .ok()
            .and_then(|totals| {
                totals
                    .get(&format!("{}:{provider_id}", app.as_str()))
                    .copied()
            })
            .unwrap_or_default()
    }
}

/// 从一段客户端协议 JSON（整包响应或单个 SSE data）里取用量；多次出现时后者覆盖前者的非零字段。
fn merge_usage(app: ClientApp, json: &Value, acc: &mut Option<Usage>) {
    let n = |v: Option<&Value>| v.and_then(Value::as_u64).unwrap_or(0);
    let found = match app {
        ClientApp::Claude | ClientApp::ClaudeDesktop => {
            // 非流式 usage；流式 message_start.message.usage 与 message_delta.usage。
            let usage = json.get("usage").or_else(|| json.pointer("/message/usage"));
            usage.map(|u| {
                let cache_read = n(u.get("cache_read_input_tokens"));
                Usage {
                    input_tokens: n(u.get("input_tokens"))
                        + cache_read
                        + n(u.get("cache_creation_input_tokens")),
                    output_tokens: n(u.get("output_tokens")),
                    cache_read_tokens: cache_read,
                }
            })
        }
        ClientApp::Codex => json
            .get("usage")
            .or_else(|| json.pointer("/response/usage"))
            .filter(|u| u.is_object())
            .map(|u| Usage {
                input_tokens: n(u.get("input_tokens")),
                output_tokens: n(u.get("output_tokens")),
                cache_read_tokens: n(u.pointer("/input_tokens_details/cached_tokens")),
            }),
        ClientApp::Gemini => json.get("usageMetadata").map(|u| Usage {
            input_tokens: n(u.get("promptTokenCount")),
            output_tokens: n(u.get("candidatesTokenCount")) + n(u.get("thoughtsTokenCount")),
            cache_read_tokens: n(u.get("cachedContentTokenCount")),
        }),
        _ => json.get("usage").filter(|u| u.is_object()).map(|u| Usage {
            input_tokens: n(u.get("prompt_tokens")),
            output_tokens: n(u.get("completion_tokens")),
            cache_read_tokens: n(u.pointer("/prompt_tokens_details/cached_tokens")),
        }),
    };
    if let Some(found) = found {
        let current = acc.get_or_insert_with(Usage::default);
        // Anthropic 流式里 message_start 带输入、message_delta 带输出：按字段取非零值合并。
        if found.input_tokens > 0 {
            current.input_tokens = found.input_tokens;
            current.cache_read_tokens = found.cache_read_tokens;
        }
        if found.output_tokens > 0 {
            current.output_tokens = found.output_tokens;
        }
    }
}

pub(crate) struct PendingGuard {
    log: Arc<RequestLog>,
    id: u64,
    started: Instant,
    armed: bool,
}

impl PendingGuard {
    /// 响应已交给 tap 接管收尾。
    pub fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if self.armed {
            self.log.update(self.id, |r| {
                r.error
                    .get_or_insert_with(|| "客户端在收到响应前断开".into());
            });
            self.log.finish(self.id, elapsed_ms(self.started), None);
        }
    }
}

/// 给响应体套一层旁路：字节原样放行，同时解析用量，结束（或被客户端中断）时写回日志。
pub(crate) fn tap(
    body: Body,
    log: Arc<RequestLog>,
    id: u64,
    app: ClientApp,
    streaming: bool,
    started: Instant,
    on_outcome: Option<OutcomeHook>,
) -> Body {
    Body::from_stream(Tap {
        inner: Box::pin(body.into_data_stream()),
        log,
        id,
        app,
        streaming,
        started,
        buffer: String::new(),
        remainder: Vec::new(),
        json: Vec::new(),
        usage: None,
        finished: false,
        on_outcome,
        failure: None,
        rejected: false,
    })
}

/// 一条流的结局（客户端提前断开不产生结局）。
pub(crate) enum StreamOutcome {
    /// 流正常读完。
    Completed,
    /// 供应商的失败：读流出错、被空闲看门中止、或流里出现了换家可能成功的错误事件。
    Failed(String),
    /// 流里报了请求本身的错误（参数无效、对话装不下）：不算供应商的成败。
    Rejected,
}

pub(crate) type OutcomeHook = Box<dyn FnOnce(StreamOutcome) + Send + 'static>;

struct Tap {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, axum::Error>> + Send>>,
    log: Arc<RequestLog>,
    id: u64,
    app: ClientApp,
    streaming: bool,
    started: Instant,
    buffer: String,
    remainder: Vec<u8>,
    json: Vec<u8>,
    usage: Option<Usage>,
    finished: bool,
    on_outcome: Option<OutcomeHook>,
    failure: Option<String>,
    rejected: bool,
}

impl Tap {
    fn observe(&mut self, bytes: &[u8]) {
        if self.streaming {
            crate::sse::append_utf8(&mut self.buffer, &mut self.remainder, bytes);
            while let Some(data) = crate::sse::take_data(&mut self.buffer) {
                if let Ok(json) = serde_json::from_str::<Value>(&data) {
                    merge_usage(self.app, &json, &mut self.usage);
                    if let Some(error) = crate::stream_error::classify(&json) {
                        if error.retryable {
                            self.failure.get_or_insert(error.message);
                        } else {
                            self.rejected = true;
                        }
                    }
                }
            }
        } else if self.json.len() + bytes.len() <= MAX_JSON_BYTES {
            self.json.extend_from_slice(bytes);
        }
    }

    fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        if !self.streaming
            && let Ok(json) = serde_json::from_slice::<Value>(&self.json)
        {
            merge_usage(self.app, &json, &mut self.usage);
        }
        let failure = self.failure.clone();
        if let Some(hook) = self.on_outcome.take() {
            hook(match failure.clone() {
                Some(reason) => StreamOutcome::Failed(reason),
                None if self.rejected => StreamOutcome::Rejected,
                None => StreamOutcome::Completed,
            });
        }
        if let Some(reason) = failure {
            let reason = format!("流中报错：{reason}");
            self.log.update(self.id, |r| {
                r.error.get_or_insert(reason);
            });
        }
        self.log
            .finish(self.id, elapsed_ms(self.started), self.usage);
    }
}

impl Stream for Tap {
    type Item = Result<Bytes, axum::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let polled = self.inner.as_mut().poll_next(cx);
        match &polled {
            Poll::Ready(Some(Ok(bytes))) => {
                let bytes = bytes.clone();
                self.observe(&bytes);
            }
            Poll::Ready(Some(Err(error))) => {
                let message = format!("响应中断：{error}");
                let id = self.id;
                self.failure.get_or_insert_with(|| message.clone());
                self.log.update(id, |r| r.error = Some(message));
                self.finish();
            }
            Poll::Ready(None) => self.finish(),
            Poll::Pending => {}
        }
        polled
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        if !self.finished {
            // 客户端自己走掉（如按 Esc）不是供应商的错：丢弃回调，不记成败。
            self.on_outcome = None;
            self.failure = None;
            let id = self.id;
            self.log.update(id, |r| {
                r.error.get_or_insert_with(|| "客户端提前断开".into());
            });
            self.finish();
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use futures_util::StreamExt;
    use serde_json::json;

    async fn drain(body: Body) -> Vec<u8> {
        let mut out = Vec::new();
        let mut stream = body.into_data_stream();
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.unwrap());
        }
        out
    }

    #[tokio::test]
    async fn anthropic_stream_usage_merges_start_and_delta_without_touching_bytes() {
        let log = Arc::new(RequestLog::default());
        let id = log.begin(ClientApp::Claude, Some("m".into()), true);
        log.update(id, |r| r.provider_id = Some("p".into()));
        let raw = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":90,\"output_tokens\":1}}}\n\n\
                   event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":42}}\n\n";
        let chunks: Vec<Result<&'static str, std::io::Error>> =
            vec![Ok(&raw[..37]), Ok(&raw[37..])];
        let body = tap(
            Body::from_stream(futures_util::stream::iter(chunks)),
            log.clone(),
            id,
            ClientApp::Claude,
            true,
            Instant::now(),
            None,
        );
        assert_eq!(drain(body).await, raw.as_bytes());
        let record = &log.recent(None, 1)[0];
        assert_eq!(
            record.usage,
            Some(Usage {
                input_tokens: 100,
                output_tokens: 42,
                cache_read_tokens: 90
            })
        );
        assert!(record.duration_ms.is_some());
        let totals = log.totals(ClientApp::Claude, "p");
        assert_eq!((totals.requests, totals.output_tokens), (1, 42));
    }

    #[tokio::test]
    async fn non_stream_usage_for_each_protocol() {
        for (app, body, expected) in [
            (
                ClientApp::Codex,
                json!({"usage": {"input_tokens": 7, "output_tokens": 3, "input_tokens_details": {"cached_tokens": 2}}}),
                (7, 3, 2),
            ),
            (
                ClientApp::Grok,
                json!({"usage": {"prompt_tokens": 5, "completion_tokens": 4}}),
                (5, 4, 0),
            ),
            (
                ClientApp::Gemini,
                json!({"usageMetadata": {"promptTokenCount": 8, "candidatesTokenCount": 2, "thoughtsTokenCount": 1}}),
                (8, 3, 0),
            ),
        ] {
            let log = Arc::new(RequestLog::default());
            let id = log.begin(app, None, false);
            drain(tap(
                Body::from(body.to_string()),
                log.clone(),
                id,
                app,
                false,
                Instant::now(),
                None,
            ))
            .await;
            let usage = log.recent(None, 1)[0].usage.unwrap();
            assert_eq!(
                (
                    usage.input_tokens,
                    usage.output_tokens,
                    usage.cache_read_tokens
                ),
                expected,
                "{app:?}"
            );
        }
    }

    #[test]
    fn ring_buffer_keeps_latest() {
        let log = RequestLog::default();
        for _ in 0..CAPACITY + 5 {
            log.begin(ClientApp::Codex, None, false);
        }
        let recent = log.recent(None, CAPACITY + 10);
        assert_eq!(recent.len(), CAPACITY);
        assert_eq!(recent[0].id, (CAPACITY + 5) as u64);
    }

    #[test]
    fn abandoned_request_is_closed_by_guard_but_disarmed_one_is_not() {
        let log = Arc::new(RequestLog::default());
        let dropped = log.begin(ClientApp::Claude, None, true);
        drop(log.guard(dropped, Instant::now()));
        let record = &log.recent(None, 1)[0];
        assert_eq!(record.error.as_deref(), Some("客户端在收到响应前断开"));
        assert!(record.duration_ms.is_some(), "收尾后不再显示为进行中");

        let handed_over = log.begin(ClientApp::Claude, None, true);
        let mut guard = log.guard(handed_over, Instant::now());
        guard.disarm();
        drop(guard);
        let record = &log.recent(None, 1)[0];
        assert!(record.error.is_none());
        assert!(record.duration_ms.is_none(), "已交给 tap 收尾");
    }

    #[test]
    fn clear_drops_records_of_one_app_and_keeps_totals() {
        let log = RequestLog::default();
        let claude = log.begin(ClientApp::Claude, None, false);
        log.finish(claude, 1, None);
        let codex = log.begin(ClientApp::Codex, None, false);
        log.clear(Some(ClientApp::Claude));
        assert!(log.recent(Some(ClientApp::Claude), 10).is_empty());
        assert_eq!(log.recent(None, 10).len(), 1);
        log.clear(None);
        assert!(log.recent(None, 10).is_empty());
        log.finish(codex, 1, None); // 已清掉的记录：更新被忽略，不 panic。
    }
}
