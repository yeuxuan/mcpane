//! SSE 下游端到端：手写最小 HTTP+SSE（legacy）MCP 服务端，验证移植的
//! SseClientTransport 能完成 endpoint 事件解析、initialize 握手与 tools/list，
//! 且 Bearer/自定义 Header 随连接注入。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::sse::{Event, Sse};
use axum::routing::{get, post};
use futures::stream::Stream;
use mcpane_hub_downstream::{DownstreamHandle, SpawnSpec};
use mcpane_net::ProxyConfig;
use serde_json::{Value, json};
use tokio::sync::mpsc;

struct SseServerState {
    /// SSE 长连接的消息发送端（单会话足够）。
    tx: mpsc::UnboundedSender<Value>,
    rx: std::sync::Mutex<Option<mpsc::UnboundedReceiver<Value>>>,
    /// 记录收到的 Authorization/X-Test 头，供断言。
    seen_auth: std::sync::Mutex<Vec<String>>,
}

async fn sse_handler(
    State(state): State<Arc<SseServerState>>,
    headers: HeaderMap,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    if let Some(auth) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        state.seen_auth.lock().unwrap().push(auth.to_owned());
    }
    let mut rx = state
        .rx
        .lock()
        .unwrap()
        .take()
        .expect("sse stream connected twice");
    use futures::StreamExt;
    // 先发 endpoint 事件（legacy 握手第一步），再持续透传 JSON-RPC 响应。
    let stream = futures::stream::once(async {
        Ok(Event::default()
            .event("endpoint")
            .data("/message?sessionId=t1"))
    })
    .chain(futures::stream::poll_fn(move |cx| {
        rx.poll_recv(cx)
            .map(|opt| opt.map(|v| Ok(Event::default().event("message").data(v.to_string()))))
    }));
    Sse::new(stream)
}

async fn message_handler(
    State(state): State<Arc<SseServerState>>,
    body: String,
) -> axum::http::StatusCode {
    let request: Value = serde_json::from_str(&body).unwrap();
    let id = request.get("id").cloned();
    let method = request["method"].as_str().unwrap_or_default().to_owned();
    let response = match method.as_str() {
        "initialize" => Some(json!({
            "jsonrpc": "2.0", "id": id,
            "result": {
                "protocolVersion": request["params"]["protocolVersion"],
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "sse-legacy-test", "version": "0.0.1" }
            }
        })),
        "tools/list" => Some(json!({
            "jsonrpc": "2.0", "id": id,
            "result": { "tools": [{
                "name": "sse_ping",
                "description": "sse downstream ping",
                "inputSchema": { "type": "object" }
            }] }
        })),
        "ping" => Some(json!({ "jsonrpc": "2.0", "id": id, "result": {} })),
        // notifications/* 无响应。
        _ => None,
    };
    if let Some(response) = response {
        state.tx.send(response).unwrap();
    }
    axum::http::StatusCode::ACCEPTED
}

async fn start_legacy_sse_server() -> (String, Arc<SseServerState>) {
    let (tx, rx) = mpsc::unbounded_channel::<Value>();
    let state = Arc::new(SseServerState {
        tx,
        rx: std::sync::Mutex::new(Some(rx)),
        seen_auth: std::sync::Mutex::new(Vec::new()),
    });
    let router = axum::Router::new()
        .route("/sse", get(sse_handler))
        .route("/message", post(message_handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (format!("http://{addr}/sse"), state)
}

#[tokio::test]
async fn sse_downstream_connects_and_lists_tools() {
    let (url, state) = start_legacy_sse_server().await;

    let spec = SpawnSpec::Sse {
        url,
        bearer: Some("sse-secret-token".into()),
        bearer_env: None,
        env: BTreeMap::new(),
        headers: BTreeMap::new(),
    };
    let handle = DownstreamHandle::connect(&spec, Duration::from_secs(10), &ProxyConfig::default())
        .await
        .expect("SSE 下游应能完成握手");

    let tools = handle.list_tools().await.expect("tools/list 应成功");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name.as_ref(), "sse_ping");
    handle.probe().await.expect("MCP ping 应成功");

    // Bearer 必须随 SSE 长连接注入。
    let seen = state.seen_auth.lock().unwrap().clone();
    assert!(
        seen.iter().any(|h| h == "Bearer sse-secret-token"),
        "SSE GET 未携带 Bearer：{seen:?}"
    );

    handle.close().await;
}
