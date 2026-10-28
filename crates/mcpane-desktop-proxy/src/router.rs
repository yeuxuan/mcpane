//! Claude Code / Codex 本地路由（ADR-022）：按「当前供应商 → 故障转移队列」
//! 依次尝试，熔断中的候选跳过；只在拿到响应头之前转移，流开始后不中途切换。
//!
//! 可重试判定：连接失败、首字节超时、
//! 以及除 400/405/406/413/414/415/422/501 之外的 4xx/5xx（换一家可能持有
//! 不同的 Key、额度或模型）；这些之外的错误换哪家都会被拒，原样透传。

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Method, Response, StatusCode};
use serde_json::Value;

use mcpane_provider::ClientApp;

use crate::ProxyState;
use crate::forward::{
    UpstreamAuth, UpstreamCall, read_body_limited, relay_response, send_upstream,
};
use crate::request_log::Attempt;
use crate::server::anthropic_error;
use crate::{claude_chat, codex_chat};

const NON_RETRYABLE_STATUS: [u16; 8] = [400, 405, 406, 413, 414, 415, 422, 501];

/// Claude Code 的模型档位（已按切换写入时的回落规则解析）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelSlots {
    pub main: Option<String>,
    pub haiku: Option<String>,
    pub sonnet: Option<String>,
    pub opus: Option<String>,
    pub fable: Option<String>,
}

impl ModelSlots {
    /// 回落规则与 Claude 切换写 env 时一致：haiku/sonnet/opus → 主模型；
    /// fable → opus → 主模型。
    pub fn resolve(
        main: Option<String>,
        haiku: Option<String>,
        sonnet: Option<String>,
        opus: Option<String>,
        fable: Option<String>,
    ) -> Self {
        Self {
            haiku: haiku.or_else(|| main.clone()),
            sonnet: sonnet.or_else(|| main.clone()),
            fable: fable.or_else(|| opus.clone()).or_else(|| main.clone()),
            opus: opus.or_else(|| main.clone()),
            main,
        }
    }

    fn slot(&self, role: Role) -> Option<&String> {
        match role {
            Role::Main => self.main.as_ref(),
            Role::Haiku => self.haiku.as_ref(),
            Role::Sonnet => self.sonnet.as_ref(),
            Role::Opus => self.opus.as_ref(),
            Role::Fable => self.fable.as_ref(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Main,
    Haiku,
    Sonnet,
    Opus,
    Fable,
}

/// 一个候选上游（Key 由托管层从凭据库物化，只存内存）。
#[derive(Debug, Clone)]
pub struct RouteUpstream {
    pub provider_id: String,
    pub name: String,
    /// Claude：Anthropic 根地址（请求打 `{base}/v1/messages`）；
    /// Codex：Responses 根地址（请求打 `{base}/responses`）。
    pub base_url: String,
    pub api_key: String,
    pub auth: UpstreamAuth,
    pub models: ModelSlots,
    /// 上游协议；与客户端协议不同时由路由转换。
    pub format: UpstreamFormat,
    /// 该供应商额外要求的请求头（加在协议转换之后；鉴权等受保护头不可覆盖）。
    pub headers: Vec<(String, String)>,
    /// 请求体覆盖（JSON Merge Patch：null 删键），用于转换后补厂商特有参数。
    pub body_override: Option<Value>,
}

/// 上游协议（相对客户端协议而言）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UpstreamFormat {
    /// 与客户端同协议，透传。
    #[default]
    Native,
    OpenaiChat,
    OpenaiResponses,
    Anthropic,
}

/// 某 app 的路由快照：`candidates[0]` 是当前供应商，其后为故障转移队列。
#[derive(Debug, Clone, Default)]
pub struct AppRoute {
    pub candidates: Vec<Arc<RouteUpstream>>,
    pub tuning: RouteTuning,
    pub strategy: mcpane_provider::RoutingStrategy,
}

/// 按 app 可调的超时与熔断参数（其余如错误率窗口、非流式总时限保持内置默认）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteTuning {
    /// 流式请求等响应头的时限。
    pub first_byte: Duration,
    /// 流开始后多久没有任何字节即中止。
    pub idle: Duration,
    /// 连续失败多少次熔断。
    pub failure_threshold: u32,
    /// 熔断后暂停多久再半开探测。
    pub cooldown: Duration,
}

impl Default for RouteTuning {
    fn default() -> Self {
        Self {
            first_byte: Duration::from_secs(60),
            idle: Duration::from_secs(120),
            failure_threshold: 4,
            cooldown: Duration::from_secs(60),
        }
    }
}

/// 候选健康状态（IPC 展示面）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateStatus {
    pub provider_id: String,
    pub name: String,
    pub primary: bool,
    #[serde(flatten)]
    pub breaker: crate::breaker::BreakerSnapshot,
    /// 本次运行以来经它完成的请求与用量。
    pub totals: crate::request_log::ProviderTotals,
    /// 额度优先策略下该候选因额度用尽被后置时的预计恢复剩余秒数。
    pub quota_exhausted_secs: Option<u64>,
}

/// 临时接替状态。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StickyStatus {
    pub provider_id: String,
    pub remaining_secs: u64,
}

/// 某 app 的路由状态。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppRouteStatus {
    pub app: ClientApp,
    /// 最近一次成功服务请求的供应商。
    pub last_served_by: Option<String>,
    /// 主站失败后由备用临时接替时的接替者。
    pub sticky: Option<StickyStatus>,
    pub candidates: Vec<CandidateStatus>,
}

pub(crate) fn breaker_key(app: ClientApp, provider_id: &str) -> String {
    format!("{}:{provider_id}", app.as_str())
}

fn is_retryable_status(status: StatusCode) -> bool {
    (status.is_client_error() || status.is_server_error())
        && !NON_RETRYABLE_STATUS.contains(&status.as_u16())
}

/// 请求模型对应的档位：先对照当前供应商写进 env 的档位值，再按名称猜。
fn detect_role(model: &str, primary: &ModelSlots) -> Role {
    for role in [
        Role::Haiku,
        Role::Sonnet,
        Role::Opus,
        Role::Fable,
        Role::Main,
    ] {
        if primary.slot(role).is_some_and(|m| m == model) && role != Role::Main {
            return role;
        }
    }
    let lower = model.to_ascii_lowercase();
    if lower.contains("haiku") {
        Role::Haiku
    } else if lower.contains("sonnet") {
        Role::Sonnet
    } else if lower.contains("opus") {
        Role::Opus
    } else if lower.contains("fable") {
        Role::Fable
    } else {
        Role::Main
    }
}

/// 转移到非当前供应商时改写请求 model；解析不了或无映射则原样发送。
fn map_body_for(
    app: ClientApp,
    body: &[u8],
    primary: &RouteUpstream,
    candidate: &RouteUpstream,
) -> Option<Vec<u8>> {
    let mut json: Value = serde_json::from_slice(body).ok()?;
    let requested = json.get("model")?.as_str()?.to_owned();
    let target = match app {
        ClientApp::Claude => {
            let role = detect_role(&requested, &primary.models);
            candidate
                .models
                .slot(role)
                .or(candidate.models.main.as_ref())?
        }
        _ => candidate.models.main.as_ref()?,
    };
    json["model"] = Value::String(target.clone());
    serde_json::to_vec(&json).ok()
}

/// Gemini 的模型写在路径里（`…/models/{model}:generateContent`）：转移到备用时换成
/// 备用的主模型；无主模型或路径不含模型段则原样。
fn gemini_path_for(rest: &str, candidate: &RouteUpstream) -> Option<String> {
    let model = candidate.models.main.as_ref()?;
    let start = rest.find("models/")? + "models/".len();
    let end = start + rest[start..].find(':')?;
    Some(format!("{}{model}{}", &rest[..start], &rest[end..]))
}

/// 是否流式：Gemini 看端点（`:streamGenerateContent` / `alt=sse`），其余看请求体 `stream`。
fn is_streaming(app: ClientApp, rest: &str, query: Option<&str>, body: &[u8]) -> bool {
    if app == ClientApp::Gemini {
        return rest.contains(":streamGenerateContent")
            || query.is_some_and(|q| q.split('&').any(|kv| kv == "alt=sse"));
    }
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|json| json.get("stream").and_then(Value::as_bool))
        .unwrap_or(false)
}

/// 客户端能读懂的错误体：Claude 用 Anthropic 形态，Gemini 用 Google 形态，其余 OpenAI 形态。
pub(crate) fn app_error(app: ClientApp, status: StatusCode, message: &str) -> Response<Body> {
    if app == ClientApp::Claude {
        return anthropic_error(status, "api_error", message);
    }
    let body = if app == ClientApp::Gemini {
        serde_json::json!({
            "error": { "code": status.as_u16(), "message": message, "status": "UNAVAILABLE" }
        })
    } else {
        serde_json::json!({ "error": { "message": message, "type": "api_error" } })
    };
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap_or_default()
}

/// 客户端打进路由命名空间的一次请求（`rest` 为命名空间后的路径）。
pub(crate) struct RoutedRequest {
    pub method: Method,
    pub rest: String,
    pub query: Option<String>,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

pub(crate) async fn forward_routed(
    state: &ProxyState,
    app: ClientApp,
    route: Arc<AppRoute>,
    request: RoutedRequest,
) -> Response<Body> {
    let RoutedRequest {
        method,
        rest,
        query,
        headers,
        body,
    } = request;
    let Some(primary) = route.candidates.first().cloned() else {
        return app_error(app, StatusCode::SERVICE_UNAVAILABLE, "没有可用的供应商");
    };
    let started = Instant::now();
    let streaming = is_streaming(app, &rest, query.as_deref(), &body);
    let log = state.request_log();
    let record = log.begin(app, requested_model(app, &rest, &body), streaming);
    let mut pending = log.guard(record, started);
    let tap = TapContext {
        record,
        app,
        streaming,
        started,
    };
    let breakers = state.breakers();
    let order = state.candidate_order(app, &route);
    let reqwest_method =
        reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::POST);
    let total = order.len();
    let mut last_error = String::new();
    let mut attempts: Vec<Attempt> = Vec::new();
    let mut primary_failed = false;

    for (index, candidate) in order.iter().enumerate() {
        let is_last = index + 1 == total;
        let is_primary = candidate.provider_id == primary.provider_id;
        let key = breaker_key(app, &candidate.provider_id);
        // HalfOpen 只放一个探测；其余请求先走下一个候选（已是最后一个则照常尝试）。
        let _probe = match breakers.admit(&key) {
            crate::breaker::Admission::Busy if !is_last => continue,
            crate::breaker::Admission::Probe(guard) => Some(guard),
            _ => None,
        };
        let mut client_body = if is_primary {
            body.clone()
        } else {
            map_body_for(app, &body, &primary, candidate).unwrap_or_else(|| body.clone())
        };
        let body_rewritten = client_body != body;
        let attempt_rest = match app {
            ClientApp::Gemini if !is_primary => {
                gemini_path_for(&rest, candidate).unwrap_or_else(|| rest.clone())
            }
            _ => rest.clone(),
        };
        let mut fixes = Fixes::default();
        let mut namespace_map: Option<crate::namespace::NamespaceMap> = None;
        loop {
            let prepared = match prepare_attempt(
                app,
                &attempt_rest,
                query.as_deref(),
                candidate,
                &client_body,
                &headers,
                body_rewritten || fixes.any(),
            ) {
                Ok(mut prepared) => {
                    apply_overrides(candidate, &mut prepared.headers, &mut prepared.body);
                    prepared
                }
                Err(response) => {
                    log.update(record, |r| {
                        r.provider_id = Some(candidate.provider_id.clone());
                        r.provider_name = Some(candidate.name.clone());
                        r.status = Some(response.status().as_u16());
                    });
                    return with_tap(*response, state, &mut pending, tap.plain(), None);
                }
            };
            let sent = send_upstream(
                &prepared.call(candidate, &route.tuning),
                reqwest_method.clone(),
                &prepared.headers,
                prepared.body,
                streaming,
            )
            .await;
            let response = match sent {
                Ok(response) => response,
                Err(error) if !error.timed_out && !fixes.transport_retry => {
                    // 连接层错误先换条连接重试一次：多半是复用了对端刚关掉的
                    // keep-alive 连接，不该据此熔断一个健康的供应商。
                    fixes.transport_retry = true;
                    tracing::info!("本地路由：「{}」{error}，重试一次", candidate.name);
                    continue;
                }
                Err(error) => {
                    // 重试后仍连不上 / 首字节超时是强信号：立即熔断，后续请求不再为它白等。
                    let message = format!("「{}」{error}", candidate.name);
                    breakers.record_failure(&key, &message, true, &route.tuning);
                    tracing::warn!("本地路由：{message}");
                    attempts.push(attempt(candidate, "连接失败或超时"));
                    primary_failed |= is_primary;
                    last_error = message;
                    break;
                }
            };
            let status = response.status();
            log.update(record, |r| r.first_byte_ms = Some(elapsed_ms(started)));
            if status.is_success() || status.is_redirection() {
                let client_response = success_response(
                    app,
                    prepared.conversion,
                    response,
                    streaming,
                    namespace_map,
                    route.tuning.idle,
                )
                .await;
                // 响应头 2xx 不等于成功：流式要看首事件与流的结局（ADR-022 §10）。
                // 还有后备时先窥探首事件，限流 / 过载就转下一个；没有后备则原样交给客户端。
                let client_response = if streaming && !is_last {
                    match peek_first_event(client_response, FIRST_EVENT_WINDOW).await {
                        Peeked::Pass(response) => response,
                        Peeked::Failed(reason) => {
                            let message = format!("「{}」流首事件报错：{reason}", candidate.name);
                            breakers.record_failure(&key, &message, false, &route.tuning);
                            tracing::warn!("本地路由：{message}，转移到下一个供应商");
                            attempts.push(attempt(candidate, "流首事件报错"));
                            primary_failed |= is_primary;
                            last_error = message;
                            break;
                        }
                    }
                } else {
                    client_response
                };
                // 非流式在响应头处即知成败；流式由结局回调记（半开探测沿用头部记成功，
                // 以免探测名额随流结束前就悬空）。
                let probing = _probe.is_some();
                if !streaming || probing {
                    breakers.record_success(&key);
                }
                let outcome = streaming.then(|| {
                    outcome_hook(
                        state,
                        key.clone(),
                        candidate.name.clone(),
                        route.tuning,
                        !probing,
                    )
                });
                state.set_last_served(app, &candidate.provider_id);
                state.update_sticky(app, &candidate.provider_id, is_primary, primary_failed);
                log.update(record, |r| {
                    r.provider_id = Some(candidate.provider_id.clone());
                    r.provider_name = Some(candidate.name.clone());
                    r.status = Some(status.as_u16());
                    r.attempts = std::mem::take(&mut attempts);
                });
                return with_tap(client_response, state, &mut pending, tap, outcome);
            }
            let upstream_headers = response.headers().clone();
            let bytes = read_body_limited(response).await.unwrap_or_default();
            let text = String::from_utf8_lossy(&bytes).into_owned();
            if is_retryable_status(status) {
                let message = format!("「{}」返回 HTTP {}", candidate.name, status.as_u16());
                breakers.record_failure(&key, &message, false, &route.tuning);
                primary_failed |= is_primary;
                if !is_last {
                    tracing::warn!("本地路由：{message}，转移到下一个供应商");
                    attempts.push(attempt(candidate, &format!("HTTP {}", status.as_u16())));
                    last_error = message;
                    break;
                }
            } else if let Some(label) = try_rectify(
                app,
                &prepared.conversion,
                status.as_u16(),
                &text,
                &mut client_body,
                &mut fixes,
                &mut namespace_map,
            ) {
                // 请求本身可修：修正后对同一候选重试（每类修正至多一次）。
                tracing::info!("本地路由：「{}」{label}", candidate.name);
                attempts.push(attempt(candidate, label));
                continue;
            }
            log.update(record, |r| {
                r.provider_id = Some(candidate.provider_id.clone());
                r.provider_name = Some(candidate.name.clone());
                r.status = Some(status.as_u16());
                r.error = Some(crate::codex_chat::error_message_from_chat(&bytes));
                r.attempts = std::mem::take(&mut attempts);
            });
            let client_response =
                error_response(app, &prepared.conversion, status, &upstream_headers, bytes);
            return with_tap(client_response, state, &mut pending, tap.plain(), None);
        }
    }
    let message = format!("本地路由的所有供应商均不可用：{last_error}");
    log.update(record, |r| {
        r.status = Some(StatusCode::BAD_GATEWAY.as_u16());
        r.error = Some(message.clone());
        r.attempts = std::mem::take(&mut attempts);
    });
    with_tap(
        app_error(app, StatusCode::BAD_GATEWAY, &message),
        state,
        &mut pending,
        tap.plain(),
        None,
    )
}

fn attempt(candidate: &RouteUpstream, outcome: &str) -> Attempt {
    Attempt {
        provider_id: candidate.provider_id.clone(),
        provider_name: candidate.name.clone(),
        outcome: outcome.to_owned(),
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// 请求模型（日志展示用）：Gemini 在路径里，其余在请求体。
fn requested_model(app: ClientApp, rest: &str, body: &[u8]) -> Option<String> {
    if app == ClientApp::Gemini {
        let start = rest.find("models/")? + "models/".len();
        let end = start + rest[start..].find(':').unwrap_or(rest.len() - start);
        return Some(rest[start..end].to_owned());
    }
    serde_json::from_slice::<Value>(body)
        .ok()?
        .get("model")?
        .as_str()
        .map(str::to_owned)
}

/// 一次请求挂 Tap 所需的上下文（记录号、客户端、是否流式、起始时刻）。
#[derive(Clone, Copy)]
struct TapContext {
    record: u64,
    app: ClientApp,
    streaming: bool,
    started: Instant,
}

impl TapContext {
    /// 错误 / 兜底响应不是流。
    fn plain(self) -> Self {
        Self {
            streaming: false,
            ..self
        }
    }
}

fn with_tap(
    response: Response<Body>,
    state: &ProxyState,
    pending: &mut crate::request_log::PendingGuard,
    tap: TapContext,
    on_outcome: Option<crate::request_log::OutcomeHook>,
) -> Response<Body> {
    pending.disarm();
    let (parts, body) = response.into_parts();
    let body = crate::request_log::tap(
        body,
        state.request_log(),
        tap.record,
        tap.app,
        tap.streaming,
        tap.started,
        on_outcome,
    );
    Response::from_parts(parts, body)
}

/// 流结束后把成败记进熔断器：完成记成功，供应商的失败记软故障，客户端提前断开不记。
/// `record_success` 为 false 时（半开探测已在响应头处记过成功）完成不重复记。
fn outcome_hook(
    state: &ProxyState,
    key: String,
    name: String,
    tuning: RouteTuning,
    record_success: bool,
) -> crate::request_log::OutcomeHook {
    use crate::request_log::StreamOutcome;
    let breakers = state.breakers_shared();
    Box::new(move |outcome| match outcome {
        StreamOutcome::Completed => {
            if record_success {
                breakers.record_success(&key);
            }
        }
        StreamOutcome::Failed(reason) => {
            let message = format!("「{name}」流式响应出错：{reason}");
            tracing::warn!("本地路由：{message}");
            breakers.record_failure(&key, &message, false, &tuning);
        }
        StreamOutcome::Rejected => {}
    })
}

/// 首事件窥探窗口：响应头已到、首个 SSE 事件却迟迟不来时不再等，照常放行。
const FIRST_EVENT_WINDOW: Duration = Duration::from_secs(2);

enum Peeked {
    /// 首事件正常（或窗口内没有事件）：流原样接回。
    Pass(Response<Body>),
    /// 首事件就是换家可能成功的错误：该候选失败，转下一个。
    Failed(String),
}

/// 窥探客户端流的首个 SSE 事件。典型场景：中转站先回 200，随即在流里报限流 / 过载——
/// 此时尚未向客户端写出任何字节，转移对客户端无感。窥探到的字节原样接回流前。
/// 只对 SSE 响应窥探（别的流式格式没有「事件」可看，白等会平添延迟）。
async fn peek_first_event(response: Response<Body>, window: Duration) -> Peeked {
    use futures_util::StreamExt;
    let is_sse = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("text/event-stream"));
    if !is_sse {
        return Peeked::Pass(response);
    }
    let (parts, body) = response.into_parts();
    let mut stream = body.into_data_stream();
    let mut held: Vec<axum::body::Bytes> = Vec::new();
    let mut buffer = String::new();
    let mut remainder = Vec::new();
    let mut failure = None;
    let deadline = Instant::now() + window;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, stream.next()).await {
            Ok(Some(Ok(chunk))) => {
                crate::sse::append_utf8(&mut buffer, &mut remainder, &chunk);
                held.push(chunk);
                if let Some(data) = crate::sse::take_data(&mut buffer) {
                    if let Some(error) = serde_json::from_str::<Value>(&data)
                        .ok()
                        .and_then(|json| crate::stream_error::classify(&json))
                        .filter(|error| error.retryable)
                    {
                        failure = Some(error.message);
                    }
                    break;
                }
            }
            Ok(Some(Err(error))) => {
                failure = Some(format!("响应中断：{error}"));
                break;
            }
            Ok(None) | Err(_) => break,
        }
    }
    if let Some(reason) = failure {
        return Peeked::Failed(reason);
    }
    let rest = futures_util::stream::iter(held.into_iter().map(Ok::<_, axum::Error>)).chain(stream);
    Peeked::Pass(Response::from_parts(parts, Body::from_stream(rest)))
}

/// 已应用过的请求修正（每类至多一次，防止修正—再拒的死循环）。
#[derive(Default)]
struct Fixes {
    /// 连接层错误后的同候选重试（不改请求体，不计入 `any`）。
    transport_retry: bool,
    signature: bool,
    budget: bool,
    images: bool,
    namespace: bool,
    max_tokens: bool,
    effort: bool,
}

impl Fixes {
    fn any(&self) -> bool {
        self.signature
            || self.budget
            || self.images
            || self.namespace
            || self.max_tokens
            || self.effort
    }
}

/// 按上游拒收原因修正客户端请求体；返回修正说明（None = 不可修，原样回错误）。
fn try_rectify(
    app: ClientApp,
    conversion: &Conversion,
    status: u16,
    error_text: &str,
    client_body: &mut Vec<u8>,
    fixes: &mut Fixes,
    namespace_map: &mut Option<crate::namespace::NamespaceMap>,
) -> Option<&'static str> {
    let mut json: Value = serde_json::from_slice(client_body).ok()?;
    let native = matches!(conversion, Conversion::None);
    let label = if app == ClientApp::Claude
        && native
        && !fixes.signature
        && crate::rectify::is_signature_error(error_text)
        && crate::rectify::strip_thinking(&mut json)
    {
        fixes.signature = true;
        "剥离历史 thinking 签名后重试"
    } else if app == ClientApp::Claude
        && native
        && !fixes.budget
        && crate::rectify::is_budget_error(error_text)
        && crate::rectify::fix_budget(&mut json)
    {
        fixes.budget = true;
        "修正 thinking 预算后重试"
    } else if let Some(limit) = (!fixes.max_tokens
        && matches!(app, ClientApp::Claude | ClientApp::Codex))
    .then(|| {
        crate::rectify::max_tokens_limit(error_text, requested_max_tokens(app, conversion, &json))
    })
    .flatten()
    {
        fixes.max_tokens = true;
        crate::rectify::fix_max_tokens(&mut json, app == ClientApp::Codex, limit);
        "输出上限超出模型允许值，按上游给出的上限重试"
    } else if !fixes.effort
        && matches!(app, ClientApp::Claude | ClientApp::Codex)
        && crate::rectify::is_effort_error(error_text)
        && crate::rectify::fix_effort(&mut json, app == ClientApp::Codex)
    {
        fixes.effort = true;
        "模型不支持该思考强度，降档后重试"
    } else if !fixes.images
        && crate::media::is_unsupported_image_error(status, error_text)
        && crate::media::strip_images(&mut json) > 0
    {
        fixes.images = true;
        "模型不支持图片，改为文字占位后重试"
    } else if app == ClientApp::Codex
        && native
        && !fixes.namespace
        && crate::namespace::is_namespace_rejection(status, error_text)
    {
        fixes.namespace = true;
        *namespace_map = Some(crate::namespace::flatten_request(&mut json));
        "上游不支持 namespace 工具，拍平后重试"
    } else {
        return None;
    };
    *client_body = serde_json::to_vec(&json).ok()?;
    Some(label)
}

/// 供应商级覆盖不可改的头：鉴权、连接与编码由路由统一管理。
const PROTECTED_OVERRIDE_HEADERS: [&str; 13] = [
    "host",
    "content-length",
    "content-type",
    "content-encoding",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "upgrade",
    "accept-encoding",
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "x-goog-api-key",
];

/// 供应商级请求覆盖（作用于协议转换后的上游请求）：头按名覆盖，体按 JSON Merge Patch
/// 合并（null 删键；`stream` 由客户端决定，不可覆盖）。
fn apply_overrides(candidate: &RouteUpstream, headers: &mut HeaderMap, body: &mut Vec<u8>) {
    for (name, value) in &candidate.headers {
        let lower = name.trim().to_ascii_lowercase();
        if PROTECTED_OVERRIDE_HEADERS.contains(&lower.as_str()) {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            axum::http::HeaderName::from_bytes(lower.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
    }
    let Some(Value::Object(patch)) = &candidate.body_override else {
        return;
    };
    let Ok(mut json) = serde_json::from_slice::<Value>(body) else {
        return;
    };
    let Some(target) = json.as_object_mut() else {
        return;
    };
    for (key, value) in patch.iter().filter(|(key, _)| key.as_str() != "stream") {
        merge_patch(target.entry(key.clone()).or_insert(Value::Null), value);
        if value.is_null() {
            target.remove(key);
        }
    }
    if let Ok(bytes) = serde_json::to_vec(&json) {
        *body = bytes;
    }
}

/// RFC 7396 JSON Merge Patch。
fn merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(patch) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(serde_json::Map::new());
    }
    if let Some(target) = target.as_object_mut() {
        for (key, value) in patch {
            if value.is_null() {
                target.remove(key);
            } else {
                merge_patch(target.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
    }
}

/// 本次实际发给上游的输出上限：Codex 未显式给时 Anthropic 转换会补默认值。
fn requested_max_tokens(app: ClientApp, conversion: &Conversion, body: &Value) -> u64 {
    let field = if app == ClientApp::Codex {
        "max_output_tokens"
    } else {
        "max_tokens"
    };
    body.get(field)
        .and_then(Value::as_u64)
        .unwrap_or(match conversion {
            Conversion::CodexAnthropic(_) => crate::codex_anthropic::DEFAULT_MAX_TOKENS,
            _ => u64::MAX,
        })
}

/// 相对客户端协议需要的转换（决定上游请求形态与响应还原方式）。
enum Conversion {
    None,
    CodexChat(codex_chat::ToolContext),
    CodexAnthropic(codex_chat::ToolContext),
    ClaudeChat,
    ClaudeResponses,
}

/// 一次上游请求的全部要素。
struct PreparedAttempt {
    url: String,
    headers: HeaderMap,
    body: Vec<u8>,
    auth: UpstreamAuth,
    anthropic: bool,
    conversion: Conversion,
}

impl PreparedAttempt {
    fn call<'a>(&self, candidate: &'a RouteUpstream, tuning: &RouteTuning) -> UpstreamCall<'a> {
        UpstreamCall {
            url: self.url.clone(),
            api_key: &candidate.api_key,
            auth: self.auth,
            anthropic: self.anthropic,
            first_byte: tuning.first_byte,
        }
    }
}

/// 组装上游请求：按「客户端 × 上游协议 × 路径」选转换；Err 为直接回客户端的响应。
#[allow(clippy::too_many_arguments)]
fn prepare_attempt(
    app: ClientApp,
    rest: &str,
    query: Option<&str>,
    candidate: &RouteUpstream,
    client_body: &[u8],
    incoming: &HeaderMap,
    body_rewritten: bool,
) -> Result<PreparedAttempt, Box<Response<Body>>> {
    let mut headers = incoming.clone();
    let format = candidate.format;
    let convertible = matches!(
        (app, rest, format),
        (
            ClientApp::Codex,
            "responses",
            UpstreamFormat::OpenaiChat | UpstreamFormat::Anthropic
        ) | (
            ClientApp::Claude,
            "v1/messages",
            UpstreamFormat::OpenaiChat | UpstreamFormat::OpenaiResponses
        )
    );
    if app == ClientApp::Claude
        && rest == "v1/messages/count_tokens"
        && format != UpstreamFormat::Native
    {
        // 非 Anthropic 上游没有 count_tokens：本地估算，免得 Claude Code 报错。
        return Err(Box::new(
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/json")
                .body(Body::from(
                    claude_chat::estimate_tokens(client_body).to_string(),
                ))
                .unwrap_or_default(),
        ));
    }
    if body_rewritten || convertible {
        headers.remove(axum::http::header::CONTENT_ENCODING);
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
    }
    if !convertible {
        let path = match query.filter(|q| !q.is_empty()) {
            Some(q) => format!("{rest}?{q}"),
            None => rest.to_owned(),
        };
        return Ok(PreparedAttempt {
            url: format!("{}/{path}", candidate.base_url),
            headers,
            body: client_body.to_vec(),
            auth: candidate.auth,
            anthropic: app == ClientApp::Claude,
            conversion: Conversion::None,
        });
    }
    let Ok(json) = serde_json::from_slice::<Value>(client_body) else {
        return Err(Box::new(app_error(
            app,
            StatusCode::BAD_REQUEST,
            "请求体不是合法 JSON，无法做协议转换",
        )));
    };
    let base = &candidate.base_url;
    let (url, body, conversion, auth, anthropic) = match (app, format) {
        (ClientApp::Codex, UpstreamFormat::Anthropic) => {
            let (upstream, ctx) = crate::codex_anthropic::request_to_anthropic(&json);
            if !headers.contains_key("anthropic-version") {
                headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
            }
            crate::forward::strip_codex_fingerprint(&mut headers);
            // Codex 发 `Accept: text/event-stream`；Anthropic 流式由 body 决定，严格网关对它回 406。
            headers.insert(
                axum::http::header::ACCEPT,
                HeaderValue::from_static("application/json"),
            );
            (
                crate::forward::endpoint(base, "/v1/messages"),
                upstream,
                Conversion::CodexAnthropic(ctx),
                candidate.auth,
                false,
            )
        }
        (ClientApp::Codex, _) => {
            let (upstream, ctx) = codex_chat::request_to_chat(&json);
            (
                crate::forward::endpoint(base, "/chat/completions"),
                upstream,
                Conversion::CodexChat(ctx),
                UpstreamAuth::Bearer,
                false,
            )
        }
        (_, UpstreamFormat::OpenaiResponses) => {
            crate::forward::strip_anthropic_headers(&mut headers);
            (
                crate::forward::endpoint(base, "/responses"),
                crate::claude_responses::request_to_responses(&json),
                Conversion::ClaudeResponses,
                UpstreamAuth::Bearer,
                false,
            )
        }
        _ => {
            crate::forward::strip_anthropic_headers(&mut headers);
            (
                crate::forward::endpoint(base, "/chat/completions"),
                claude_chat::request_to_chat(&json),
                Conversion::ClaudeChat,
                UpstreamAuth::Bearer,
                false,
            )
        }
    };
    Ok(PreparedAttempt {
        url,
        headers,
        body: serde_json::to_vec(&body).unwrap_or_default(),
        auth,
        anthropic,
        conversion,
    })
}

/// 成功响应 → 客户端协议（流式逐事件转换；非流式整包转换）。
async fn success_response(
    app: ClientApp,
    conversion: Conversion,
    response: reqwest::Response,
    streaming: bool,
    namespace_map: Option<crate::namespace::NamespaceMap>,
    idle: Duration,
) -> Response<Body> {
    // 转换 / 还原要解析正文；上游无视 identity 仍压缩时给出明确原因，而非 JSON 解析失败。
    let rewrites = !matches!(conversion, Conversion::None) || namespace_map.is_some();
    if let Some(encoding) = response
        .headers()
        .get(axum::http::header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .filter(|v| rewrites && !v.eq_ignore_ascii_case("identity"))
    {
        return app_error(
            app,
            StatusCode::BAD_GATEWAY,
            &format!("上游无视 accept-encoding: identity 返回了 {encoding} 压缩内容，无法转换协议"),
        );
    }
    match conversion {
        Conversion::None => match namespace_map {
            Some(map) if streaming => sse_response(crate::namespace::restore_stream(
                crate::forward::idle_guarded(response, idle),
                map,
            )),
            Some(map) => json_response(
                app,
                read_json(response).await.map(|mut json| {
                    crate::namespace::restore_json(&mut json, &map);
                    json
                }),
            ),
            None => relay_response(response, streaming, idle)
                .await
                .unwrap_or_else(|message| app_error(app, StatusCode::BAD_GATEWAY, &message)),
        },
        Conversion::CodexChat(ctx) if streaming => sse_response(codex_chat::convert_stream(
            crate::forward::idle_guarded(response, idle),
            ctx,
        )),
        Conversion::CodexChat(ctx) => json_response(
            app,
            read_json(response)
                .await
                .and_then(|json| codex_chat::response_from_chat(&json, &ctx)),
        ),
        Conversion::CodexAnthropic(ctx) if streaming => {
            sse_response(crate::codex_anthropic::convert_stream(
                crate::forward::idle_guarded(response, idle),
                ctx,
            ))
        }
        Conversion::CodexAnthropic(ctx) => json_response(
            app,
            read_json(response)
                .await
                .and_then(|json| crate::codex_anthropic::response_from_anthropic(&json, &ctx)),
        ),
        Conversion::ClaudeChat => {
            crate::forward::claude_openai_response(
                UpstreamFormat::OpenaiChat,
                response,
                streaming,
                idle,
            )
            .await
        }
        Conversion::ClaudeResponses => {
            crate::forward::claude_openai_response(
                UpstreamFormat::OpenaiResponses,
                response,
                streaming,
                idle,
            )
            .await
        }
    }
}

async fn read_json(response: reqwest::Response) -> Result<Value, String> {
    let bytes = read_body_limited(response).await?;
    serde_json::from_slice(&bytes).map_err(|e| format!("上游响应不是合法 JSON：{e}"))
}

fn sse_response<S>(stream: S) -> Response<Body>
where
    S: futures_util::Stream<Item = Result<axum::body::Bytes, std::io::Error>> + Send + 'static,
{
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .body(Body::from_stream(stream))
        .unwrap_or_default()
}

fn json_response(app: ClientApp, converted: Result<Value, String>) -> Response<Body> {
    match converted {
        Ok(json) => Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Body::from(json.to_string()))
            .unwrap_or_default(),
        Err(message) => app_error(app, StatusCode::BAD_GATEWAY, &message),
    }
}

/// 上游错误 → 客户端：透传原样（状态码、头、体），转换路径规整成客户端协议的错误形态。
fn error_response(
    app: ClientApp,
    conversion: &Conversion,
    status: StatusCode,
    upstream_headers: &HeaderMap,
    bytes: Vec<u8>,
) -> Response<Body> {
    let message = codex_chat::error_message_from_chat(&bytes);
    // 对话装不下：改写成客户端认得的形态，让它自己压缩后重试（ADR-022 §10）。
    if let Some(response) = crate::overflow::normalize(app, status, &message, &bytes) {
        return response;
    }
    if !matches!(conversion, Conversion::None) {
        return app_error(app, status, &message);
    }
    let mut builder = Response::builder().status(status);
    for (name, value) in upstream_headers {
        let lower = name.as_str();
        if !matches!(
            lower,
            "content-length" | "transfer-encoding" | "connection" | "content-encoding"
        ) {
            builder = builder.header(name, value);
        }
    }
    builder.body(Body::from(bytes)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn upstream(id: &str, slots: ModelSlots) -> RouteUpstream {
        RouteUpstream {
            provider_id: id.into(),
            name: id.into(),
            base_url: String::new(),
            api_key: String::new(),
            auth: UpstreamAuth::Bearer,
            models: slots,
            format: UpstreamFormat::Native,
            headers: Vec::new(),
            body_override: None,
        }
    }

    #[test]
    fn overrides_patch_body_and_skip_protected_headers() {
        let mut candidate = upstream("a", slots("m", None));
        candidate.headers = vec![
            ("X-Route".into(), "blue".into()),
            ("Authorization".into(), "Bearer stolen".into()),
        ];
        candidate.body_override = Some(serde_json::json!({
            "enable_thinking": false,
            "temperature": null,
            "stream": false,
            "extra": { "a": 1 }
        }));
        let mut headers = HeaderMap::new();
        let mut body = serde_json::to_vec(&serde_json::json!({
            "model": "m", "stream": true, "temperature": 0.7, "extra": { "b": 2 }
        }))
        .unwrap();
        apply_overrides(&candidate, &mut headers, &mut body);
        assert_eq!(headers["x-route"], "blue");
        assert!(!headers.contains_key("authorization"), "鉴权头不可被覆盖");
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["enable_thinking"], false);
        assert!(json.get("temperature").is_none(), "null 删键");
        assert_eq!(json["stream"], true, "stream 由客户端决定");
        assert_eq!(json["extra"], serde_json::json!({ "a": 1, "b": 2 }));

        // 非 JSON 体（如 GET）原样保留。
        let mut empty = Vec::new();
        apply_overrides(&candidate, &mut headers, &mut empty);
        assert!(empty.is_empty());
    }

    fn slots(main: &str, haiku: Option<&str>) -> ModelSlots {
        ModelSlots::resolve(Some(main.into()), haiku.map(Into::into), None, None, None)
    }

    #[test]
    fn claude_role_maps_across_providers() {
        let primary = upstream("a", slots("glm-5", Some("glm-4.7-air")));
        let backup = upstream("b", slots("kimi-k3", Some("kimi-k3-turbo")));
        let body = br#"{"model":"glm-4.7-air","stream":true}"#;
        let mapped: Value = serde_json::from_slice(
            &map_body_for(ClientApp::Claude, body, &primary, &backup).unwrap(),
        )
        .unwrap();
        assert_eq!(mapped["model"], "kimi-k3-turbo");
        assert_eq!(mapped["stream"], true);

        let body = br#"{"model":"glm-5"}"#;
        let mapped: Value = serde_json::from_slice(
            &map_body_for(ClientApp::Claude, body, &primary, &backup).unwrap(),
        )
        .unwrap();
        assert_eq!(mapped["model"], "kimi-k3");

        // 官方默认名按子串识别档位。
        let body = br#"{"model":"claude-haiku-4-5"}"#;
        let mapped: Value = serde_json::from_slice(
            &map_body_for(ClientApp::Claude, body, &primary, &backup).unwrap(),
        )
        .unwrap();
        assert_eq!(mapped["model"], "kimi-k3-turbo");
    }

    #[test]
    fn codex_maps_to_candidate_main_model_or_passes_through() {
        let primary = upstream("a", slots("gpt-5.5", None));
        let backup = upstream("b", slots("deepseek-v4", None));
        let body = br#"{"model":"gpt-5.5"}"#;
        let mapped: Value = serde_json::from_slice(
            &map_body_for(ClientApp::Codex, body, &primary, &backup).unwrap(),
        )
        .unwrap();
        assert_eq!(mapped["model"], "deepseek-v4");
        let bare = upstream("c", ModelSlots::default());
        assert!(map_body_for(ClientApp::Codex, body, &primary, &bare).is_none());
    }

    #[test]
    fn gemini_model_in_path_is_rewritten_for_backup() {
        let backup = upstream("b", slots("gemini-3-pro", None));
        assert_eq!(
            gemini_path_for(
                "v1beta/models/gemini-2.5-flash:streamGenerateContent",
                &backup
            )
            .as_deref(),
            Some("v1beta/models/gemini-3-pro:streamGenerateContent")
        );
        assert!(gemini_path_for("v1beta/models", &backup).is_none());
        assert!(is_streaming(
            ClientApp::Gemini,
            "v1beta/models/x:streamGenerateContent",
            Some("alt=sse"),
            b""
        ));
        assert!(!is_streaming(
            ClientApp::Gemini,
            "v1beta/models/x:generateContent",
            None,
            b"{}"
        ));
    }

    #[test]
    fn retryable_status_classification() {
        assert!(is_retryable_status(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable_status(StatusCode::UNAUTHORIZED));
        assert!(is_retryable_status(StatusCode::NOT_FOUND));
        assert!(!is_retryable_status(StatusCode::BAD_REQUEST));
        assert!(!is_retryable_status(StatusCode::PAYLOAD_TOO_LARGE));
        assert!(!is_retryable_status(StatusCode::OK));
    }
}
