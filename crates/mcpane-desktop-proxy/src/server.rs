//! 本地路由 HTTP 面：绑定 loopback、鉴权、按命名空间分发。
//!
//! `/claude-desktop` 对齐 cc-switch 的 Claude Desktop 命名空间（/v1/messages、
//! /v1/models）；`/claude`、`/codex` 为路由接管命名空间，其后路径原样拼到上游。

use axum::Router;
use axum::body::Body;
use axum::extract::FromRequestParts;
use axum::extract::Path;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, Response, StatusCode};
use axum::routing::{any, get, post};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use mcpane_provider::{ClientApp, claude_desktop};

use crate::router::{RoutedRequest, app_error, forward_routed};
use crate::{ProxyState, forward};

/// Anthropic 长上下文请求（含图片/大历史）远超 axum 默认 2MB 限制。
const MAX_BODY_BYTES: usize = 128 * 1024 * 1024;

/// 运行中的代理服务器句柄；drop 或显式 shutdown 即优雅停机。
pub struct ProxyServer {
    port: u16,
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl ProxyServer {
    /// 绑定 `127.0.0.1:{listen_port}`（0 = 临时端口，实际值经 `port()` 读回）。
    pub async fn start(listen_port: u16, state: ProxyState) -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", listen_port))
            .await
            .map_err(|e| format!("绑定 127.0.0.1:{listen_port} 失败：{e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("读取监听地址失败：{e}"))?
            .port();
        let app = router(state);
        let (tx, rx) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let serve = axum::serve(listener, app).with_graceful_shutdown(async {
                let _ = rx.await;
            });
            if let Err(error) = serve.await {
                tracing::error!("Claude Desktop 网关退出异常：{error}");
            }
        });
        Ok(Self {
            port,
            shutdown: Some(tx),
            task,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// 优雅停机并等待任务退出。
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        let _ = (&mut self.task).await;
    }
}

impl Drop for ProxyServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

fn router(state: ProxyState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/claude-desktop/v1/models", get(handle_models))
        .route("/claude-desktop/v1/messages", post(handle_messages))
        .route("/claude/{*rest}", any(handle_claude))
        .route("/codex/{*rest}", any(handle_codex))
        .route("/gemini/{*rest}", any(handle_gemini))
        .route("/grok/{*rest}", any(handle_grok))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

/// Anthropic 形态的错误响应（Desktop 按此结构展示错误信息）。
pub(crate) fn anthropic_error(
    status: StatusCode,
    error_type: &str,
    message: &str,
) -> Response<Body> {
    let body = serde_json::json!({
        "type": "error",
        "error": { "type": error_type, "message": message }
    });
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap_or_default()
}

/// 校验 gateway token（Bearer / x-api-key / x-goog-api-key）；恒定时比较不必要（token 为本机
/// loopback 门闩，非跨网秘密），按字符串相等即可。
fn authorize(headers: &HeaderMap, expected: &str) -> bool {
    let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    !expected.is_empty()
        && header("authorization")
            .and_then(|value| value.strip_prefix("Bearer "))
            .or_else(|| header("x-api-key"))
            .or_else(|| header("x-goog-api-key"))
            .is_some_and(|token| token.trim() == expected)
}

/// 路由命名空间的请求提取（各命名空间只差客户端标识）。
struct Routed(RoutedRequest);

impl<S: Send + Sync> axum::extract::FromRequest<S> for Routed {
    type Rejection = Response<Body>;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        let (mut parts, body) = req.into_parts();
        let Path(rest) = Path::<String>::from_request_parts(&mut parts, state)
            .await
            .map_err(axum::response::IntoResponse::into_response)?;
        let body = axum::body::to_bytes(body, MAX_BODY_BYTES)
            .await
            .map_err(|_| {
                anthropic_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "request_too_large",
                    "请求体过大",
                )
            })?;
        Ok(Self(RoutedRequest {
            method: parts.method,
            rest,
            query: parts.uri.query().map(str::to_owned),
            headers: parts.headers,
            body: body.to_vec(),
        }))
    }
}

async fn handle_claude(State(state): State<ProxyState>, Routed(request): Routed) -> Response<Body> {
    handle_routed(state, ClientApp::Claude, request).await
}

async fn handle_codex(State(state): State<ProxyState>, Routed(request): Routed) -> Response<Body> {
    handle_routed(state, ClientApp::Codex, request).await
}

async fn handle_gemini(State(state): State<ProxyState>, Routed(request): Routed) -> Response<Body> {
    handle_routed(state, ClientApp::Gemini, request).await
}

async fn handle_grok(State(state): State<ProxyState>, Routed(request): Routed) -> Response<Body> {
    handle_routed(state, ClientApp::Grok, request).await
}

async fn handle_routed(
    state: ProxyState,
    app: ClientApp,
    request: RoutedRequest,
) -> Response<Body> {
    let (token, route) = state.route_snapshot(app);
    if !authorize(&request.headers, &token) {
        return app_error(
            app,
            StatusCode::UNAUTHORIZED,
            "无效的本地路由 token；请在 MCPane 中重新切换一次供应商",
        );
    }
    let Some(route) = route else {
        return app_error(
            app,
            StatusCode::SERVICE_UNAVAILABLE,
            "本地路由未接管该客户端；请在 MCPane 供应商页开启本地路由或切换供应商",
        );
    };
    forward_routed(&state, app, route, request).await
}

async fn handle_models(State(state): State<ProxyState>, headers: HeaderMap) -> Response<Body> {
    let (token, upstream) = state.snapshot();
    if !authorize(&headers, &token) {
        return anthropic_error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "无效的本地网关 token；重新切换该供应商可写入新 token",
        );
    }
    let Some(upstream) = upstream else {
        return no_upstream_response();
    };
    let body = claude_desktop::model_list_response(&upstream.routes_resolved);
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap_or_default()
}

async fn handle_messages(
    State(state): State<ProxyState>,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response<Body> {
    let (token, upstream) = state.snapshot();
    if !authorize(&headers, &token) {
        return anthropic_error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "无效的本地网关 token；重新切换该供应商可写入新 token",
        );
    }
    let Some(upstream) = upstream else {
        return no_upstream_response();
    };
    let body: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => {
            return anthropic_error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                &format!("请求体不是合法 JSON：{error}"),
            );
        }
    };
    forward::forward_messages(upstream, headers, query, body).await
}

fn no_upstream_response() -> Response<Body> {
    anthropic_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "api_error",
        "当前没有生效的 Claude Desktop 本地路由供应商；请在 MCPane 中切换后重试",
    )
}
