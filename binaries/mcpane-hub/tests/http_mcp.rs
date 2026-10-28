//! /mcp 数据面集成测试（AGENTS.md 规则 11：并发、鉴权、隔离）。
//!
//! 启动真实 TCP 监听（127.0.0.1 随机端口）+ 完整中间件栈，
//! 用 reqwest 以 MCP 2025-11-25 Streamable HTTP 语义驱动。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use mcpane_hub::app::{HubOptions, build_router};
use mcpane_hub::auth::{ClientIdentity, TokenRegistry};
use serde_json::{Value, json};

const CLAUDE_TOKEN: &str = "test-claude-token-0000000000000000000000000000000000000001";
const CODEX_TOKEN: &str = "test-codex-token-00000000000000000000000000000000000000002";

struct TestHub {
    base_url: String,
    client: reqwest::Client,
    registry: Arc<TokenRegistry>,
}

async fn start_hub() -> TestHub {
    let registry = TokenRegistry::new();
    registry.register(
        CLAUDE_TOKEN,
        ClientIdentity::from_raw("client_claude_test".into(), "claude"),
    );
    registry.register(
        CODEX_TOKEN,
        ClientIdentity::from_raw("client_codex_test".into(), "codex"),
    );

    let registry = Arc::new(registry);
    // 本文件覆盖 TASK-100 无状态基线；有状态 Session 见 tests/http_session.rs。
    let state = std::sync::Arc::new(mcpane_hub::state::AggregationState::new(None));
    let router = build_router(
        registry.clone(),
        state,
        HubOptions {
            stateful: false,
            session_keep_alive: None,
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    TestHub {
        base_url: format!("http://{addr}/mcp"),
        client: reqwest::Client::new(),
        registry,
    }
}

impl TestHub {
    async fn post_raw(&self, token: Option<&str>, body: Value) -> reqwest::Response {
        let mut request = self
            .client
            .post(&self.base_url)
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .header("MCP-Protocol-Version", "2025-11-25")
            .json(&body);
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        request.send().await.unwrap()
    }

    /// 发送 JSON-RPC 请求并解析响应（兼容 application/json 与 SSE 两种形式）。
    async fn rpc(&self, token: &str, body: Value) -> Value {
        let response = self.post_raw(Some(token), body).await;
        let status = response.status();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let text = response.text().await.unwrap();
        assert!(status.is_success(), "unexpected status {status}: {text}");
        if content_type.starts_with("text/event-stream") {
            let data_line = text
                .lines()
                .find_map(|l| l.strip_prefix("data: "))
                .unwrap_or_else(|| panic!("SSE 响应中没有 data 行: {text}"));
            serde_json::from_str(data_line).unwrap()
        } else {
            serde_json::from_str(&text).unwrap()
        }
    }

    fn initialize_body(id: i64) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "mcpane-interop-test", "version": "0.0.1" }
            }
        })
    }

    async fn initialize(&self, token: &str) -> Value {
        let value = self.rpc(token, Self::initialize_body(1)).await;
        // 无状态模式无需 notifications/initialized 即可继续，但仍按规范发送。
        let response = self
            .post_raw(
                Some(token),
                json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            )
            .await;
        assert_eq!(response.status().as_u16(), 202, "notification 应返回 202");
        value
    }

    async fn call_tool(&self, token: &str, id: i64, name: &str, arguments: Value) -> Value {
        self.rpc(
            token,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": { "name": name, "arguments": arguments }
            }),
        )
        .await
    }
}

// ---------- 鉴权 ----------

#[tokio::test]
async fn rejects_missing_and_invalid_tokens() {
    let hub = start_hub().await;

    let response = hub.post_raw(None, TestHub::initialize_body(1)).await;
    assert_eq!(response.status().as_u16(), 401);
    assert!(response.headers().contains_key("www-authenticate"));

    let response = hub
        .post_raw(Some("wrong-token"), TestHub::initialize_body(1))
        .await;
    assert_eq!(response.status().as_u16(), 401);

    // GET / DELETE 同样必须认证。
    let response = hub.client.get(&hub.base_url).send().await.unwrap();
    assert_eq!(response.status().as_u16(), 401);
    let response = hub.client.delete(&hub.base_url).send().await.unwrap();
    assert_eq!(response.status().as_u16(), 401);
}

#[tokio::test]
async fn accepts_both_registered_tokens() {
    let hub = start_hub().await;
    for token in [CLAUDE_TOKEN, CODEX_TOKEN] {
        let value = hub.initialize(token).await;
        assert_eq!(value["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(value["result"]["serverInfo"]["name"], "mcpane-hub");
    }
}

// ---------- 工具列表与命名空间 ----------

#[tokio::test]
async fn tools_list_exposes_prefixed_builtin_tools() {
    let hub = start_hub().await;
    hub.initialize(CLAUDE_TOKEN).await;
    let value = hub
        .rpc(
            CLAUDE_TOKEN,
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        )
        .await;
    let tools = value["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(
        names.contains(&"mcpane__echo"),
        "缺少 mcpane__echo: {names:?}"
    );
    assert!(
        names.contains(&"mcpane__connection_info"),
        "缺少 mcpane__connection_info: {names:?}"
    );
    for name in names {
        assert!(name.starts_with("mcpane__"));
    }
}

// ---------- echo 与 connection_info ----------

#[tokio::test]
async fn echo_roundtrip_and_connection_identity() {
    let hub = start_hub().await;
    hub.initialize(CLAUDE_TOKEN).await;

    let value = hub
        .call_tool(
            CLAUDE_TOKEN,
            3,
            "mcpane__echo",
            json!({ "message": "你好 MCPane" }),
        )
        .await;
    assert_eq!(
        value["result"]["structuredContent"]["message"],
        "你好 MCPane"
    );

    let value = hub
        .call_tool(CLAUDE_TOKEN, 4, "mcpane__connection_info", json!({}))
        .await;
    let info = &value["result"]["structuredContent"];
    assert_eq!(info["clientConnectionId"], "client_claude_test");
    assert_eq!(info["clientType"], "claude");
    assert_eq!(info["protocolVersion"], "2025-11-25");
    assert_eq!(info["sessionId"], Value::Null);
    assert!(info["requestId"].as_str().unwrap().starts_with("req_"));
}

/// Token 身份不串线：Claude 与 Codex 各自拿到自己的连接身份。
#[tokio::test]
async fn identities_do_not_cross_between_clients() {
    let hub = start_hub().await;
    hub.initialize(CLAUDE_TOKEN).await;
    hub.initialize(CODEX_TOKEN).await;

    let claude = hub
        .call_tool(CLAUDE_TOKEN, 5, "mcpane__connection_info", json!({}))
        .await;
    let codex = hub
        .call_tool(CODEX_TOKEN, 5, "mcpane__connection_info", json!({}))
        .await;
    assert_eq!(
        claude["result"]["structuredContent"]["clientConnectionId"],
        "client_claude_test"
    );
    assert_eq!(
        codex["result"]["structuredContent"]["clientConnectionId"],
        "client_codex_test"
    );
    assert_ne!(
        claude["result"]["structuredContent"]["requestId"],
        codex["result"]["structuredContent"]["requestId"]
    );
}

// ---------- 并发隔离（TASK-103 预演） ----------

/// 双客户端各 20 个并发 echo：响应内容与 JSON-RPC ID 均不得串线。
#[tokio::test]
async fn concurrent_echo_calls_do_not_cross() {
    let hub = Arc::new(start_hub().await);
    hub.initialize(CLAUDE_TOKEN).await;
    hub.initialize(CODEX_TOKEN).await;

    let mut handles = Vec::new();
    for i in 0..20i64 {
        for (token, label) in [(CLAUDE_TOKEN, "claude"), (CODEX_TOKEN, "codex")] {
            let hub = hub.clone();
            handles.push(tokio::spawn(async move {
                let message = format!("{label}-{i}");
                let id = 1000 + i;
                let value = hub
                    .call_tool(token, id, "mcpane__echo", json!({ "message": message }))
                    .await;
                (value, id, message)
            }));
        }
    }

    for handle in handles {
        let (value, id, message) = handle.await.unwrap();
        assert_eq!(value["id"], id, "JSON-RPC ID 串线: {value}");
        assert_eq!(
            value["result"]["structuredContent"]["message"], *message,
            "echo 内容串线: {value}"
        );
    }
}

// ---------- Host / Origin 防护 ----------

#[tokio::test]
async fn rejects_bad_host_and_origin() {
    let hub = start_hub().await;

    // 恶意 Origin（DNS rebinding 场景）。
    let response = hub
        .client
        .post(&hub.base_url)
        .header("Authorization", format!("Bearer {CLAUDE_TOKEN}"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("Origin", "http://evil.example.com")
        .json(&TestHub::initialize_body(1))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 403);

    // 合法 loopback Origin 放行。
    let response = hub
        .client
        .post(&hub.base_url)
        .header("Authorization", format!("Bearer {CLAUDE_TOKEN}"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("Origin", "http://127.0.0.1:1420")
        .json(&TestHub::initialize_body(1))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
}

/// 伪造 Host 头（工具层直接构造，不经 reqwest 的 URL→Host 推导）。
#[tokio::test]
async fn rejects_forged_host_header() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let hub = start_hub().await;
    let authority = hub
        .base_url
        .trim_start_matches("http://")
        .trim_end_matches("/mcp");

    let mut stream = tokio::net::TcpStream::connect(authority).await.unwrap();
    let body = TestHub::initialize_body(1).to_string();
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: evil.example.com\r\nAuthorization: Bearer {CLAUDE_TOKEN}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 403"),
        "伪造 Host 应被拒绝: {response}"
    );
}

// ---------- Token 撤销隔离（TASK-103 预演） ----------

/// 撤销 Claude Token：Claude 立即 401，Codex 完全不受影响；恢复后放行。
#[tokio::test]
async fn revoking_one_token_does_not_affect_the_other() {
    let hub = start_hub().await;
    hub.initialize(CLAUDE_TOKEN).await;
    hub.initialize(CODEX_TOKEN).await;

    hub.registry.set_enabled("client_claude_test", false);

    let response = hub
        .post_raw(Some(CLAUDE_TOKEN), TestHub::initialize_body(1))
        .await;
    assert_eq!(response.status().as_u16(), 401, "撤销后 Claude 必须 401");

    let value = hub
        .call_tool(
            CODEX_TOKEN,
            6,
            "mcpane__echo",
            json!({ "message": "codex-alive" }),
        )
        .await;
    assert_eq!(
        value["result"]["structuredContent"]["message"], "codex-alive",
        "撤销 Claude 不得影响 Codex"
    );

    hub.registry.set_enabled("client_claude_test", true);
    let value = hub.initialize(CLAUDE_TOKEN).await;
    assert_eq!(value["result"]["protocolVersion"], "2025-11-25");
}
