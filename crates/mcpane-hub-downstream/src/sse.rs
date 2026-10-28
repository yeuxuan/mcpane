//! HTTP+SSE 客户端传输（MCP 2024-11-05 传输，规范已标记 legacy）。
//!
//! rmcp 2.x 随规范演进移除了 SSE 客户端传输（Streamable HTTP 取而代之），但
//! 存量 SSE-only 的远程 MCP Server 仍然常见，聚合网关必须能接。本模块自
//! rmcp 0.8.5（MIT License，https://github.com/modelcontextprotocol/rust-sdk）
//! 移植 `transport/sse_client.rs`、`transport/common/reqwest/sse_client.rs`，
//! 以及 `client_side_sse.rs` 中未公开导出的重连状态机（`SseStreamReconnect` /
//! `SseAutoReconnectStream`——rmcp 2.2.0 中为 pub(crate)，公开部分如
//! `SseRetryPolicy`/`FixedInterval` 直接复用上游）。`Transport` trait 在
//! 0.8.5 与 2.2.0 间逐字节一致，移植零语义偏移。
//!
//! 改动：去掉 auth feature 分支与文档示例；`poll_next` 尾递归改显式循环
//! （连发控制帧防爆栈）；`message_endpoint` 锁中毒改恢复续用（见各处注释）；
//! 其余保持上游原样，便于日后对照。

// vendored 代码尽量与上游逐行可对照，错误改道再解析处的 expect/unwrap_err
// 用法原样保留，不为本仓 lint 改写语义。
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::{
    pin::Pin,
    sync::{Arc, RwLock},
    task::{Poll, ready},
    time::Duration,
};

use futures::{Stream, StreamExt, future::BoxFuture};
use http::Uri;
use rmcp::RoleClient;
use rmcp::model::{ClientJsonRpcMessage, ServerJsonRpcMessage};
use rmcp::transport::Transport;
use rmcp::transport::common::client_side_sse::{BoxedSseResponse, FixedInterval, SseRetryPolicy};
use sse_stream::{Error as SseError, Sse, SseStream};
use thiserror::Error;

const EVENT_STREAM_MIME_TYPE: &str = "text/event-stream";
const HEADER_LAST_EVENT_ID: &str = "Last-Event-Id";

#[derive(Error, Debug)]
pub enum SseTransportError<E: std::error::Error + Send + Sync + 'static> {
    #[error("SSE error: {0}")]
    Sse(#[from] SseError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Client error: {0}")]
    Client(E),
    #[error("unexpected end of stream")]
    UnexpectedEndOfStream,
    #[error("Unexpected content type: {0:?}")]
    UnexpectedContentType(Option<String>),
    #[error("Invalid uri: {0}")]
    InvalidUri(#[from] http::uri::InvalidUri),
    #[error("Invalid uri parts: {0}")]
    InvalidUriParts(#[from] http::uri::InvalidUriParts),
    #[error("cross-origin message endpoint rejected: {0}")]
    CrossOriginEndpoint(String),
}

/// 两个 URI 是否同源（scheme + authority 含 host:port）。
/// SSE server 经 `event: endpoint` 下发的 POST 目标必须与 SSE 端点同源，
/// 否则携带 Bearer 的消息会被劫持到任意/内网主机（SSRF + 凭据重定向）。
fn same_origin(a: &http::Uri, b: &http::Uri) -> bool {
    a.scheme() == b.scheme()
        && a.authority().map(|x| x.as_str()) == b.authority().map(|x| x.as_str())
}

pub trait SseClient: Clone + Send + Sync + 'static {
    type Error: std::error::Error + Send + Sync + 'static;
    fn post_message(
        &self,
        uri: Uri,
        message: ClientJsonRpcMessage,
        auth_token: Option<String>,
    ) -> impl Future<Output = Result<(), SseTransportError<Self::Error>>> + Send + '_;
    fn get_stream(
        &self,
        uri: Uri,
        last_event_id: Option<String>,
        auth_token: Option<String>,
    ) -> impl Future<Output = Result<BoxedSseResponse, SseTransportError<Self::Error>>> + Send + '_;
}

// ---------- 重连状态机（rmcp 2.2.0 中 pub(crate)，按原样移植） ----------

trait SseStreamReconnect {
    type Error: std::error::Error;
    type Future: Future<Output = Result<BoxedSseResponse, Self::Error>> + Send;
    fn retry_connection(&mut self, last_event_id: Option<&str>) -> Self::Future;
    fn handle_control_event(&mut self, _event: &Sse) -> Result<(), Self::Error> {
        Ok(())
    }
    fn handle_stream_error(
        &mut self,
        error: &(dyn std::error::Error + 'static),
        last_event_id: Option<&str>,
    ) {
        if let Some(id) = last_event_id {
            tracing::warn!(%id, "sse stream error: {error}");
        } else {
            tracing::warn!("sse stream error: {error}");
        }
    }
}

pin_project_lite::pin_project! {
    struct SseAutoReconnectStream<R>
    where R: SseStreamReconnect
     {
        retry_policy: Arc<dyn SseRetryPolicy>,
        last_event_id: Option<String>,
        server_retry_interval: Option<Duration>,
        connector: R,
        #[pin]
        state: SseAutoReconnectStreamState<R::Future>,
    }
}

impl<R: SseStreamReconnect> SseAutoReconnectStream<R> {
    fn new(stream: BoxedSseResponse, connector: R, retry_policy: Arc<dyn SseRetryPolicy>) -> Self {
        Self {
            retry_policy,
            last_event_id: None,
            server_retry_interval: None,
            connector,
            state: SseAutoReconnectStreamState::Connected { stream },
        }
    }
}

pin_project_lite::pin_project! {
    #[project = SseAutoReconnectStreamStateProj]
    enum SseAutoReconnectStreamState<F> {
        Connected {
            #[pin]
            stream: BoxedSseResponse,
        },
        Retrying {
            retry_times: usize,
            #[pin]
            retrying: F,
        },
        WaitingNextRetry {
            #[pin]
            sleep: tokio::time::Sleep,
            retry_times: usize,
        },
        Terminated,
    }
}

impl<R> Stream for SseAutoReconnectStream<R>
where
    R: SseStreamReconnect,
{
    type Item = Result<ServerJsonRpcMessage, R::Error>;
    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let mut this = self.as_mut().project();
        // 循环而非尾递归重入 poll_next：连续就绪的控制帧/畸形消息每条都会
        // 多压一层栈帧，缓冲区里攒够一批（恶意下游刷 ping 即可）就有爆栈风险。
        loop {
            let state = this.state.as_mut().project();
            let next_state = match state {
                SseAutoReconnectStreamStateProj::Connected { stream } => {
                    match ready!(stream.poll_next(cx)) {
                        Some(Ok(sse)) => {
                            if let Some(new_server_retry) = sse.retry {
                                *this.server_retry_interval =
                                    Some(Duration::from_millis(new_server_retry));
                            }
                            if let Some(ref event_id) = sse.id {
                                *this.last_event_id = Some(event_id.clone());
                            }
                            // 仅空/`message` 事件按 JSON-RPC 载荷处理；其余控制帧
                            // （endpoint、ping 等）交给重连处理器。
                            let is_message_event =
                                matches!(sse.event.as_deref(), None | Some("") | Some("message"));
                            if !is_message_event {
                                match this.connector.handle_control_event(&sse) {
                                    Ok(()) => continue,
                                    Err(e) => {
                                        this.state.set(SseAutoReconnectStreamState::Terminated);
                                        return Poll::Ready(Some(Err(e)));
                                    }
                                }
                            }
                            if let Some(data) = sse.data {
                                match serde_json::from_str::<ServerJsonRpcMessage>(&data) {
                                    Err(e) => {
                                        let last_id = this.last_event_id.as_deref().unwrap_or("");
                                        tracing::debug!(last_event_id=%last_id, "failed to deserialize server message: {e}");
                                        continue;
                                    }
                                    Ok(message) => {
                                        return Poll::Ready(Some(Ok(message)));
                                    }
                                }
                            } else {
                                continue;
                            }
                        }
                        Some(Err(e)) => {
                            this.connector
                                .handle_stream_error(&e, this.last_event_id.as_deref());
                            let retrying = this
                                .connector
                                .retry_connection(this.last_event_id.as_deref());
                            SseAutoReconnectStreamState::Retrying {
                                retry_times: 0,
                                retrying,
                            }
                        }
                        None => {
                            // 优雅关流可重连（SEP-1699）；服务端下发过 retry 间隔时
                            // 与 policy floor 取 max——否则恶意下游 `retry: 0` + 立即
                            // 关流 → sleep(0) 零延迟重连忙循环（对齐下方错误分支）。
                            let floor = this.retry_policy.retry(0);
                            let interval = match (this.server_retry_interval.take(), floor) {
                                (Some(server), Some(floor)) => Some(server.max(floor)),
                                (Some(server), None) => Some(server),
                                (None, floor) => floor,
                            };
                            if let Some(interval) = interval {
                                tracing::debug!(
                                    ?interval,
                                    "sse stream ended gracefully, reconnecting"
                                );
                                SseAutoReconnectStreamState::WaitingNextRetry {
                                    sleep: tokio::time::sleep(interval),
                                    retry_times: 0,
                                }
                            } else {
                                tracing::debug!("sse stream terminated, no reconnect policy");
                                return Poll::Ready(None);
                            }
                        }
                    }
                }
                SseAutoReconnectStreamStateProj::Retrying {
                    retry_times,
                    retrying,
                } => {
                    let retry_result = ready!(retrying.poll(cx));
                    match retry_result {
                        Ok(new_stream) => {
                            SseAutoReconnectStreamState::Connected { stream: new_stream }
                        }
                        Err(e) => {
                            tracing::debug!("retry sse stream error: {e}");
                            *retry_times += 1;
                            if let Some(interval) = this.retry_policy.retry(*retry_times) {
                                let interval = this
                                    .server_retry_interval
                                    .map(|server_retry_interval| {
                                        server_retry_interval.max(interval)
                                    })
                                    .unwrap_or(interval);
                                let sleep = tokio::time::sleep(interval);
                                SseAutoReconnectStreamState::WaitingNextRetry {
                                    sleep,
                                    retry_times: *retry_times,
                                }
                            } else {
                                tracing::error!("sse stream error: {e}, max retry times reached");
                                this.state.set(SseAutoReconnectStreamState::Terminated);
                                return Poll::Ready(Some(Err(e)));
                            }
                        }
                    }
                }
                SseAutoReconnectStreamStateProj::WaitingNextRetry { sleep, retry_times } => {
                    ready!(sleep.poll(cx));
                    let retrying = this
                        .connector
                        .retry_connection(this.last_event_id.as_deref());
                    let retry_times = *retry_times;
                    SseAutoReconnectStreamState::Retrying {
                        retry_times,
                        retrying,
                    }
                }
                SseAutoReconnectStreamStateProj::Terminated => {
                    return Poll::Ready(None);
                }
            };
            // update the state
            this.state.set(next_state);
        }
    }
}

// ---------- SSE 客户端传输本体 ----------

/// 重连时消费服务端控制帧（`event: endpoint` 常携带新 sessionId 的 POST 端点），
/// 与 [`SseAutoReconnectStream`] 配套使用。
struct SseClientReconnect<C> {
    pub client: C,
    pub uri: Uri,
    pub message_endpoint: Arc<RwLock<Uri>>,
}

impl<C: SseClient> SseStreamReconnect for SseClientReconnect<C> {
    type Error = SseTransportError<C::Error>;
    type Future = BoxFuture<'static, Result<BoxedSseResponse, Self::Error>>;
    fn retry_connection(&mut self, last_event_id: Option<&str>) -> Self::Future {
        let client = self.client.clone();
        let uri = self.uri.clone();
        let last_event_id = last_event_id.map(|s| s.to_owned());
        Box::pin(async move { client.get_stream(uri, last_event_id, None).await })
    }

    fn handle_control_event(&mut self, event: &Sse) -> Result<(), Self::Error> {
        if event.event.as_deref() != Some("endpoint") {
            return Ok(());
        }
        let Some(data) = event.data.as_ref() else {
            return Ok(());
        };
        let new_endpoint = message_endpoint(self.uri.clone(), data.clone())
            .map_err(SseTransportError::InvalidUri)?;
        // 拒绝异源重定向：忽略这次更新、保留原端点，而非终止流——
        // 一次可疑的 endpoint 帧不该整条断连。
        if !same_origin(&new_endpoint, &self.uri) {
            tracing::warn!(uri = %self.uri, endpoint = %new_endpoint,
                "拒绝跨源 SSE message endpoint，保留原端点");
            return Ok(());
        }
        // 中毒即收拾着用（into_inner）：临界区只有一次赋值/克隆，真中毒也
        // 只是别的持锁线程 panic 过，这里 panic 会把整条 SSE 传输陪葬。
        *self
            .message_endpoint
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = new_endpoint;
        Ok(())
    }

    fn handle_stream_error(
        &mut self,
        error: &(dyn std::error::Error + 'static),
        last_event_id: Option<&str>,
    ) {
        tracing::warn!(
            uri = %self.uri,
            last_event_id = last_event_id.unwrap_or(""),
            "sse stream error: {error}"
        );
    }
}

type ServerMessageStream<C> = Pin<Box<SseAutoReconnectStream<SseClientReconnect<C>>>>;

/// 客户端无关的 SSE 传输：`GET` 长连接收消息、`POST` 到 endpoint 事件解析出的
/// 端点发消息，断流按 retry policy 自动重连。
pub struct SseClientTransport<C: SseClient> {
    client: C,
    config: SseClientConfig,
    /// 当前 POST 端点；服务端重发 endpoint 控制帧时随之刷新。
    message_endpoint: Arc<RwLock<Uri>>,
    stream: Option<ServerMessageStream<C>>,
}

impl<C: SseClient> Transport<RoleClient> for SseClientTransport<C> {
    type Error = SseTransportError<C::Error>;
    async fn receive(&mut self) -> Option<ServerJsonRpcMessage> {
        self.stream.as_mut()?.next().await?.ok()
    }
    fn send(
        &mut self,
        item: rmcp::service::TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let client = self.client.clone();
        let message_endpoint = self.message_endpoint.clone();
        async move {
            let uri = {
                let guard = message_endpoint
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                guard.clone()
            };
            client.post_message(uri, item, None).await
        }
    }
    async fn close(&mut self) -> Result<(), Self::Error> {
        self.stream.take();
        Ok(())
    }
}

impl<C: SseClient + std::fmt::Debug> std::fmt::Debug for SseClientTransport<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SseClientWorker")
            .field("client", &self.client)
            .field("config", &self.config)
            .finish()
    }
}

impl<C: SseClient> SseClientTransport<C> {
    pub async fn start_with_client(
        client: C,
        config: SseClientConfig,
    ) -> Result<Self, SseTransportError<C::Error>> {
        let sse_endpoint = config.sse_endpoint.as_ref().parse::<http::Uri>()?;

        let mut sse_stream = client.get_stream(sse_endpoint.clone(), None, None).await?;
        let initial_message_endpoint = if let Some(endpoint) = config.use_message_endpoint.clone() {
            let ep = endpoint.parse::<http::Uri>()?;
            let mut sse_endpoint_parts = sse_endpoint.clone().into_parts();
            sse_endpoint_parts.path_and_query = ep.into_parts().path_and_query;
            Uri::from_parts(sse_endpoint_parts)?
        } else {
            // 等 endpoint 事件。
            loop {
                let sse = sse_stream
                    .next()
                    .await
                    .ok_or(SseTransportError::UnexpectedEndOfStream)??;
                let Some("endpoint") = sse.event.as_deref() else {
                    continue;
                };
                let ep = sse.data.unwrap_or_default();

                let resolved = message_endpoint(sse_endpoint.clone(), ep)?;
                // 初次握手 endpoint 同样校验同源：绝对 URL 指向异源主机时拒绝
                // 连接（相对路径保留 sse_endpoint 的 authority，天然同源）。
                if !same_origin(&resolved, &sse_endpoint) {
                    return Err(SseTransportError::CrossOriginEndpoint(resolved.to_string()));
                }
                break resolved;
            }
        };
        let message_endpoint = Arc::new(RwLock::new(initial_message_endpoint));

        let stream = Box::pin(SseAutoReconnectStream::new(
            sse_stream,
            SseClientReconnect {
                client: client.clone(),
                uri: sse_endpoint.clone(),
                message_endpoint: message_endpoint.clone(),
            },
            config.retry_policy.clone(),
        ));
        Ok(Self {
            client,
            config,
            message_endpoint,
            stream: Some(stream),
        })
    }
}

fn message_endpoint(base: http::Uri, endpoint: String) -> Result<http::Uri, http::uri::InvalidUri> {
    // 完整 URL 直接解析返回。
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        return endpoint.parse::<http::Uri>();
    }

    let mut base_parts = base.into_parts();

    if endpoint.starts_with("?") {
        // 仅 query：保留 base 路径追加 query。
        if let Some(base_path_and_query) = &base_parts.path_and_query {
            let base_path = base_path_and_query.path();
            base_parts.path_and_query = Some(format!("{base_path}{endpoint}").parse()?);
        } else {
            base_parts.path_and_query = Some(format!("/{endpoint}").parse()?);
        }
    } else {
        // 路径（可带 query）：整体替换 path_and_query。
        let path_to_use = if endpoint.starts_with("/") {
            endpoint
        } else {
            format!("/{endpoint}")
        };
        base_parts.path_and_query = Some(path_to_use.parse()?);
    }

    // from_parts 失败（base 缺 scheme/authority 等畸形形态）时需要一个
    // InvalidUri 作错误值，而 http crate 不提供直接构造。旧写法
    // `endpoint_clone.parse().unwrap_err()` 赌 endpoint 也解析失败——
    // endpoint 是纯路径（如 `message?sid=1`）时恰好能解析成功，
    // unwrap_err 反向 panic 在 SSE 轮询任务里。改为解析一个必然非法的
    // 串取得 InvalidUri，携带信息由上层日志的 endpoint 字段补足。
    http::Uri::from_parts(base_parts).map_err(|_| {
        #[allow(clippy::unwrap_used)]
        "".parse::<http::Uri>().unwrap_err()
    })
}

#[derive(Debug, Clone)]
pub struct SseClientConfig {
    /// SSE 端点。消息端点按 JavaScript `new URL(url, base)` 规则从
    /// endpoint 事件数据解析（如 base `http://ex.com/some_path/sse` +
    /// 事件 `message?session_id=123` → `http://ex.com/message?session_id=123`）。
    pub sse_endpoint: Arc<str>,
    pub retry_policy: Arc<dyn SseRetryPolicy>,
    /// 指定后跳过 endpoint 事件等待，直接以该端点发消息。
    pub use_message_endpoint: Option<String>,
}

impl Default for SseClientConfig {
    fn default() -> Self {
        Self {
            sse_endpoint: "".into(),
            retry_policy: Arc::new(FixedInterval::default()),
            use_message_endpoint: None,
        }
    }
}

// ---------- reqwest 实现 ----------

impl From<reqwest::Error> for SseTransportError<reqwest::Error> {
    fn from(e: reqwest::Error) -> Self {
        SseTransportError::Client(e)
    }
}

impl SseClient for reqwest::Client {
    type Error = reqwest::Error;

    async fn post_message(
        &self,
        uri: Uri,
        message: ClientJsonRpcMessage,
        auth_token: Option<String>,
    ) -> Result<(), SseTransportError<Self::Error>> {
        let mut request_builder = self.post(uri.to_string()).json(&message);
        if let Some(auth_header) = auth_token {
            request_builder = request_builder.bearer_auth(auth_header);
        }
        request_builder
            .send()
            .await
            .and_then(|resp| resp.error_for_status())
            .map_err(SseTransportError::from)
            .map(drop)
    }

    async fn get_stream(
        &self,
        uri: Uri,
        last_event_id: Option<String>,
        auth_token: Option<String>,
    ) -> Result<BoxedSseResponse, SseTransportError<Self::Error>> {
        let mut request_builder = self
            .get(uri.to_string())
            .header(reqwest::header::ACCEPT, EVENT_STREAM_MIME_TYPE);
        if let Some(auth_header) = auth_token {
            request_builder = request_builder.bearer_auth(auth_header);
        }
        if let Some(last_event_id) = last_event_id {
            request_builder = request_builder.header(HEADER_LAST_EVENT_ID, last_event_id);
        }
        let response = request_builder.send().await?;
        let response = response.error_for_status()?;
        match response.headers().get(reqwest::header::CONTENT_TYPE) {
            Some(ct) => {
                if !ct.as_bytes().starts_with(EVENT_STREAM_MIME_TYPE.as_bytes()) {
                    return Err(SseTransportError::UnexpectedContentType(Some(
                        String::from_utf8_lossy(ct.as_bytes()).to_string(),
                    )));
                }
            }
            None => {
                return Err(SseTransportError::UnexpectedContentType(None));
            }
        }
        // 上游 0.8.5 用的 from_byte_stream 在 sse-stream 0.2 已标弃用（typo），
        // 此处按其提示改用 from_bytes_stream，行为一致。
        let event_stream = SseStream::from_bytes_stream(response.bytes_stream()).boxed();
        Ok(event_stream)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn message_endpoint_resolution_follows_url_join_rules() {
        let base_url = "https://localhost/sse".parse::<http::Uri>().unwrap();

        // 仅 query。
        let result = message_endpoint(base_url.clone(), "?sessionId=x".to_string()).unwrap();
        assert_eq!(result.to_string(), "https://localhost/sse?sessionId=x");

        // 相对路径带 query。
        let result = message_endpoint(base_url.clone(), "mypath?sessionId=x".to_string()).unwrap();
        assert_eq!(result.to_string(), "https://localhost/mypath?sessionId=x");

        // 绝对路径带 query。
        let result = message_endpoint(base_url.clone(), "/xxx?sessionId=x".to_string()).unwrap();
        assert_eq!(result.to_string(), "https://localhost/xxx?sessionId=x");

        // 完整 URL。
        let result = message_endpoint(
            base_url.clone(),
            "http://example.com/xxx?sessionId=x".to_string(),
        )
        .unwrap();
        assert_eq!(result.to_string(), "http://example.com/xxx?sessionId=x");
    }

    #[derive(Clone)]
    struct DummyClient;

    #[derive(Debug, thiserror::Error)]
    #[error("dummy error")]
    struct DummyError;

    impl SseClient for DummyClient {
        type Error = DummyError;

        async fn post_message(
            &self,
            _uri: Uri,
            _message: ClientJsonRpcMessage,
            _auth_token: Option<String>,
        ) -> Result<(), SseTransportError<Self::Error>> {
            Ok(())
        }

        async fn get_stream(
            &self,
            _uri: Uri,
            _last_event_id: Option<String>,
            _auth_token: Option<String>,
        ) -> Result<BoxedSseResponse, SseTransportError<Self::Error>> {
            unreachable!("get_stream should not be called in this test")
        }
    }

    #[test]
    fn handle_endpoint_control_event_updates_uri() {
        let initial_endpoint = "https://example.com/message?sessionId=old"
            .parse::<Uri>()
            .unwrap();
        let shared_endpoint = Arc::new(RwLock::new(initial_endpoint));
        let mut reconnect = SseClientReconnect {
            client: DummyClient,
            uri: "https://example.com/sse".parse::<Uri>().unwrap(),
            message_endpoint: shared_endpoint.clone(),
        };

        let control_event = Sse::default()
            .event("endpoint")
            .data("/message?sessionId=new");

        reconnect.handle_control_event(&control_event).unwrap();

        let guard = shared_endpoint.read().expect("lock poisoned");
        assert_eq!(
            guard.to_string(),
            "https://example.com/message?sessionId=new"
        );
    }

    /// 安全：SSE 流下发的跨源绝对 endpoint 被拒绝，原端点保留（不被劫持）。
    #[test]
    fn cross_origin_endpoint_is_rejected_and_original_kept() {
        let initial = "https://example.com/message?sessionId=old"
            .parse::<Uri>()
            .unwrap();
        let shared = Arc::new(RwLock::new(initial.clone()));
        let mut reconnect = SseClientReconnect {
            client: DummyClient,
            uri: "https://example.com/sse".parse::<Uri>().unwrap(),
            message_endpoint: shared.clone(),
        };
        // 攻击者注入指向 attacker 主机的绝对 endpoint。
        let evil = Sse::default()
            .event("endpoint")
            .data("http://attacker.example/steal");
        reconnect.handle_control_event(&evil).unwrap();
        assert_eq!(
            shared.read().unwrap().to_string(),
            initial.to_string(),
            "跨源 endpoint 必须被忽略，原端点保留"
        );

        // 同源绝对 endpoint 仍接受。
        let ok = Sse::default()
            .event("endpoint")
            .data("https://example.com/msg2");
        reconnect.handle_control_event(&ok).unwrap();
        assert_eq!(
            shared.read().unwrap().to_string(),
            "https://example.com/msg2"
        );
    }
}
