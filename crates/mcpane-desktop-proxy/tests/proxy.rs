//! 网关端到端测试：假上游（axum）断言转发语义，reqwest 扮演 Claude Desktop。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::post;
use serde_json::{Value, json};

use mcpane_desktop_proxy::{DesktopUpstream, ProxyServer, ProxyState};
use mcpane_provider::DesktopRoute;

const TOKEN: &str = "mcpane-test-token";

#[derive(Default)]
struct Captured {
    body: Option<Value>,
    headers: Option<HeaderMap>,
    query: Option<String>,
}

type Capture = Arc<Mutex<Captured>>;

/// 假上游：记录请求，返回固定 JSON。
async fn upstream_json(
    State(capture): State<Capture>,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response<Body> {
    let mut captured = capture.lock().unwrap();
    captured.body = serde_json::from_slice(&body).ok();
    captured.headers = Some(headers);
    captured.query = query;
    Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .header("x-upstream-mark", "yes")
        .body(Body::from(
            json!({"type": "message", "model": "unchanged"}).to_string(),
        ))
        .unwrap()
}

/// 假上游：SSE 流式返回两块数据。
async fn upstream_sse(
    State(capture): State<Capture>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response<Body> {
    let mut captured = capture.lock().unwrap();
    captured.body = serde_json::from_slice(&body).ok();
    captured.headers = Some(headers);
    let chunks: Vec<Result<&'static str, std::io::Error>> = vec![
        Ok("event: message_start\ndata: {\"type\":\"message_start\"}\n\n"),
        Ok("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"),
    ];
    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(futures::stream::iter(chunks)))
        .unwrap()
}

async fn spawn_upstream(sse: bool) -> (u16, Capture) {
    let capture: Capture = Arc::default();
    let route = if sse {
        post(upstream_sse)
    } else {
        post(upstream_json)
    };
    let app = Router::new()
        .route("/anthropic/v1/messages", route)
        .with_state(capture.clone());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (port, capture)
}

fn routes() -> Vec<DesktopRoute> {
    vec![DesktopRoute {
        route_id: "claude-sonnet-5".into(),
        upstream_model: "deepseek-v4-pro".into(),
        label_override: Some("DeepSeek".into()),
        supports_1m: false,
    }]
}

async fn spawn_proxy(upstream_port: u16, use_x_api_key: bool) -> (ProxyServer, ProxyState) {
    let state = ProxyState::new(TOKEN);
    state.set_upstream(Some(
        DesktopUpstream::new(
            format!("http://127.0.0.1:{upstream_port}/anthropic/"),
            "sk-upstream",
            use_x_api_key,
            routes(),
        )
        .unwrap(),
    ));
    let server = ProxyServer::start(0, state.clone()).await.unwrap();
    (server, state)
}

fn desktop_client() -> reqwest::Client {
    reqwest::Client::new()
}

#[tokio::test]
async fn rejects_bad_gateway_token() {
    let (upstream_port, _capture) = spawn_upstream(false).await;
    let (server, _state) = spawn_proxy(upstream_port, false).await;
    let resp = desktop_client()
        .post(format!(
            "http://127.0.0.1:{}/claude-desktop/v1/messages",
            server.port()
        ))
        .bearer_auth("wrong-token")
        .json(&json!({"model": "claude-sonnet-5", "messages": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["type"], "authentication_error");
    server.shutdown().await;
}

#[tokio::test]
async fn maps_model_and_injects_bearer_auth() {
    let (upstream_port, capture) = spawn_upstream(false).await;
    let (server, _state) = spawn_proxy(upstream_port, false).await;
    let resp = desktop_client()
        .post(format!(
            "http://127.0.0.1:{}/claude-desktop/v1/messages?beta=true",
            server.port()
        ))
        .bearer_auth(TOKEN)
        // 客户端已带 beta 标（不含 claude-code）：重建值应前置 claude-code。
        .header("anthropic-beta", "context-1m-2025-08-07")
        // [1m] 后缀在映射边界剥离。
        .json(&json!({"model": "claude-sonnet-5[1m]", "messages": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    // 上游收到映射后的真实模型 + Bearer 凭据 + claude-code beta 标 + 透传 query。
    {
        let captured = capture.lock().unwrap();
        assert_eq!(captured.body.as_ref().unwrap()["model"], "deepseek-v4-pro");
        let headers = captured.headers.as_ref().unwrap();
        assert_eq!(
            headers.get("authorization").unwrap().to_str().unwrap(),
            "Bearer sk-upstream"
        );
        // 前置合并（逐位对齐 cc-switch 重建格式）。
        assert_eq!(
            headers.get("anthropic-beta").unwrap().to_str().unwrap(),
            "claude-code-20250219,context-1m-2025-08-07"
        );
        assert_eq!(captured.query.as_deref(), Some("beta=true"));
    }
    server.shutdown().await;
}

#[tokio::test]
async fn injects_x_api_key_when_declared() {
    let (upstream_port, capture) = spawn_upstream(false).await;
    let (server, _state) = spawn_proxy(upstream_port, true).await;
    desktop_client()
        .post(format!(
            "http://127.0.0.1:{}/claude-desktop/v1/messages",
            server.port()
        ))
        .bearer_auth(TOKEN)
        .json(&json!({"model": "claude-sonnet-5", "messages": []}))
        .send()
        .await
        .unwrap();
    {
        let captured = capture.lock().unwrap();
        let headers = captured.headers.as_ref().unwrap();
        assert_eq!(
            headers.get("x-api-key").unwrap().to_str().unwrap(),
            "sk-upstream"
        );
        // Bearer 形态的 authorization 不应同时出现（替换而非叠加）。
        assert!(!headers.contains_key("authorization"));
    }
    server.shutdown().await;
}

#[tokio::test]
async fn streams_sse_bytes_through_unchanged() {
    let (upstream_port, capture) = spawn_upstream(true).await;
    let (server, _state) = spawn_proxy(upstream_port, false).await;
    let resp = desktop_client()
        .post(format!(
            "http://127.0.0.1:{}/claude-desktop/v1/messages",
            server.port()
        ))
        .bearer_auth(TOKEN)
        .json(&json!({"model": "claude-sonnet-5", "messages": [], "stream": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "text/event-stream"
    );
    let body = resp.text().await.unwrap();
    // 逐字节透传：两条事件原样到达。
    assert_eq!(
        body,
        "event: message_start\ndata: {\"type\":\"message_start\"}\n\n\
         event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    // SSE 路径强制 identity 编码。
    {
        let captured = capture.lock().unwrap();
        let headers = captured.headers.as_ref().unwrap();
        assert_eq!(
            headers.get("accept-encoding").unwrap().to_str().unwrap(),
            "identity"
        );
    }
    server.shutdown().await;
}

#[tokio::test]
async fn models_endpoint_serves_route_table() {
    let (upstream_port, _capture) = spawn_upstream(false).await;
    let (server, _state) = spawn_proxy(upstream_port, false).await;
    let resp = desktop_client()
        .get(format!(
            "http://127.0.0.1:{}/claude-desktop/v1/models",
            server.port()
        ))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["data"][0]["id"], "claude-sonnet-5");
    assert_eq!(body["has_more"], false);
    server.shutdown().await;
}

#[tokio::test]
async fn unknown_model_yields_400_and_no_upstream_yields_503() {
    let (upstream_port, _capture) = spawn_upstream(false).await;
    let (server, state) = spawn_proxy(upstream_port, false).await;
    // 未配置路由的非 Claude 模型 → 400。
    let resp = desktop_client()
        .post(format!(
            "http://127.0.0.1:{}/claude-desktop/v1/messages",
            server.port()
        ))
        .bearer_auth(TOKEN)
        .json(&json!({"model": "gpt-5.6", "messages": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    // 上游快照撤除（切回官方）→ 503。
    state.set_upstream(None);
    let resp = desktop_client()
        .post(format!(
            "http://127.0.0.1:{}/claude-desktop/v1/messages",
            server.port()
        ))
        .bearer_auth(TOKEN)
        .json(&json!({"model": "claude-sonnet-5", "messages": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
    server.shutdown().await;
}
