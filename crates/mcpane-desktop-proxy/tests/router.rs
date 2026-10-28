//! 本地路由端到端测试：假上游按预设状态码应答，断言故障转移、熔断与模型映射。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::any;
use serde_json::{Value, json};

use mcpane_desktop_proxy::{
    AppRoute, BreakerState, ModelSlots, ProxyServer, ProxyState, RouteUpstream, UpstreamAuth,
    UpstreamFormat,
};
use mcpane_provider::ClientApp;

const TOKEN: &str = "mcpane-router-token";

#[derive(Default)]
struct Upstream {
    status: u16,
    sse: bool,
    hits: AtomicUsize,
    last_body: Mutex<Option<Value>>,
    last_path: Mutex<Option<String>>,
    last_auth: Mutex<Option<String>>,
}

async fn handler(
    State(up): State<Arc<Upstream>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response<Body> {
    up.hits.fetch_add(1, Ordering::SeqCst);
    *up.last_body.lock().unwrap() = serde_json::from_slice(&body).ok();
    *up.last_path.lock().unwrap() = Some(uri.to_string());
    *up.last_auth.lock().unwrap() = headers
        .get("authorization")
        .or_else(|| headers.get("x-api-key"))
        .or_else(|| headers.get("x-goog-api-key"))
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    if up.sse {
        let chunks: Vec<Result<&'static str, std::io::Error>> = vec![
            Ok("event: a\ndata: {\"n\":1}\n\n"),
            Ok("event: b\ndata: {\"n\":2}\n\n"),
        ];
        return Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(Body::from_stream(futures::stream::iter(chunks)))
            .unwrap();
    }
    Response::builder()
        .status(up.status)
        .header("content-type", "application/json")
        .body(Body::from(json!({"served": up.status}).to_string()))
        .unwrap()
}

async fn spawn_upstream(status: u16, sse: bool) -> (String, Arc<Upstream>) {
    let up = Arc::new(Upstream {
        status,
        sse,
        ..Default::default()
    });
    let app = Router::new().fallback(any(handler)).with_state(up.clone());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://127.0.0.1:{port}/api"), up)
}

fn candidate(id: &str, base_url: String, main: &str, haiku: Option<&str>) -> Arc<RouteUpstream> {
    Arc::new(RouteUpstream {
        provider_id: id.into(),
        name: format!("供应商{id}"),
        base_url,
        api_key: format!("sk-{id}"),
        auth: UpstreamAuth::Bearer,
        models: ModelSlots::resolve(Some(main.into()), haiku.map(Into::into), None, None, None),
        format: UpstreamFormat::Native,
        headers: Vec::new(),
        body_override: None,
    })
}

async fn spawn_router(
    app: ClientApp,
    candidates: Vec<Arc<RouteUpstream>>,
) -> (ProxyServer, ProxyState) {
    let state = ProxyState::new(TOKEN);
    state.set_route(
        app,
        Some(AppRoute {
            candidates,
            ..AppRoute::default()
        }),
    );
    let server = ProxyServer::start(0, state.clone()).await.unwrap();
    (server, state)
}

async fn post_claude(port: u16, token: &str, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{port}/claude/v1/messages?beta=true"
        ))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn fails_over_to_queue_and_maps_model_role() {
    let (base_a, a) = spawn_upstream(503, false).await;
    let (base_b, b) = spawn_upstream(200, false).await;
    let (server, state) = spawn_router(
        ClientApp::Claude,
        vec![
            candidate("a", base_a, "glm-5", Some("glm-air")),
            candidate("b", base_b, "kimi-k3", Some("kimi-turbo")),
        ],
    )
    .await;

    let resp = post_claude(
        server.port(),
        TOKEN,
        json!({"model": "glm-air", "messages": []}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.json::<Value>().await.unwrap()["served"], 200);
    assert_eq!(a.hits.load(Ordering::SeqCst), 1);
    assert_eq!(b.hits.load(Ordering::SeqCst), 1);
    // 当前供应商原样收到请求；备用收到映射后的同档位模型与自己的 Key。
    assert_eq!(
        a.last_body.lock().unwrap().as_ref().unwrap()["model"],
        "glm-air"
    );
    assert_eq!(
        b.last_body.lock().unwrap().as_ref().unwrap()["model"],
        "kimi-turbo"
    );
    assert_eq!(b.last_auth.lock().unwrap().as_deref(), Some("Bearer sk-b"));
    assert_eq!(
        b.last_path.lock().unwrap().as_deref(),
        Some("/api/v1/messages?beta=true")
    );

    let status = state.route_status();
    assert_eq!(status[0].last_served_by.as_deref(), Some("b"));
    assert_eq!(status[0].candidates[0].breaker.consecutive_failures, 1);
    assert_eq!(status[0].candidates[0].breaker.failed, 1);
    assert_eq!(status[0].candidates[1].breaker.served, 1);
    server.shutdown().await;
}

#[tokio::test]
async fn non_retryable_error_is_passed_through_without_failover() {
    let (base_a, a) = spawn_upstream(400, false).await;
    let (base_b, b) = spawn_upstream(200, false).await;
    let (server, _state) = spawn_router(
        ClientApp::Claude,
        vec![
            candidate("a", base_a, "m", None),
            candidate("b", base_b, "m", None),
        ],
    )
    .await;
    let resp = post_claude(server.port(), TOKEN, json!({"model": "m"})).await;
    assert_eq!(resp.status(), 400);
    assert_eq!(a.hits.load(Ordering::SeqCst), 1);
    assert_eq!(b.hits.load(Ordering::SeqCst), 0);
    server.shutdown().await;
}

/// 5xx 类软故障：首次转移后进入临时接替，后续请求直接走接替者、不再先打坏主站；
/// 手动恢复主站即结束接替，下个请求回到主站。
#[tokio::test]
async fn soft_failure_starts_stand_in_until_primary_is_resumed() {
    let (base_a, a) = spawn_upstream(500, false).await;
    let (base_b, b) = spawn_upstream(200, false).await;
    let (server, state) = spawn_router(
        ClientApp::Claude,
        vec![
            candidate("a", base_a, "m", None),
            candidate("b", base_b, "n", None),
        ],
    )
    .await;
    for _ in 0..4 {
        let resp = post_claude(server.port(), TOKEN, json!({"model": "m"})).await;
        assert_eq!(resp.status(), 200);
    }
    assert_eq!(a.hits.load(Ordering::SeqCst), 1, "接替期间不再白打主站");
    assert_eq!(b.hits.load(Ordering::SeqCst), 4);
    let status = state.route_status();
    assert_eq!(status[0].sticky.as_ref().unwrap().provider_id, "b");
    // 软故障只记一次失败，未到熔断阈值。
    assert_eq!(status[0].candidates[0].breaker.state, BreakerState::Closed);

    state.reset_breaker(ClientApp::Claude, "a");
    post_claude(server.port(), TOKEN, json!({"model": "m"})).await;
    assert_eq!(a.hits.load(Ordering::SeqCst), 2, "恢复后回到主站");
    server.shutdown().await;
}

#[tokio::test]
async fn last_candidate_error_is_relayed_and_all_down_reports_502() {
    let (base_a, _a) = spawn_upstream(429, false).await;
    let (server, _state) =
        spawn_router(ClientApp::Claude, vec![candidate("a", base_a, "m", None)]).await;
    let resp = post_claude(server.port(), TOKEN, json!({"model": "m"})).await;
    assert_eq!(resp.status(), 429, "唯一候选的上游错误原样透传");
    server.shutdown().await;

    // 上游连不上：返回 Anthropic 形态的 502。
    let (server, _state) = spawn_router(
        ClientApp::Claude,
        vec![candidate("x", "http://127.0.0.1:9/api".into(), "m", None)],
    )
    .await;
    let resp = post_claude(server.port(), TOKEN, json!({"model": "m"})).await;
    assert_eq!(resp.status(), 502);
    assert_eq!(resp.json::<Value>().await.unwrap()["type"], "error");
    server.shutdown().await;
}

#[tokio::test]
async fn codex_namespace_streams_and_rejects_bad_token() {
    let (base, up) = spawn_upstream(200, true).await;
    let (server, _state) =
        spawn_router(ClientApp::Codex, vec![candidate("c", base, "gpt", None)]).await;
    let url = format!("http://127.0.0.1:{}/codex/responses", server.port());

    let resp = reqwest::Client::new()
        .post(&url)
        .bearer_auth("wrong")
        .json(&json!({"model": "gpt", "stream": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    assert!(resp.json::<Value>().await.unwrap()["error"]["message"].is_string());

    let resp = reqwest::Client::new()
        .post(&url)
        .bearer_auth(TOKEN)
        .json(&json!({"model": "gpt", "stream": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert_eq!(
        text,
        "event: a\ndata: {\"n\":1}\n\nevent: b\ndata: {\"n\":2}\n\n"
    );
    assert_eq!(
        up.last_path.lock().unwrap().as_deref(),
        Some("/api/responses")
    );
    assert_eq!(up.last_auth.lock().unwrap().as_deref(), Some("Bearer sk-c"));

    // 未接管的 app：503。
    let resp = post_claude(server.port(), TOKEN, json!({"model": "m"})).await;
    assert_eq!(resp.status(), 503);
    server.shutdown().await;
}

/// 假 Chat 上游：记录请求，按 stream 字段回 SSE 或 JSON。
async fn chat_handler(
    State(up): State<Arc<Upstream>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response<Body> {
    up.hits.fetch_add(1, Ordering::SeqCst);
    let json: Value = serde_json::from_slice(&body).unwrap();
    *up.last_path.lock().unwrap() = Some(uri.to_string());
    *up.last_auth.lock().unwrap() = headers
        .get("authorization")
        .or_else(|| headers.get("x-api-key"))
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let stream = json["stream"].as_bool().unwrap_or(false);
    *up.last_body.lock().unwrap() = Some(json);
    if stream {
        let chunks: Vec<Result<&'static str, std::io::Error>> = vec![
            Ok(
                "data: {\"id\":\"c\",\"model\":\"ds\",\"choices\":[{\"delta\":{\"content\":\"你好\"}}]}\n\n",
            ),
            Ok("data: {\"id\":\"c\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n"),
            Ok("data: [DONE]\n\n"),
        ];
        return Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(Body::from_stream(futures::stream::iter(chunks)))
            .unwrap();
    }
    Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"id":"c","choices":[{"finish_reason":"stop","message":{"content":"ok"}}]})
                .to_string(),
        ))
        .unwrap()
}

#[tokio::test]
async fn codex_chat_candidate_is_converted_both_ways() {
    let up = Arc::new(Upstream::default());
    let app = Router::new()
        .fallback(any(chat_handler))
        .with_state(up.clone());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let mut chat = (*candidate("ds", format!("http://127.0.0.1:{port}/v1"), "ds", None)).clone();
    chat.format = UpstreamFormat::OpenaiChat;
    let (server, _state) = spawn_router(ClientApp::Codex, vec![Arc::new(chat)]).await;
    let url = format!("http://127.0.0.1:{}/codex/responses", server.port());
    let request = json!({
        "model": "ds",
        "instructions": "sys",
        "stream": true,
        "input": [{ "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hi" }] }]
    });

    let resp = reqwest::Client::new()
        .post(&url)
        .bearer_auth(TOKEN)
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert!(text.contains("event: response.output_text.delta"));
    assert!(text.contains("你好"));
    assert!(text.contains("event: response.completed"));
    assert_eq!(
        up.last_path.lock().unwrap().as_deref(),
        Some("/v1/chat/completions")
    );
    let sent = up.last_body.lock().unwrap().clone().unwrap();
    assert_eq!(sent["messages"][0]["content"], "sys");
    assert_eq!(sent["messages"][1]["content"], "hi");

    let mut plain = request.clone();
    plain["stream"] = json!(false);
    let resp: Value = reqwest::Client::new()
        .post(&url)
        .bearer_auth(TOKEN)
        .json(&plain)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resp["object"], "response");
    assert_eq!(resp["output"][0]["content"][0]["text"], "ok");
    server.shutdown().await;
}

#[tokio::test]
async fn claude_chat_candidate_is_converted_and_count_tokens_estimated() {
    let up = Arc::new(Upstream::default());
    let app = Router::new()
        .fallback(any(chat_handler))
        .with_state(up.clone());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let mut chat = (*candidate("glm", format!("http://127.0.0.1:{port}/v1"), "glm", None)).clone();
    chat.format = UpstreamFormat::OpenaiChat;
    chat.auth = UpstreamAuth::XApiKey;
    let (server, _state) = spawn_router(ClientApp::Claude, vec![Arc::new(chat)]).await;
    let base = format!("http://127.0.0.1:{}/claude/v1/messages", server.port());

    let resp = reqwest::Client::new()
        .post(format!("{base}?beta=true"))
        .bearer_auth(TOKEN)
        .header("anthropic-beta", "claude-code-20250219")
        .json(&json!({"model": "glm", "system": "sys", "stream": true,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert!(text.starts_with("event: message_start"));
    assert!(text.contains("你好"));
    assert!(text.contains("event: message_stop"));
    assert_eq!(
        up.last_path.lock().unwrap().as_deref(),
        Some("/v1/chat/completions")
    );
    // Chat 上游一律 Bearer，不带 x-api-key。
    assert_eq!(
        up.last_auth.lock().unwrap().as_deref(),
        Some("Bearer sk-glm")
    );
    assert_eq!(
        up.last_body.lock().unwrap().as_ref().unwrap()["messages"][0]["content"],
        "sys"
    );

    let hits = up.hits.load(Ordering::SeqCst);
    let count: Value = reqwest::Client::new()
        .post(format!("{base}/count_tokens"))
        .bearer_auth(TOKEN)
        .json(&json!({"model": "glm", "messages": [{"role": "user", "content": "hello world"}]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(count["input_tokens"].as_u64().unwrap() > 0);
    assert_eq!(
        up.hits.load(Ordering::SeqCst),
        hits,
        "count_tokens 本地估算，不打上游"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn gemini_fails_over_with_path_model_and_goog_key() {
    let (base_a, a) = spawn_upstream(503, false).await;
    let (base_b, b) = spawn_upstream(200, true).await;
    let mut primary = (*candidate("g1", base_a, "gemini-a", None)).clone();
    primary.auth = UpstreamAuth::XGoogApiKey;
    let mut backup = (*candidate("g2", base_b, "gemini-b", None)).clone();
    backup.auth = UpstreamAuth::XGoogApiKey;
    let (server, _state) =
        spawn_router(ClientApp::Gemini, vec![Arc::new(primary), Arc::new(backup)]).await;

    let resp = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/gemini/v1beta/models/gemini-a:streamGenerateContent?alt=sse",
            server.port()
        ))
        .header("x-goog-api-key", TOKEN)
        .json(&json!({"contents": [{"role": "user", "parts": [{"text": "hi"}]}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.text().await.unwrap().contains("event: a"));
    assert_eq!(a.hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        b.last_path.lock().unwrap().as_deref(),
        Some("/api/v1beta/models/gemini-b:streamGenerateContent?alt=sse")
    );
    // Gemini 上游收 x-goog-api-key（测试上游把它记在 last_auth 里）。
    assert_eq!(b.last_auth.lock().unwrap().as_deref(), Some("sk-g2"));

    let bad = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/gemini/v1beta/models/x:generateContent",
            server.port()
        ))
        .header("x-goog-api-key", "wrong")
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 401);
    let body: Value = bad.json().await.unwrap();
    assert_eq!(body["error"]["code"], 401);
    server.shutdown().await;
}

#[tokio::test]
async fn grok_namespace_passes_chat_through() {
    let (base, up) = spawn_upstream(200, false).await;
    let (server, _state) = spawn_router(
        ClientApp::Grok,
        vec![candidate("x", base, "grok-4.5", None)],
    )
    .await;
    let resp = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/grok/chat/completions",
            server.port()
        ))
        .bearer_auth(TOKEN)
        .json(&json!({"model": "grok-4.5", "messages": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        up.last_path.lock().unwrap().as_deref(),
        Some("/api/chat/completions")
    );
    assert_eq!(up.last_auth.lock().unwrap().as_deref(), Some("Bearer sk-x"));
    server.shutdown().await;
}

/// 按脚本依次应答的上游（用完后重复最后一条），记录每次请求体与关键头。
#[derive(Default)]
struct Scripted {
    replies: Mutex<std::collections::VecDeque<(u16, String)>>,
    last: Mutex<Option<(u16, String)>>,
    bodies: Mutex<Vec<Value>>,
    encodings: Mutex<Vec<Option<String>>>,
    paths: Mutex<Vec<String>>,
    headers: Mutex<Vec<HeaderMap>>,
}

async fn scripted_handler(
    State(up): State<Arc<Scripted>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response<Body> {
    up.paths.lock().unwrap().push(uri.path().to_owned());
    up.headers.lock().unwrap().push(headers.clone());
    up.bodies
        .lock()
        .unwrap()
        .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
    up.encodings.lock().unwrap().push(
        headers
            .get("accept-encoding")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
    );
    let reply = {
        let mut replies = up.replies.lock().unwrap();
        match replies.pop_front() {
            Some(reply) => {
                *up.last.lock().unwrap() = Some(reply.clone());
                reply
            }
            None => up
                .last
                .lock()
                .unwrap()
                .clone()
                .unwrap_or((200, "{}".into())),
        }
    };
    Response::builder()
        .status(reply.0)
        .header("content-type", "application/json")
        .body(Body::from(reply.1))
        .unwrap()
}

async fn spawn_scripted(replies: Vec<(u16, &str)>) -> (String, Arc<Scripted>) {
    let up = Arc::new(Scripted::default());
    up.replies
        .lock()
        .unwrap()
        .extend(replies.into_iter().map(|(s, b)| (s, b.to_owned())));
    let app = Router::new()
        .fallback(any(scripted_handler))
        .with_state(up.clone());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://127.0.0.1:{port}"), up)
}

const OK_MESSAGE: &str = r#"{"id":"m","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":10,"output_tokens":2}}"#;

#[tokio::test]
async fn signature_rejection_is_rectified_and_retried_on_same_provider() {
    let (base, up) = spawn_scripted(vec![
        (400, r#"{"type":"error","error":{"type":"invalid_request_error","message":"messages.1.content.0: Invalid `signature` in `thinking` block"}}"#),
        (200, OK_MESSAGE),
    ])
    .await;
    let (server, state) =
        spawn_router(ClientApp::Claude, vec![candidate("a", base, "m", None)]).await;
    let body = json!({
        "model": "m", "max_tokens": 100,
        "messages": [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": [{"type": "thinking", "thinking": "x", "signature": "foreign"}, {"type": "text", "text": "yo"}]},
            {"role": "user", "content": "again"}
        ]
    });
    let resp = post_claude(server.port(), TOKEN, body).await;
    assert_eq!(resp.status(), 200);
    let bodies = up.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2);
    assert_eq!(
        bodies[1]["messages"][1]["content"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "重试时已剥离 thinking"
    );
    // 整流不算供应商故障，且记进日志经过。
    let status = state.route_status();
    assert_eq!(status[0].candidates[0].breaker.consecutive_failures, 0);
    let record = &state.recent_requests(None, 1)[0];
    assert_eq!(record.attempts[0].outcome, "剥离历史 thinking 签名后重试");
    assert_eq!(record.status, Some(200));
    server.shutdown().await;
}

#[tokio::test]
async fn budget_rejection_is_fixed_once_then_error_passes_through() {
    let (base, up) = spawn_scripted(vec![
        (400, r#"{"error":{"message":"thinking.budget_tokens: Input should be greater than or equal to 1024"}}"#),
        (400, r#"{"error":{"message":"thinking.budget_tokens: still bad"}}"#),
    ])
    .await;
    let (server, _state) =
        spawn_router(ClientApp::Claude, vec![candidate("a", base, "m", None)]).await;
    let body = json!({"model": "m", "max_tokens": 600, "thinking": {"type": "enabled", "budget_tokens": 512}, "messages": [{"role": "user", "content": "hi"}]});
    let resp = post_claude(server.port(), TOKEN, body).await;
    assert_eq!(resp.status(), 400, "每类修正只做一次，第二次原样透传");
    let bodies = up.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[1]["thinking"]["budget_tokens"], 1024);
    assert!(bodies[1]["max_tokens"].as_u64().unwrap() > 1024);
    server.shutdown().await;
}

#[tokio::test]
async fn connect_failure_opens_breaker_immediately_and_backup_stands_in() {
    let (base_b, b) = spawn_scripted(vec![(200, OK_MESSAGE)]).await;
    let (server, state) = spawn_router(
        ClientApp::Claude,
        vec![
            candidate("dead", "http://127.0.0.1:9".into(), "m", None),
            candidate("b", base_b, "m", None),
        ],
    )
    .await;
    for _ in 0..3 {
        let resp = post_claude(
            server.port(),
            TOKEN,
            json!({"model": "m", "max_tokens": 5, "messages": []}),
        )
        .await;
        assert_eq!(resp.status(), 200);
    }
    let status = state.route_status();
    // 一次连接失败即熔断：后两次请求不再碰主站。
    assert_eq!(status[0].candidates[0].breaker.failed, 1);
    assert_eq!(status[0].candidates[0].breaker.state, BreakerState::Open);
    assert_eq!(status[0].sticky.as_ref().unwrap().provider_id, "b");
    assert_eq!(b.bodies.lock().unwrap().len(), 3);
    // 请求日志：服务方、经过、首字节与用量。
    let records = state.recent_requests(Some(ClientApp::Claude), 10);
    assert_eq!(records.len(), 3);
    let first = records.last().unwrap();
    assert_eq!(first.provider_id.as_deref(), Some("b"));
    assert_eq!(first.attempts[0].outcome, "连接失败或超时");
    assert!(first.first_byte_ms.is_some());
    // 用量在响应体读完后写入；reqwest 已把响应读完。
    assert_eq!(
        first.usage.map(|u| (u.input_tokens, u.output_tokens)),
        Some((10, 2))
    );
    assert_eq!(status[0].candidates[1].totals.requests, 3);
    // 手动恢复主站即结束临时接替。
    state.reset_breaker(ClientApp::Claude, "dead");
    assert!(state.route_status()[0].sticky.is_none());
    server.shutdown().await;
}

#[tokio::test]
async fn upstream_requests_never_ask_for_compression() {
    let (base, up) = spawn_scripted(vec![(200, OK_MESSAGE)]).await;
    let (server, _state) =
        spawn_router(ClientApp::Claude, vec![candidate("a", base, "m", None)]).await;
    reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/claude/v1/messages",
            server.port()
        ))
        .bearer_auth(TOKEN)
        .header("accept-encoding", "gzip, br, zstd")
        .json(&json!({"model": "m", "max_tokens": 5, "messages": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(up.encodings.lock().unwrap()[0].as_deref(), Some("identity"));
    server.shutdown().await;
}

/// 首条连接一接上就被对端关掉（陈旧 keep-alive 的典型症状）：换条连接重试成功，
/// 不记失败、不熔断。
#[tokio::test]
async fn transient_connection_drop_is_retried_without_tripping_breaker() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let up = Arc::new(Scripted::default());
    up.replies
        .lock()
        .unwrap()
        .push_back((200, OK_MESSAGE.to_owned()));
    let app = Router::new()
        .fallback(any(scripted_handler))
        .with_state(up.clone());
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        drop(socket);
        axum::serve(listener, app).await.unwrap();
    });
    let (server, state) = spawn_router(
        ClientApp::Claude,
        vec![candidate(
            "a",
            format!("http://127.0.0.1:{port}"),
            "m",
            None,
        )],
    )
    .await;
    let resp = post_claude(
        server.port(),
        TOKEN,
        json!({"model": "m", "max_tokens": 5, "messages": []}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let candidate_status = &state.route_status()[0].candidates[0];
    assert_eq!(candidate_status.breaker.failed, 0);
    assert_eq!(candidate_status.breaker.state, BreakerState::Closed);
    assert_eq!(up.bodies.lock().unwrap().len(), 1);
    server.shutdown().await;
}

#[tokio::test]
async fn codex_anthropic_joins_v1_base_once_and_drops_codex_fingerprint() {
    let (base, up) = spawn_scripted(vec![(200, OK_MESSAGE)]).await;
    let mut anthropic = (*candidate("k", format!("{base}/v1"), "k2", None)).clone();
    anthropic.format = UpstreamFormat::Anthropic;
    let (server, _state) = spawn_router(ClientApp::Codex, vec![Arc::new(anthropic)]).await;
    let resp = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/codex/responses",
            server.port()
        ))
        .bearer_auth(TOKEN)
        .header("originator", "codex_cli_rs")
        .header("session_id", "sess-1")
        .header("x-codex-turn-metadata", "t")
        .header("accept", "text/event-stream")
        .json(&json!({"model": "k2", "input": "hi"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        up.paths.lock().unwrap()[0],
        "/v1/messages",
        "填了 /v1 也不重复拼接"
    );
    let headers = up.headers.lock().unwrap()[0].clone();
    for name in ["originator", "session_id", "x-codex-turn-metadata"] {
        assert!(
            !headers.contains_key(name),
            "{name} 不应转给 Anthropic 端点"
        );
    }
    assert_eq!(headers["accept"], "application/json");
    server.shutdown().await;
}

#[tokio::test]
async fn output_limit_rejection_is_retried_with_upstream_limit() {
    let (base, up) = spawn_scripted(vec![
        (400, r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: 64000 > 32000, which is the maximum allowed number of output tokens for claude-opus-4-1"}}"#),
        (200, OK_MESSAGE),
    ])
    .await;
    let (server, state) =
        spawn_router(ClientApp::Claude, vec![candidate("a", base, "m", None)]).await;
    let body = json!({"model": "m", "max_tokens": 64000,
                      "thinking": {"type": "enabled", "budget_tokens": 40000},
                      "messages": [{"role": "user", "content": "hi"}]});
    let resp = post_claude(server.port(), TOKEN, body).await;
    assert_eq!(resp.status(), 200);
    let bodies = up.bodies.lock().unwrap().clone();
    assert_eq!(bodies[1]["max_tokens"], 32000);
    assert_eq!(bodies[1]["thinking"]["budget_tokens"], 23808);
    let record = &state.recent_requests(None, 1)[0];
    assert!(record.attempts[0].outcome.contains("输出上限"));
    server.shutdown().await;
}

#[tokio::test]
async fn compressed_reply_on_conversion_path_is_reported_clearly() {
    async fn gzip_anyway() -> Response<Body> {
        Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .header("content-encoding", "gzip")
            .body(Body::from(vec![0x1f, 0x8b, 0x08, 0x00]))
            .unwrap()
    }
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, Router::new().fallback(any(gzip_anyway)))
            .await
            .unwrap()
    });
    let mut chat = (*candidate("c", format!("http://127.0.0.1:{port}/v1"), "m", None)).clone();
    chat.format = UpstreamFormat::OpenaiChat;
    let (server, _state) = spawn_router(ClientApp::Claude, vec![Arc::new(chat)]).await;
    let resp = post_claude(
        server.port(),
        TOKEN,
        json!({"model": "m", "max_tokens": 5, "messages": [{"role": "user", "content": "hi"}]}),
    )
    .await;
    assert_eq!(resp.status(), 502);
    let text = resp.text().await.unwrap();
    assert!(text.contains("gzip") && text.contains("压缩"), "{text}");
    server.shutdown().await;
}

/// 调参生效：首字节时限调到 1s、连续失败 1 次即熔断——挂起的主站 1s 后让位给备用，
/// 且随即熔断（下个请求直接走备用）；供应商请求头随请求发往该供应商。
#[tokio::test]
async fn tuning_and_provider_headers_are_applied() {
    let hang = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let hang_port = hang.local_addr().unwrap().port();
    // 接受连接但永不应答：模拟上游挂起。
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = hang.accept().await {
            held.push(socket);
        }
    });
    let (base_b, backup) = spawn_scripted(vec![(200, OK_MESSAGE)]).await;
    let mut b = (*candidate("b", base_b, "m", None)).clone();
    b.headers = vec![("X-Route-Tag".into(), "backup".into())];
    let state = ProxyState::new(TOKEN);
    state.set_route(
        ClientApp::Claude,
        Some(AppRoute {
            candidates: vec![
                candidate("a", format!("http://127.0.0.1:{hang_port}"), "m", None),
                Arc::new(b),
            ],
            tuning: mcpane_desktop_proxy::RouteTuning {
                first_byte: std::time::Duration::from_secs(1),
                failure_threshold: 1,
                ..Default::default()
            },
            ..AppRoute::default()
        }),
    );
    let server = ProxyServer::start(0, state.clone()).await.unwrap();
    let started = std::time::Instant::now();
    let resp = post_claude(
        server.port(),
        TOKEN,
        json!({"model": "m", "max_tokens": 5, "stream": true, "messages": []}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "按调参的 1s 首字节时限让位"
    );
    let status = state.route_status();
    assert_eq!(status[0].candidates[0].breaker.state, BreakerState::Open);
    assert_eq!(backup.headers.lock().unwrap()[0]["x-route-tag"], "backup");
    server.shutdown().await;
}

// ---------- 额度优先策略（ADR-022 三期） ----------

fn exhausted_for(
    ids: &[&str],
    secs: u64,
) -> std::collections::BTreeMap<String, std::time::Instant> {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    ids.iter().map(|id| ((*id).to_owned(), until)).collect()
}

async fn spawn_strategy_router(
    strategy: mcpane_provider::RoutingStrategy,
    ids: &[&str],
) -> (ProxyServer, ProxyState, Vec<Arc<Upstream>>) {
    let mut candidates = Vec::new();
    let mut upstreams = Vec::new();
    for id in ids {
        let (base, up) = spawn_upstream(200, false).await;
        candidates.push(candidate(id, base, "m", None));
        upstreams.push(up);
    }
    let state = ProxyState::new(TOKEN);
    state.set_route(
        ClientApp::Claude,
        Some(AppRoute {
            candidates,
            strategy,
            ..AppRoute::default()
        }),
    );
    let server = ProxyServer::start(0, state.clone()).await.unwrap();
    (server, state, upstreams)
}

async fn ask(server: &ProxyServer) {
    let resp = post_claude(
        server.port(),
        TOKEN,
        json!({"model": "m", "max_tokens": 5, "messages": []}),
    )
    .await;
    assert_eq!(resp.status(), 200);
}

/// 额度用尽的主站被后置：请求直接由备用服务，主站一次都不碰。
#[tokio::test]
async fn quota_aware_moves_exhausted_primary_behind_backup() {
    let (server, state, ups) =
        spawn_strategy_router(mcpane_provider::RoutingStrategy::QuotaAware, &["a", "b"]).await;
    state.set_quota_exhausted(exhausted_for(&["a"], 60));
    ask(&server).await;
    assert_eq!(
        ups[0].hits.load(Ordering::SeqCst),
        0,
        "用尽的主站不应被请求"
    );
    assert_eq!(ups[1].hits.load(Ordering::SeqCst), 1);
    let status = state.route_status();
    let candidates = &status[0].candidates;
    assert!(candidates[0].quota_exhausted_secs.is_some());
    assert!(candidates[1].quota_exhausted_secs.is_none());
}

/// 默认的纯故障转移策略不看额度信号（行为零变化）。
#[tokio::test]
async fn failover_strategy_ignores_quota_signals() {
    let (server, state, ups) =
        spawn_strategy_router(mcpane_provider::RoutingStrategy::Failover, &["a", "b"]).await;
    state.set_quota_exhausted(exhausted_for(&["a"], 60));
    ask(&server).await;
    assert_eq!(ups[0].hits.load(Ordering::SeqCst), 1);
    assert_eq!(ups[1].hits.load(Ordering::SeqCst), 0);
    assert!(
        state.route_status()[0].candidates[0]
            .quota_exhausted_secs
            .is_none()
    );
}

/// 全部用尽时顺序不变：仍由主站先试，不因额度信号把用户锁死。
#[tokio::test]
async fn quota_aware_keeps_order_when_everyone_is_exhausted() {
    let (server, state, ups) =
        spawn_strategy_router(mcpane_provider::RoutingStrategy::QuotaAware, &["a", "b"]).await;
    state.set_quota_exhausted(exhausted_for(&["a", "b"], 60));
    ask(&server).await;
    assert_eq!(ups[0].hits.load(Ordering::SeqCst), 1);
    assert_eq!(ups[1].hits.load(Ordering::SeqCst), 0);
}

/// 窗口重置（信号过期）后主站回到首位；信号表被整体替换后同样恢复。
#[tokio::test]
async fn quota_aware_restores_primary_after_reset_or_signal_cleared() {
    let (server, state, ups) =
        spawn_strategy_router(mcpane_provider::RoutingStrategy::QuotaAware, &["a", "b"]).await;
    state.set_quota_exhausted(exhausted_for(&["a"], 0));
    ask(&server).await;
    assert_eq!(
        ups[0].hits.load(Ordering::SeqCst),
        1,
        "已过期的信号不应再后置"
    );

    state.set_quota_exhausted(exhausted_for(&["a"], 60));
    ask(&server).await;
    assert_eq!(ups[0].hits.load(Ordering::SeqCst), 1);
    assert_eq!(ups[1].hits.load(Ordering::SeqCst), 1);

    state.set_quota_exhausted(std::collections::BTreeMap::new());
    ask(&server).await;
    assert_eq!(
        ups[0].hits.load(Ordering::SeqCst),
        2,
        "信号清空后主站回到首位"
    );
}

// ---------- 上下文超限错误归一（ADR-022 §10） ----------

async fn post_codex(port: u16, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/codex/responses"))
        .bearer_auth(TOKEN)
        .json(&body)
        .send()
        .await
        .unwrap()
}

/// 中转站用自己的话说「装不下」：Claude Code 收到的必须是它认得的「prompt is too long」，
/// 才会自动压缩；且不会因此转移到备用（换一家也装不下，原样回给客户端处理）。
#[tokio::test]
async fn claude_overflow_is_said_the_way_claude_code_understands() {
    let (base_a, _a) = spawn_scripted(vec![(
        400,
        r#"{"error":{"message":"Range of input length should be [1, 98304]","type":"invalid_request_error"}}"#,
    )])
    .await;
    let (base_b, b) = spawn_scripted(vec![(200, OK_MESSAGE)]).await;
    let (server, _state) = spawn_router(
        ClientApp::Claude,
        vec![
            candidate("a", base_a, "m", None),
            candidate("b", base_b, "m", None),
        ],
    )
    .await;
    let resp = post_claude(
        server.port(),
        TOKEN,
        json!({"model": "m", "max_tokens": 5, "messages": []}),
    )
    .await;
    assert_eq!(resp.status(), 400);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["error"]["type"], "invalid_request_error");
    assert_eq!(
        json["error"]["message"],
        "prompt is too long: Range of input length should be [1, 98304]"
    );
    assert_eq!(b.paths.lock().unwrap().len(), 0, "超限不是换一家能解决的事");
    server.shutdown().await;
}

/// Codex 靠错误码 `context_length_exceeded` 压缩；413 一并归成 400。
#[tokio::test]
async fn codex_overflow_carries_the_openai_error_code() {
    let (base, _up) = spawn_scripted(vec![(
        413,
        r#"{"message":"输入内容过长，超出模型上下文限制"}"#,
    )])
    .await;
    let (server, _state) =
        spawn_router(ClientApp::Codex, vec![candidate("c", base, "gpt", None)]).await;
    let resp = post_codex(
        server.port(),
        json!({"model": "gpt", "stream": false, "input": []}),
    )
    .await;
    assert_eq!(resp.status(), 400);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["error"]["code"], "context_length_exceeded");
    assert_eq!(json["error"]["type"], "invalid_request_error");
    server.shutdown().await;
}

/// 限速、回复额度、鉴权类错误一概不动：误判成超限会让客户端反复压缩。
#[tokio::test]
async fn non_overflow_errors_pass_through_untouched() {
    for body in [
        r#"{"error":{"message":"Rate limit reached: 30000 tokens per minute. Please try again later"}}"#,
        r#"{"error":{"message":"max_tokens is too large: 99999 exceeds the maximum context length"}}"#,
        r#"{"error":{"message":"invalid model"}}"#,
    ] {
        let (base, _up) = spawn_scripted(vec![(400, body)]).await;
        let (server, _state) =
            spawn_router(ClientApp::Claude, vec![candidate("a", base, "m", None)]).await;
        let resp = post_claude(
            server.port(),
            TOKEN,
            json!({"model": "m", "max_tokens": 5, "messages": []}),
        )
        .await;
        assert_eq!(resp.status(), 400);
        assert_eq!(resp.text().await.unwrap(), body, "{body}");
        server.shutdown().await;
    }
}

// ---------- Codex→Chat 工具回合配对（ADR-022 §10） ----------

/// 像 Kimi 那样严格的 Chat 上游：tool 消息的 id 必须对得上紧邻前一条 assistant 的调用，
/// 每个调用都得有应答，否则 400。
async fn strict_chat_handler(
    State(up): State<Arc<Upstream>>,
    body: axum::body::Bytes,
) -> Response<Body> {
    up.hits.fetch_add(1, Ordering::SeqCst);
    let json: Value = serde_json::from_slice(&body).unwrap();
    let mut open: Vec<String> = Vec::new();
    let mut fault = None;
    for message in json["messages"].as_array().unwrap() {
        match message["role"].as_str().unwrap_or("") {
            "assistant" => {
                if !open.is_empty() {
                    fault = Some(format!("tool_calls without results: {open:?}"));
                }
                open = message["tool_calls"]
                    .as_array()
                    .map(|calls| {
                        calls
                            .iter()
                            .map(|c| c["id"].as_str().unwrap().to_owned())
                            .collect()
                    })
                    .unwrap_or_default();
            }
            "tool" => {
                let id = message["tool_call_id"].as_str().unwrap_or("");
                if let Some(pos) = open.iter().position(|o| o == id) {
                    open.remove(pos);
                } else {
                    fault = Some(format!("tool_call_id is not found: {id}"));
                }
            }
            _ => {
                if !open.is_empty() {
                    fault = Some(format!("tool_calls without results: {open:?}"));
                }
            }
        }
    }
    if !open.is_empty() {
        fault = Some(format!("tool_calls without results: {open:?}"));
    }
    *up.last_body.lock().unwrap() = Some(json);
    match fault {
        Some(message) => Response::builder()
            .status(400)
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"error": {"message": message}}).to_string(),
            ))
            .unwrap(),
        None => Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"id":"c","choices":[{"finish_reason":"stop","message":{"content":"ok"}}]})
                    .to_string(),
            ))
            .unwrap(),
    }
}

/// 压缩 / 中断留下孤立输出和没应答的调用：改前严格上游直接 400 且每次重试都一样，
/// 改后修补后放行。
#[tokio::test]
async fn strict_chat_upstream_accepts_a_mended_codex_history() {
    let up = Arc::new(Upstream::default());
    let app = Router::new()
        .fallback(any(strict_chat_handler))
        .with_state(up.clone());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let mut chat =
        (*candidate("kimi", format!("http://127.0.0.1:{port}/v1"), "kimi", None)).clone();
    chat.format = UpstreamFormat::OpenaiChat;
    let (server, _state) = spawn_router(ClientApp::Codex, vec![Arc::new(chat)]).await;
    let request = json!({
        "model": "kimi",
        "stream": false,
        "input": [
            { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "start" }] },
            { "type": "function_call_output", "call_id": "compacted-away", "output": "stale" },
            { "type": "function_call", "call_id": "c1", "name": "run", "arguments": "{}" },
            { "type": "function_call_output", "call_id": "c1", "output": "ok" },
            { "type": "function_call", "call_id": "c2", "name": "run", "arguments": "{}" },
        ]
    });
    let resp = post_codex(server.port(), request).await;
    assert_eq!(resp.status(), 200, "{:?}", resp.text().await);
    assert_eq!(up.hits.load(Ordering::SeqCst), 1, "一次通过，不靠重试");
    server.shutdown().await;
}

// ---------- 流结果记账与流首事件转移（ADR-022 §10） ----------

/// 脚本化 SSE 上游：按脚本吐块（块间可停顿），可在末尾故意断开连接。
struct SseScript {
    chunks: Vec<(std::time::Duration, &'static str)>,
    abort_after: bool,
    hits: AtomicUsize,
}

async fn sse_script_handler(State(script): State<Arc<SseScript>>) -> Response<Body> {
    script.hits.fetch_add(1, Ordering::SeqCst);
    let chunks = script.chunks.clone();
    let abort = script.abort_after;
    let stream = futures::stream::unfold(0usize, move |index| {
        let chunks = chunks.clone();
        async move {
            if let Some((delay, text)) = chunks.get(index) {
                tokio::time::sleep(*delay).await;
                return Some((
                    Ok::<_, std::io::Error>(axum::body::Bytes::from_static(text.as_bytes())),
                    index + 1,
                ));
            }
            if abort && index == chunks.len() {
                return Some((Err(std::io::Error::other("upstream reset")), index + 1));
            }
            None
        }
    });
    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}

async fn spawn_sse(
    chunks: Vec<(u64, &'static str)>,
    abort_after: bool,
) -> (String, Arc<SseScript>) {
    let script = Arc::new(SseScript {
        chunks: chunks
            .into_iter()
            .map(|(ms, text)| (std::time::Duration::from_millis(ms), text))
            .collect(),
        abort_after,
        hits: AtomicUsize::new(0),
    });
    let app = Router::new()
        .fallback(any(sse_script_handler))
        .with_state(script.clone());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://127.0.0.1:{port}"), script)
}

const SSE_START: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\n";
const SSE_DELTA: &str = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n";
const SSE_STOP: &str = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
const SSE_OVERLOADED: &str = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
const SSE_BAD_REQUEST: &str = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"bad field\"}}\n\n";
const STREAM_BODY: fn() -> Value =
    || json!({"model": "m", "max_tokens": 5, "stream": true, "messages": []});

fn healthy_stream() -> Vec<(u64, &'static str)> {
    vec![(0, SSE_START), (0, SSE_DELTA), (0, SSE_STOP)]
}

fn breaker_of(state: &ProxyState, index: usize) -> mcpane_desktop_proxy::BreakerSnapshot {
    state.route_status()[0].candidates[index].breaker.clone()
}

async fn spawn_stream_router(
    ups: &[&str],
    tuning: mcpane_desktop_proxy::RouteTuning,
) -> (ProxyServer, ProxyState) {
    let state = ProxyState::new(TOKEN);
    state.set_route(
        ClientApp::Claude,
        Some(AppRoute {
            candidates: ups
                .iter()
                .enumerate()
                .map(|(i, base)| candidate(&format!("p{i}"), (*base).to_owned(), "m", None))
                .collect(),
            tuning,
            ..AppRoute::default()
        }),
    );
    let server = ProxyServer::start(0, state.clone()).await.unwrap();
    (server, state)
}

/// 中转站先回 200、随即在流里报过载：客户端还没收到任何字节，换下一家对它无感，
/// 失败记在主站头上。
#[tokio::test]
async fn first_event_error_fails_over_before_the_client_sees_anything() {
    let (a_url, a) = spawn_sse(vec![(0, SSE_OVERLOADED)], false).await;
    let (b_url, b) = spawn_sse(healthy_stream(), false).await;
    let (server, state) = spawn_stream_router(
        &[&a_url, &b_url],
        mcpane_desktop_proxy::RouteTuning::default(),
    )
    .await;
    let resp = post_claude(server.port(), TOKEN, STREAM_BODY()).await;
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert!(text.contains("\"text\":\"hi\""), "{text}");
    assert!(
        !text.contains("Overloaded"),
        "客户端不该看到主站的错误：{text}"
    );
    assert_eq!(a.hits.load(Ordering::SeqCst), 1);
    assert_eq!(b.hits.load(Ordering::SeqCst), 1);
    assert_eq!(breaker_of(&state, 0).failed, 1);
    assert_eq!(breaker_of(&state, 1).served, 1);
    server.shutdown().await;
}

/// 请求本身的错误（参数无效）换哪家都一样：不转移，也不算主站的失败。
#[tokio::test]
async fn request_fault_in_the_first_event_is_not_failed_over() {
    let (a_url, _a) = spawn_sse(vec![(0, SSE_BAD_REQUEST)], false).await;
    let (b_url, b) = spawn_sse(healthy_stream(), false).await;
    let (server, state) = spawn_stream_router(
        &[&a_url, &b_url],
        mcpane_desktop_proxy::RouteTuning::default(),
    )
    .await;
    let resp = post_claude(server.port(), TOKEN, STREAM_BODY()).await;
    let text = resp.text().await.unwrap();
    assert!(text.contains("bad field"), "{text}");
    assert_eq!(b.hits.load(Ordering::SeqCst), 0);
    let breaker = breaker_of(&state, 0);
    assert_eq!((breaker.failed, breaker.served), (0, 0));
    server.shutdown().await;
}

/// 流到一半报错：已经写给客户端的字节收不回，不转移；但这是主站的失败，
/// 记进熔断，连续出现就熔断，之后的请求直接走备用。
#[tokio::test]
async fn mid_stream_error_trips_the_breaker_for_the_next_request() {
    let (a_url, a) = spawn_sse(
        vec![(0, SSE_START), (0, SSE_DELTA), (0, SSE_OVERLOADED)],
        false,
    )
    .await;
    let (b_url, b) = spawn_sse(healthy_stream(), false).await;
    let tuning = mcpane_desktop_proxy::RouteTuning {
        failure_threshold: 1,
        ..Default::default()
    };
    let (server, state) = spawn_stream_router(&[&a_url, &b_url], tuning).await;
    let first = post_claude(server.port(), TOKEN, STREAM_BODY()).await;
    let text = first.text().await.unwrap();
    assert!(
        text.contains("Overloaded"),
        "中途的错误照常转给客户端：{text}"
    );
    assert_eq!(
        breaker_of(&state, 0).state,
        mcpane_desktop_proxy::BreakerState::Open
    );
    let second = post_claude(server.port(), TOKEN, STREAM_BODY()).await;
    assert!(second.text().await.unwrap().contains("\"text\":\"hi\""));
    assert_eq!(a.hits.load(Ordering::SeqCst), 1, "熔断后不再打主站");
    assert_eq!(b.hits.load(Ordering::SeqCst), 1);
    server.shutdown().await;
}

/// 读流中途连接被掐断同样算主站的失败。
#[tokio::test]
async fn broken_connection_mid_stream_counts_as_a_failure() {
    let (a_url, _a) = spawn_sse(vec![(0, SSE_START), (0, SSE_DELTA)], true).await;
    let (server, state) =
        spawn_stream_router(&[&a_url], mcpane_desktop_proxy::RouteTuning::default()).await;
    let resp = post_claude(server.port(), TOKEN, STREAM_BODY()).await;
    let _ = resp.text().await; // 连接中断，读取报错属预期
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(breaker_of(&state, 0).failed, 1);
    server.shutdown().await;
}

/// 客户端自己走掉（按 Esc）不是供应商的错：既不记失败，也不记成功。
#[tokio::test]
async fn client_leaving_early_is_not_the_providers_fault() {
    let (a_url, _a) = spawn_sse(
        vec![(0, SSE_START), (3000, SSE_DELTA), (0, SSE_STOP)],
        false,
    )
    .await;
    let (server, state) =
        spawn_stream_router(&[&a_url], mcpane_desktop_proxy::RouteTuning::default()).await;
    let mut resp = post_claude(server.port(), TOKEN, STREAM_BODY()).await;
    assert!(resp.chunk().await.unwrap().is_some());
    drop(resp);
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let breaker = breaker_of(&state, 0);
    assert_eq!(
        (breaker.failed, breaker.served, breaker.consecutive_failures),
        (0, 0, 0)
    );
    server.shutdown().await;
}

/// 流式响应读完才记成功，且只记一次（不再在响应头处重复记）。
#[tokio::test]
async fn completed_stream_is_booked_once_when_it_ends() {
    let (a_url, _a) = spawn_sse(healthy_stream(), false).await;
    let (server, state) =
        spawn_stream_router(&[&a_url], mcpane_desktop_proxy::RouteTuning::default()).await;
    let resp = post_claude(server.port(), TOKEN, STREAM_BODY()).await;
    let _ = resp.text().await.unwrap();
    assert_eq!(breaker_of(&state, 0).served, 1);
    server.shutdown().await;
}

/// 响应头到了、首事件却迟迟不来：窗口一到就放行，不白等，也不误判成失败。
#[tokio::test]
async fn slow_first_event_is_let_through_after_the_window() {
    let (a_url, a) = spawn_sse(
        vec![(2600, SSE_START), (0, SSE_DELTA), (0, SSE_STOP)],
        false,
    )
    .await;
    let (b_url, b) = spawn_sse(healthy_stream(), false).await;
    let (server, state) = spawn_stream_router(
        &[&a_url, &b_url],
        mcpane_desktop_proxy::RouteTuning::default(),
    )
    .await;
    let resp = post_claude(server.port(), TOKEN, STREAM_BODY()).await;
    assert!(resp.text().await.unwrap().contains("\"text\":\"hi\""));
    assert_eq!(a.hits.load(Ordering::SeqCst), 1);
    assert_eq!(b.hits.load(Ordering::SeqCst), 0, "慢不是失败，不转移");
    assert_eq!(breaker_of(&state, 0).failed, 0);
    server.shutdown().await;
}

/// 只有一个供应商时不窥探（没有后备可转移，白等只增加延迟）。
#[tokio::test]
async fn single_candidate_streams_are_not_delayed_by_peeking() {
    let (a_url, _a) = spawn_sse(
        vec![(1500, SSE_START), (0, SSE_DELTA), (0, SSE_STOP)],
        false,
    )
    .await;
    let (server, _state) =
        spawn_stream_router(&[&a_url], mcpane_desktop_proxy::RouteTuning::default()).await;
    let started = std::time::Instant::now();
    let mut resp = post_claude(server.port(), TOKEN, STREAM_BODY()).await;
    let first_chunk_at = started.elapsed();
    assert!(resp.chunk().await.unwrap().is_some());
    assert!(
        first_chunk_at < std::time::Duration::from_millis(1000),
        "响应头应立即返回，实际 {first_chunk_at:?}"
    );
    server.shutdown().await;
}

/// 转换路径同样生效：Codex 经 Chat 上游，首个 chunk 就是 error → 转下一家，
/// 客户端拿到的是备用供应商转换后的 Responses 流。
#[tokio::test]
async fn first_event_error_fails_over_on_a_converted_chat_stream() {
    const CHAT_ERROR: &str =
        "data: {\"error\":{\"message\":\"upstream overloaded\",\"type\":\"server_error\"}}\n\n";
    const CHAT_HELLO: &str = "data: {\"id\":\"c\",\"model\":\"ds\",\"choices\":[{\"delta\":{\"content\":\"你好\"}}]}\n\n";
    const CHAT_STOP: &str = "data: {\"id\":\"c\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    let (a_url, a) = spawn_sse(vec![(0, CHAT_ERROR)], false).await;
    let (b_url, b) = spawn_sse(vec![(0, CHAT_HELLO), (0, CHAT_STOP)], false).await;
    let chat = |id: &str, base: String| {
        let mut upstream = (*candidate(id, format!("{base}/v1"), "ds", None)).clone();
        upstream.format = UpstreamFormat::OpenaiChat;
        Arc::new(upstream)
    };
    let (server, state) =
        spawn_router(ClientApp::Codex, vec![chat("a", a_url), chat("b", b_url)]).await;
    let resp = post_codex(
        server.port(),
        json!({"model": "ds", "stream": true, "input": [
            { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hi" }] }
        ]}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert!(text.contains("你好"), "{text}");
    assert!(!text.contains("upstream overloaded"), "{text}");
    assert_eq!(a.hits.load(Ordering::SeqCst), 1);
    assert_eq!(b.hits.load(Ordering::SeqCst), 1);
    assert_eq!(breaker_of(&state, 0).failed, 1);
    server.shutdown().await;
}
