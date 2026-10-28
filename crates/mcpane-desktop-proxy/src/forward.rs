//! 上游转发：请求头重建 → 上游转发 → SSE/JSON 透传（Desktop 直通与路由共用）。
//!
//! 头重建规则：
//! host 由客户端库按上游重设；hop-by-hop 头剥离；鉴权头整体替换为上游凭据；
//! 一律 `accept-encoding: identity`（reqwest 未开解压，转换与用量旁路都要读明文）；
//! `anthropic-beta` 确保含 claude-code 标（部分 Coding Plan 端点按此放行）；
//! 其余头透传。响应侧不改写 model 名（对上游透明）。

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Response, StatusCode};
use serde_json::Value;
use tokio_stream_idle::IdleTimeoutStream;

use mcpane_provider::claude_desktop;

use crate::DesktopUpstream;
use crate::router::UpstreamFormat;
use crate::server::anthropic_error;

/// 首字节 / 空闲时限按 app 可调（`RouteTuning`，默认对齐 cc-switch）。首字节时限只管
/// 流式：非流式要等整个回答生成完才有响应头，推理模型写长回答超过 60s 很正常，由 600s
/// 整体时限兜底。
const NON_STREAMING_TIMEOUT: Duration = Duration::from_secs(600);
/// 只有保活心跳、没有内容的时限：足够覆盖推理模型的长时间思考（不短于空闲时限）。
const STREAM_STALL_TIMEOUT: Duration = Duration::from_secs(300);

/// 非流式响应聚合上限（与请求侧 `MAX_BODY_BYTES` 同档）：上游透传
/// accept-encoding，reqwest 自动解压时该预算按**解压后**字节计，
/// 逐块累加、超限即中止，压缩炸弹不会先展开再比较（对齐 cc-switch 硬化）。
const MAX_RESPONSE_BODY_BYTES: usize = 128 * 1024 * 1024;

/// 部分 Coding Plan 端点按 claude-code beta 标识放行（cc-switch 恒重建注入）。
const CLAUDE_CODE_BETA: &str = "claude-code-20250219";

/// 请求侧不透传的头（小写）：hop-by-hop（RFC 9110 §7.6.1）+ 由本代理重设的头。
const REQUEST_DROP_HEADERS: [&str; 14] = [
    "host",
    "content-length",
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "expect",
    "authorization",
    "x-api-key",
    "x-goog-api-key",
];

/// 响应侧不透传的头（小写）：hop-by-hop + 由 axum 按实际 body 重设的头。
const RESPONSE_DROP_HEADERS: [&str; 7] = [
    "content-length",
    "connection",
    "keep-alive",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// 上游凭据的注入方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamAuth {
    /// `Authorization: Bearer`（OpenAI 兼容、多数 Anthropic 中转站）。
    Bearer,
    /// `x-api-key`（Anthropic 官方形态）。
    XApiKey,
    /// `x-goog-api-key`（Gemini 原生 API）。
    XGoogApiKey,
}

/// 一次上游调用的目标（Desktop 直通与路由命名空间共用）。
pub(crate) struct UpstreamCall<'a> {
    pub url: String,
    pub api_key: &'a str,
    pub auth: UpstreamAuth,
    /// Anthropic 端点：确保 `anthropic-beta` 含 claude-code 标。
    pub anthropic: bool,
    /// 流式请求等响应头的时限。
    pub first_byte: Duration,
}

/// 发送失败（未拿到响应头）：区分已等满首字节时限的超时与连接层错误——
/// 后者常是复用了对端刚关闭的 keep-alive 连接，值得立即换条连接重试一次。
#[derive(Debug)]
pub(crate) struct SendError {
    pub timed_out: bool,
    message: String,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// 重建请求头并发送，流式按 `call.first_byte` 看门；返回上游响应（任意状态码）。
pub(crate) async fn send_upstream(
    call: &UpstreamCall<'_>,
    method: reqwest::Method,
    incoming_headers: &HeaderMap,
    body: Vec<u8>,
    streaming: bool,
) -> Result<reqwest::Response, SendError> {
    let mut request = mcpane_net::client_for(&call.url)
        .request(method, &call.url)
        .timeout(if streaming {
            // 流式总时长不设上限（长回复合法），首字节与空闲另行看门。
            Duration::from_secs(60 * 60)
        } else {
            NON_STREAMING_TIMEOUT
        });

    // 头重建：透传白名单外全部原头。
    let mut beta_seen = false;
    for (name, value) in incoming_headers {
        let lower = name.as_str().to_ascii_lowercase();
        if REQUEST_DROP_HEADERS.contains(&lower.as_str()) {
            continue;
        }
        if lower == "accept-encoding" {
            continue; // 下方强制 identity。
        }
        if lower == "anthropic-beta" && call.anthropic {
            beta_seen = true;
            let merged = merge_beta_header(value.to_str().unwrap_or_default());
            request = request.header("anthropic-beta", merged);
            continue;
        }
        request = request.header(name, value);
    }
    if call.anthropic && !beta_seen {
        request = request.header("anthropic-beta", CLAUDE_CODE_BETA);
    }
    // 一律要求不压缩：reqwest 未开解压特性，而协议转换、用量旁路都要读明文；
    // loopback 上多几 KB 可以忽略。
    request = request.header("accept-encoding", "identity");
    // 上游凭据注入（按供应商声明的认证形态）。
    request = match call.auth {
        UpstreamAuth::Bearer => request.header("authorization", format!("Bearer {}", call.api_key)),
        UpstreamAuth::XApiKey => request.header("x-api-key", call.api_key),
        UpstreamAuth::XGoogApiKey => request.header("x-goog-api-key", call.api_key),
    };
    if !body.is_empty() {
        request = request.body(body);
    }

    let first_byte = if streaming {
        call.first_byte
    } else {
        NON_STREAMING_TIMEOUT
    };
    match tokio::time::timeout(first_byte, request.send()).await {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(error)) => Err(SendError {
            timed_out: false,
            message: format!("转发上游失败：{}", error_chain(&error)),
        }),
        Err(_) => Err(SendError {
            timed_out: true,
            message: format!("上游响应超时（{}s 未返回响应头）", first_byte.as_secs()),
        }),
    }
}

/// reqwest 的 Display 只有最外层一句（"error sending request"），根因在 source 链里。
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut parts = vec![error.to_string()];
    let mut source = error.source();
    while let Some(inner) = source {
        let text = inner.to_string();
        if !parts.iter().any(|p| p.contains(&text)) {
            parts.push(text);
        }
        source = inner.source();
    }
    parts.join("：")
}

/// 上游响应 → 客户端响应：状态码与头透传，SSE 逐字节直通，非流式限额聚合。
pub(crate) async fn relay_response(
    response: reqwest::Response,
    streaming: bool,
    idle: Duration,
) -> Result<Response<Body>, String> {
    let status = response.status();
    let mut builder = Response::builder().status(status);
    for (name, value) in response.headers() {
        if RESPONSE_DROP_HEADERS.contains(&name.as_str().to_ascii_lowercase().as_str()) {
            continue;
        }
        builder = builder.header(name, value);
    }
    let body = if streaming {
        // 块间空闲超过看门时长即断流（对齐 idle timeout 语义）。
        Body::from_stream(IdleTimeoutStream::new(
            response.bytes_stream(),
            idle,
            STREAM_STALL_TIMEOUT.max(idle),
        ))
    } else {
        Body::from(read_body_capped(response, MAX_RESPONSE_BODY_BYTES).await?)
    };
    builder.body(body).map_err(|_| "响应构造失败".to_owned())
}

pub(crate) async fn forward_messages(
    upstream: Arc<DesktopUpstream>,
    incoming_headers: HeaderMap,
    query: Option<String>,
    body: Value,
) -> Response<Body> {
    // 模型映射（含 [1m] 剥离、角色回落、MiMo thinking 归一化）。
    let body = match claude_desktop::map_request_body(
        &upstream.routes_resolved,
        &upstream.routes_raw,
        upstream.endpoint_is_mimo,
        body,
    ) {
        Ok(body) => body,
        Err(error) => {
            return anthropic_error(StatusCode::BAD_REQUEST, "invalid_request_error", &error);
        }
    };
    let streaming = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    // 客户端 query 原样透传（如 ?beta=true——cc-switch anthropic 直通路径
    // 把 endpoint 的 path+query 整体拼到 base_url 后，此处等价）。
    let format = upstream.format;
    let native = format == UpstreamFormat::Native;
    let url = match (format, query.as_deref().filter(|q| !q.is_empty())) {
        (UpstreamFormat::OpenaiChat, _) => endpoint(&upstream.base_url, "/chat/completions"),
        (UpstreamFormat::OpenaiResponses, _) => endpoint(&upstream.base_url, "/responses"),
        (_, Some(q)) => format!("{}?{q}", endpoint(&upstream.base_url, "/v1/messages")),
        (_, None) => endpoint(&upstream.base_url, "/v1/messages"),
    };
    let call = UpstreamCall {
        url,
        api_key: &upstream.api_key,
        auth: if upstream.use_x_api_key && native {
            UpstreamAuth::XApiKey
        } else {
            UpstreamAuth::Bearer
        },
        anthropic: native,
        first_byte: crate::router::RouteTuning::default().first_byte,
    };
    let mut headers = incoming_headers;
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    if !native {
        strip_anthropic_headers(&mut headers);
    }
    let upstream_body = match format {
        UpstreamFormat::OpenaiChat => crate::claude_chat::request_to_chat(&body),
        UpstreamFormat::OpenaiResponses => crate::claude_responses::request_to_responses(&body),
        _ => body,
    };
    let bytes = serde_json::to_vec(&upstream_body).unwrap_or_default();
    let response = match send_upstream(
        &call,
        reqwest::Method::POST,
        &headers,
        bytes.clone(),
        streaming,
    )
    .await
    {
        Ok(response) => response,
        // 连接层错误（多为对端刚关掉的 keep-alive 连接）换条连接重试一次。
        Err(error) if !error.timed_out => {
            match send_upstream(&call, reqwest::Method::POST, &headers, bytes, streaming).await {
                Ok(response) => response,
                Err(error) => {
                    tracing::warn!("Claude Desktop 网关{error}");
                    return anthropic_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "api_error",
                        &error.to_string(),
                    );
                }
            }
        }
        Err(error) => {
            tracing::warn!("Claude Desktop 网关{error}");
            return anthropic_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "api_error",
                &error.to_string(),
            );
        }
    };
    if !native {
        return claude_openai_response(
            format,
            response,
            streaming,
            crate::router::RouteTuning::default().idle,
        )
        .await;
    }
    // 错误响应同样透传，Desktop 展示上游原始错误。
    relay_response(
        response,
        streaming,
        crate::router::RouteTuning::default().idle,
    )
    .await
    .unwrap_or_else(|message| {
        anthropic_error(StatusCode::INTERNAL_SERVER_ERROR, "api_error", &message)
    })
}

/// Anthropic 专有头对 OpenAI 兼容端点无意义，严格网关还会拒收。
/// 拼上游端点：用户填的已是完整端点（`…/chat/completions`）或已带 `/v1` 时不重复拼接，
/// 否则会拼出 `…/v1/v1/messages` 这类恒 404/400 的地址（cc-switch 同样防御）。
pub(crate) fn endpoint(base: &str, path: &str) -> String {
    let lower = base.to_ascii_lowercase();
    if lower.ends_with(path) {
        return base.to_owned();
    }
    if let Some(rest) = path.strip_prefix("/v1")
        && lower.ends_with("/v1")
    {
        return format!("{base}{rest}");
    }
    format!("{base}{path}")
}

/// Codex 客户端指纹头（会话 / 账号 / SDK 标识）：转到另一家的 Anthropic 端点时剥掉，
/// 既防严格网关按陌生头拒收，也不把会话 ID 泄给无关供应商（列表对齐 cc-switch）。
pub(crate) fn strip_codex_fingerprint(headers: &mut HeaderMap) {
    let names: Vec<_> = headers
        .keys()
        .filter(|name| {
            let name = name.as_str();
            matches!(
                name,
                "originator"
                    | "session_id"
                    | "session-id"
                    | "thread-id"
                    | "conversation_id"
                    | "chatgpt-account-id"
                    | "x-openai-subagent"
                    | "x-client-request-id"
                    | "openai-beta"
                    | "openai-organization"
                    | "openai-project"
            ) || name.starts_with("x-stainless-")
                || name.starts_with("x-codex-")
        })
        .cloned()
        .collect();
    for name in names {
        headers.remove(name);
    }
}

pub(crate) fn strip_anthropic_headers(headers: &mut HeaderMap) {
    let names: Vec<_> = headers
        .keys()
        .filter(|name| name.as_str().starts_with("anthropic-"))
        .cloned()
        .collect();
    for name in names {
        headers.remove(name);
    }
}

/// OpenAI（Chat / Responses）上游响应 → Anthropic（Claude Code 路由与 Claude Desktop 网关共用）。
pub(crate) async fn claude_openai_response(
    format: UpstreamFormat,
    response: reqwest::Response,
    streaming: bool,
    idle: Duration,
) -> Response<Body> {
    let status = response.status();
    if !status.is_success() {
        let body = read_body_limited(response).await.unwrap_or_default();
        return anthropic_error(
            status,
            "api_error",
            &crate::codex_chat::error_message_from_chat(&body),
        );
    }
    let responses = format == UpstreamFormat::OpenaiResponses;
    if streaming {
        let upstream = idle_guarded(response, idle);
        let body = if responses {
            Body::from_stream(crate::claude_responses::convert_stream(upstream))
        } else {
            Body::from_stream(crate::claude_chat::convert_stream(upstream))
        };
        return Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .header("cache-control", "no-cache")
            .body(body)
            .unwrap_or_default();
    }
    let converted = read_body_limited(response).await.and_then(|bytes| {
        let json: Value =
            serde_json::from_slice(&bytes).map_err(|e| format!("上游响应不是合法 JSON：{e}"))?;
        if responses {
            crate::claude_responses::response_from_responses(&json)
        } else {
            crate::claude_chat::response_from_chat(&json)
        }
    });
    match converted {
        Ok(json) => Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Body::from(json.to_string()))
            .unwrap_or_default(),
        Err(message) => anthropic_error(StatusCode::BAD_GATEWAY, "api_error", &message),
    }
}

/// 带空闲看门的上游字节流（协议转换路径用）。
pub(crate) fn idle_guarded(
    response: reqwest::Response,
    idle: Duration,
) -> impl futures_core::Stream<Item = Result<axum::body::Bytes, std::io::Error>> + Send + Unpin {
    IdleTimeoutStream::new(
        response.bytes_stream(),
        idle,
        STREAM_STALL_TIMEOUT.max(idle),
    )
}

/// 非流式响应体（同 128 MiB 上限）。
pub(crate) async fn read_body_limited(response: reqwest::Response) -> Result<Vec<u8>, String> {
    read_body_capped(response, MAX_RESPONSE_BODY_BYTES).await
}

/// 逐块聚合响应体，累计超过 `cap` 立即中止（不整体缓冲后再检查）。
async fn read_body_capped(mut response: reqwest::Response, cap: usize) -> Result<Vec<u8>, String> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if buf.len().saturating_add(chunk.len()) > cap {
                    return Err(format!(
                        "上游响应超过 {} MiB 上限，已中止",
                        cap / 1024 / 1024
                    ));
                }
                buf.extend_from_slice(&chunk);
            }
            Ok(None) => return Ok(buf),
            Err(error) => return Err(format!("读取上游响应失败：{error}")),
        }
    }
}

/// 合并 anthropic-beta：保留客户端已声明的标，确保含 claude-code 标
///（缺失时**前置**，逐位对齐 cc-switch `format!("{CLAUDE_CODE_BETA},{beta_str}")`）。
fn merge_beta_header(existing: &str) -> String {
    if existing
        .split(',')
        .map(str::trim)
        .any(|part| part == CLAUDE_CODE_BETA)
    {
        return existing.to_owned();
    }
    format!("{CLAUDE_CODE_BETA},{existing}")
}

/// 块间空闲看门的最小实现（避免为一个 adapter 引入 tokio-stream 全家桶）。
mod tokio_stream_idle {
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use std::time::Duration;

    use futures_core::Stream;

    /// 两个看门：`idle` 按收到任何字节重置；`stall` 只按真实数据事件重置——网关在上游
    /// 卡住时会持续发 SSE 注释行 / ping 保活，只看字节的话流会永远挂着。
    pub struct IdleTimeoutStream<S> {
        inner: S,
        idle: Duration,
        stall: Duration,
        idle_sleep: Pin<Box<tokio::time::Sleep>>,
        stall_sleep: Pin<Box<tokio::time::Sleep>>,
    }

    impl<S> IdleTimeoutStream<S> {
        pub fn new(inner: S, idle: Duration, stall: Duration) -> Self {
            Self {
                inner,
                idle,
                stall,
                idle_sleep: Box::pin(tokio::time::sleep(idle)),
                stall_sleep: Box::pin(tokio::time::sleep(stall)),
            }
        }
    }

    /// 这块字节是否只有保活：SSE 注释行（`:` 开头）、`event: ping`、ping 数据。
    pub(super) fn is_keepalive_only(bytes: &[u8]) -> bool {
        let text = String::from_utf8_lossy(bytes);
        let mut saw_line = false;
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            saw_line = true;
            let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
            let keepalive = line.starts_with(':')
                || compact == "event:ping"
                || compact.starts_with("data:{\"type\":\"ping\"");
            if !keepalive {
                return false;
            }
        }
        saw_line
    }

    impl<S, T, E> Stream for IdleTimeoutStream<S>
    where
        S: Stream<Item = Result<T, E>> + Unpin,
        T: AsRef<[u8]>,
        E: std::fmt::Display,
    {
        type Item = Result<T, std::io::Error>;

        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let stalled = || {
                Poll::Ready(Some(Err(std::io::Error::other(
                    "上游只有保活心跳、长时间没有内容（可能已卡住），已中止",
                ))))
            };
            match Pin::new(&mut self.inner).poll_next(cx) {
                Poll::Ready(Some(Ok(item))) => {
                    let now = tokio::time::Instant::now();
                    let idle = self.idle;
                    self.idle_sleep.as_mut().reset(now + idle);
                    if is_keepalive_only(item.as_ref()) {
                        // 纯保活不算进展；无进展时限已到就中止。
                        if self.stall_sleep.as_mut().poll(cx).is_ready() {
                            return stalled();
                        }
                    } else {
                        let stall = self.stall;
                        self.stall_sleep.as_mut().reset(now + stall);
                    }
                    Poll::Ready(Some(Ok(item)))
                }
                Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(std::io::Error::other(
                    format!("上游流中断：{error}"),
                )))),
                Poll::Ready(None) => Poll::Ready(None),
                Poll::Pending => {
                    if self.idle_sleep.as_mut().poll(cx).is_ready() {
                        return Poll::Ready(Some(Err(std::io::Error::other(format!(
                            "上游流空闲超时（{}s 无数据）",
                            self.idle.as_secs()
                        )))));
                    }
                    if self.stall_sleep.as_mut().poll(cx).is_ready() {
                        return stalled();
                    }
                    Poll::Pending
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        #![allow(clippy::unwrap_used)]

        use super::*;
        use futures_util::StreamExt;

        #[test]
        fn keepalive_detection() {
            assert!(is_keepalive_only(b": PING\n\n"));
            assert!(is_keepalive_only(
                b"event: ping\ndata: {\"type\": \"ping\"}\n\n"
            ));
            assert!(!is_keepalive_only(
                b"data: {\"type\":\"content_block_delta\"}\n\n"
            ));
            assert!(!is_keepalive_only(b": PING\n\ndata: {\"x\":1}\n\n"));
            assert!(!is_keepalive_only(b""), "空块不算保活");
        }

        /// 只有心跳的流在无进展时限到期后中止，而不是被心跳一直续命。
        #[tokio::test(start_paused = true)]
        async fn heartbeat_only_stream_is_cut_after_stall_limit() {
            let pings = futures_util::stream::unfold(0u32, |n| async move {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Some((
                    Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b": PING\n\n")),
                    n + 1,
                ))
            });
            let mut guarded = IdleTimeoutStream::new(
                Box::pin(pings),
                Duration::from_secs(120),
                Duration::from_secs(60),
            );
            let mut delivered = 0;
            // 上限防看门失效时测试挂死：失效就在断言处报红。
            while delivered < 20 {
                match guarded.next().await.unwrap() {
                    Ok(_) => delivered += 1,
                    Err(error) => {
                        assert!(error.to_string().contains("保活心跳"));
                        break;
                    }
                }
            }
            assert!(
                (5..=7).contains(&delivered),
                "约 60s 后中止，收到 {delivered} 个心跳"
            );
        }
    }
}
