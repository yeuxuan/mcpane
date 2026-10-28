//! TASK-104（有状态 Session）与 TASK-105（响应形式兼容）集成测试。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use mcpane_hub::app::{HubOptions, build_router};
use mcpane_hub::auth::{ClientIdentity, TokenRegistry};
use serde_json::{Value, json};

const CLAUDE_TOKEN: &str = "session-claude-token-000000000000000000000000000000000001";
const CODEX_TOKEN: &str = "session-codex-token-0000000000000000000000000000000000002";

struct SessionHub {
    base_url: String,
    client: reqwest::Client,
}

async fn start_stateful_hub(keep_alive: Option<Duration>) -> SessionHub {
    let registry = TokenRegistry::new();
    registry.register(
        CLAUDE_TOKEN,
        ClientIdentity::from_raw("client_claude_test".into(), "claude"),
    );
    registry.register(
        CODEX_TOKEN,
        ClientIdentity::from_raw("client_codex_test".into(), "codex"),
    );
    let state = Arc::new(mcpane_hub::state::AggregationState::new(None));
    let router = build_router(
        Arc::new(registry),
        state,
        HubOptions {
            stateful: true,
            session_keep_alive: keep_alive,
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    SessionHub {
        base_url: format!("http://{addr}/mcp"),
        client: reqwest::Client::new(),
    }
}

impl SessionHub {
    fn request(
        &self,
        method: reqwest::Method,
        token: &str,
        session: Option<&str>,
    ) -> reqwest::RequestBuilder {
        let mut builder = self
            .client
            .request(method, &self.base_url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-11-25");
        if let Some(session) = session {
            builder = builder.header("Mcp-Session-Id", session);
        }
        builder
    }

    /// initialize 并返回服务器分配的 Session ID。
    async fn initialize(&self, token: &str) -> String {
        let response = self
            .request(reqwest::Method::POST, token, None)
            .header("Content-Type", "application/json")
            .json(&json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "session-test", "version": "0" }
                }
            }))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success(), "{}", response.status());
        let session_id = response
            .headers()
            .get("mcp-session-id")
            .expect("有状态模式 initialize 必须返回 MCP-Session-Id")
            .to_str()
            .unwrap()
            .to_owned();

        let response = self
            .request(reqwest::Method::POST, token, Some(&session_id))
            .header("Content-Type", "application/json")
            .json(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 202);
        session_id
    }

    /// 在 Session 内调用 Tool，返回 (HTTP 状态, 解析后的 JSON-RPC 响应)。
    async fn call_tool(
        &self,
        token: &str,
        session: &str,
        id: i64,
        name: &str,
        arguments: Value,
    ) -> (u16, Option<Value>) {
        let response = self
            .request(reqwest::Method::POST, token, Some(session))
            .header("Content-Type", "application/json")
            .json(&json!({
                "jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": { "name": name, "arguments": arguments }
            }))
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let text = response.text().await.unwrap();
        if !(200..300).contains(&status) {
            return (status, None);
        }
        let value = if content_type.starts_with("text/event-stream") {
            text.lines()
                .filter_map(|l| l.strip_prefix("data: "))
                .filter_map(|d| serde_json::from_str::<Value>(d).ok())
                .find(|v| v.get("id").is_some())
                .unwrap_or_else(|| panic!("SSE 中没有响应消息: {text}"))
        } else {
            serde_json::from_str(&text).unwrap()
        };
        (status, Some(value))
    }
}

// ---------- TASK-104 ----------

#[tokio::test]
async fn each_initialize_creates_distinct_session_even_with_same_token() {
    let hub = start_stateful_hub(None).await;
    let s1 = hub.initialize(CLAUDE_TOKEN).await;
    let s2 = hub.initialize(CLAUDE_TOKEN).await;
    assert_ne!(s1, s2, "同一 Token 的两次 initialize 必须得到不同 Session");

    // 两个 Session 都可独立工作。
    for (i, session) in [&s1, &s2].into_iter().enumerate() {
        let (status, value) = hub
            .call_tool(
                CLAUDE_TOKEN,
                session,
                10 + i as i64,
                "mcpane__echo",
                json!({ "message": format!("s{i}") }),
            )
            .await;
        assert_eq!(status, 200);
        assert_eq!(
            value.unwrap()["result"]["structuredContent"]["message"],
            format!("s{i}")
        );
    }
}

#[tokio::test]
async fn connection_info_reports_session_id() {
    let hub = start_stateful_hub(None).await;
    let session = hub.initialize(CODEX_TOKEN).await;
    let (status, value) = hub
        .call_tool(
            CODEX_TOKEN,
            &session,
            2,
            "mcpane__connection_info",
            json!({}),
        )
        .await;
    assert_eq!(status, 200);
    let info = value.unwrap();
    assert_eq!(
        info["result"]["structuredContent"]["sessionId"],
        Value::String(session.clone())
    );
    assert_eq!(
        info["result"]["structuredContent"]["clientConnectionId"],
        "client_codex_test"
    );
}

#[tokio::test]
async fn invalid_session_returns_404_and_only_affects_that_session() {
    let hub = start_stateful_hub(None).await;
    let claude_session = hub.initialize(CLAUDE_TOKEN).await;
    let codex_session = hub.initialize(CODEX_TOKEN).await;

    // 无效 Session → 404。
    let (status, _) = hub
        .call_tool(
            CLAUDE_TOKEN,
            "nonexistent-session-id",
            3,
            "mcpane__echo",
            json!({ "message": "x" }),
        )
        .await;
    assert_eq!(status, 404, "无效 Session 必须返回 404");

    // DELETE 终止 Claude Session。
    let response = hub
        .request(reqwest::Method::DELETE, CLAUDE_TOKEN, Some(&claude_session))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "DELETE session: {}",
        response.status()
    );

    // 被终止的 Session → 404；Codex Session 完全不受影响。
    let (status, _) = hub
        .call_tool(
            CLAUDE_TOKEN,
            &claude_session,
            4,
            "mcpane__echo",
            json!({ "message": "x" }),
        )
        .await;
    assert_eq!(status, 404, "已删除 Session 必须 404");

    let (status, value) = hub
        .call_tool(
            CODEX_TOKEN,
            &codex_session,
            5,
            "mcpane__echo",
            json!({ "message": "codex-still-alive" }),
        )
        .await;
    assert_eq!(status, 200, "Claude Session 失效不得影响 Codex");
    assert_eq!(
        value.unwrap()["result"]["structuredContent"]["message"],
        "codex-still-alive"
    );

    // 客户端可在 404 后重新 initialize 恢复。
    let new_session = hub.initialize(CLAUDE_TOKEN).await;
    assert_ne!(new_session, claude_session);
    let (status, _) = hub
        .call_tool(
            CLAUDE_TOKEN,
            &new_session,
            6,
            "mcpane__echo",
            json!({ "message": "recovered" }),
        )
        .await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn get_and_delete_also_validate_session() {
    let hub = start_stateful_hub(None).await;

    // GET：无效 Session → 404。
    let response = hub
        .request(reqwest::Method::GET, CLAUDE_TOKEN, Some("bogus-session"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 404);

    // DELETE：未知 Session 幂等接受（rmcp 返回 202；规范未强制 404，
    // 404 强制语义只针对携带无效 Session 的请求路由，上面 GET/POST 已覆盖）。
    let response = hub
        .request(reqwest::Method::DELETE, CLAUDE_TOKEN, Some("bogus-session"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 202);

    // DELETE：缺失 Session 头 → 400（规范 SHOULD）。
    let response = hub
        .request(reqwest::Method::DELETE, CLAUDE_TOKEN, None)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 400);
}

/// Session 空闲超时可配置：keep_alive=1s，2.5s 后 Session 失效（404）。
#[tokio::test]
async fn session_idle_timeout_is_configurable() {
    let hub = start_stateful_hub(Some(Duration::from_secs(1))).await;
    let session = hub.initialize(CLAUDE_TOKEN).await;

    let (status, _) = hub
        .call_tool(
            CLAUDE_TOKEN,
            &session,
            7,
            "mcpane__echo",
            json!({ "message": "warm" }),
        )
        .await;
    assert_eq!(status, 200);

    tokio::time::sleep(Duration::from_millis(2500)).await;

    let (status, _) = hub
        .call_tool(
            CLAUDE_TOKEN,
            &session,
            8,
            "mcpane__echo",
            json!({ "message": "late" }),
        )
        .await;
    assert_eq!(status, 404, "空闲超时后的 Session 必须 404");

    // 超时后重新 initialize 可恢复。
    let new_session = hub.initialize(CLAUDE_TOKEN).await;
    let (status, _) = hub
        .call_tool(
            CLAUDE_TOKEN,
            &new_session,
            9,
            "mcpane__echo",
            json!({ "message": "recovered" }),
        )
        .await;
    assert_eq!(status, 200);
}

// ---------- TASK-105 ----------

#[tokio::test]
async fn notification_returns_202_without_body() {
    let hub = start_stateful_hub(None).await;
    let session = hub.initialize(CLAUDE_TOKEN).await;
    let response = hub
        .request(reqwest::Method::POST, CLAUDE_TOKEN, Some(&session))
        .header("Content-Type", "application/json")
        .json(
            &json!({ "jsonrpc": "2.0", "method": "notifications/cancelled",
            "params": { "requestId": 999 } }),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 202);
    assert!(response.text().await.unwrap().is_empty());
}

#[tokio::test]
async fn missing_accept_header_yields_explicit_client_error() {
    let hub = start_stateful_hub(None).await;
    // 不带 Accept: text/event-stream（规范要求 POST 同时接受两种类型）。
    let response = hub
        .client
        .post(&hub.base_url)
        .header("Authorization", format!("Bearer {CLAUDE_TOKEN}"))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": { "name": "t", "version": "0" } }
        }))
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    assert!(
        (400..500).contains(&status),
        "Accept 不完整必须返回 4xx 明确错误，实际 {status}"
    );
    assert!(!response.text().await.unwrap().is_empty(), "错误必须有说明");
}

#[tokio::test]
async fn get_requires_sse_accept() {
    let hub = start_stateful_hub(None).await;
    let session = hub.initialize(CLAUDE_TOKEN).await;
    // GET 必须声明 Accept: text/event-stream。
    let response = hub
        .client
        .get(&hub.base_url)
        .header("Authorization", format!("Bearer {CLAUDE_TOKEN}"))
        .header("Accept", "application/json")
        .header("Mcp-Session-Id", &session)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    assert!(
        (400..500).contains(&status),
        "GET 缺 SSE Accept 应 4xx，实际 {status}"
    );
}

#[tokio::test]
async fn unsupported_protocol_version_yields_explicit_error() {
    let hub = start_stateful_hub(None).await;
    let session = hub.initialize(CLAUDE_TOKEN).await;
    let response = hub
        .client
        .post(&hub.base_url)
        .header("Authorization", format!("Bearer {CLAUDE_TOKEN}"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("MCP-Protocol-Version", "1999-01-01")
        .header("Mcp-Session-Id", &session)
        .json(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    assert!(
        (400..500).contains(&status),
        "不支持的协议版本必须 4xx，实际 {status}"
    );
}
