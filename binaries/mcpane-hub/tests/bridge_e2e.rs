//! bridge 端到端：起真实 Hub 数据面 → 以 `mcpane-hub bridge` 为 stdio 子进程
//! → 经桥完成 initialize / tools/list / tools/call 全链路。
//!
//! 验证点：消息透传（含会话头处理）、MCPANE_TOKEN 注入、缺 token 时以
//! usage error 退出而不是挂着。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use mcpane_hub::app::{HubOptions, build_router};
use mcpane_hub::auth::{ClientIdentity, TokenRegistry};
use mcpane_hub::state::AggregationState;
use mcpane_hub_downstream::{DownstreamHandle, SpawnSpec};
use mcpane_net::ProxyConfig;

const TOKEN: &str = "bridge-e2e-token-000000000000000000000000000000000000000001";

/// 起真实 Hub（内建工具即可，无需下游），返回数据面 URL。
async fn start_hub() -> String {
    let registry = TokenRegistry::new();
    registry.register(
        TOKEN,
        ClientIdentity::from_raw("client_bridge_test".into(), "custom"),
    );
    let state = Arc::new(AggregationState::new(None));
    let router = build_router(
        Arc::new(registry),
        state,
        HubOptions {
            stateful: true,
            session_keep_alive: None,
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{addr}/mcp")
}

fn bridge_spec(url: &str, token: &str) -> SpawnSpec {
    SpawnSpec::Stdio {
        command: env!("CARGO_BIN_EXE_mcpane-hub").to_owned(),
        args: vec!["bridge".into()],
        env: BTreeMap::from([
            ("MCPANE_TOKEN".to_owned(), token.to_owned()),
            ("MCPANE_HUB_URL".to_owned(), url.to_owned()),
        ]),
        cwd: None,
    }
}

#[tokio::test]
async fn bridge_relays_initialize_list_and_call() {
    let url = start_hub().await;

    // 复用下游客户端封装当"宿主客户端"：spawn 桥进程 + initialize 握手。
    let handle = DownstreamHandle::connect(
        &bridge_spec(&url, TOKEN),
        Duration::from_secs(10),
        &ProxyConfig::default(),
    )
    .await
    .expect("经 bridge 应能完成 initialize");

    let tools = handle.list_tools().await.expect("tools/list 应透传");
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert!(names.contains(&"mcpane__echo"), "{names:?}");

    let result = handle
        .call_tool(
            "mcpane__echo",
            Some(
                serde_json::json!({ "message": "through the bridge" })
                    .as_object()
                    .cloned()
                    .unwrap(),
            ),
        )
        .await
        .expect("tools/call 应透传");
    let structured = serde_json::to_value(&result.structured_content).unwrap();
    assert_eq!(structured["message"], "through the bridge");

    handle.close().await;
}

/// 错误 token：桥能起来，但 Hub 会拒绝——上游错误必须透传给宿主客户端
/// （initialize 失败），而不是桥自己吞掉装正常。
#[tokio::test]
async fn bridge_surfaces_auth_failure() {
    let url = start_hub().await;
    let result = DownstreamHandle::connect(
        &bridge_spec(
            &url,
            "wrong-token-000000000000000000000000000000000000000000",
        ),
        Duration::from_secs(10),
        &ProxyConfig::default(),
    )
    .await;
    assert!(result.is_err(), "错误 token 不应完成握手");
}

/// 缺 MCPANE_TOKEN：以 usage error（exit 2）立即退出，stderr 给出指引。
#[tokio::test]
async fn bridge_without_token_exits_with_usage_error() {
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_mcpane-hub"))
        .arg("bridge")
        .env_remove("MCPANE_TOKEN")
        .output()
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("MCPANE_TOKEN"), "{stderr}");
}

/// 桥是透明消息泵，本身不理解协议。但新协议要求 `Mcp-Method`/`Mcp-Name`
/// 这类 **HTTP 头**，而头由桥的 HTTP 客户端传输生成——它能否为一条自己没有
/// 参与协商的转发消息补上正确的头，决定了桥在 2026-07-28 下还能不能用。
///
/// rmcp 3.x 从消息自身的 `_meta` 读协议版本来决定加不加这些头，所以答案是
/// 能；这个测试把这件事钉死，免得将来 SDK 改了实现而我们无声退回 legacy。
#[tokio::test]
async fn bridge_relays_the_modern_protocol_end_to_end() {
    let url = start_hub().await;
    let handle = DownstreamHandle::connect(
        &bridge_spec(&url, TOKEN),
        Duration::from_secs(10),
        &ProxyConfig::default(),
    )
    .await
    .expect("经 bridge 建连");

    // connection_info 回显的是 Hub 侧**本次请求实际协商**到的版本，
    // 比任何间接推断都准。
    let result = handle
        .call_tool("mcpane__connection_info", None)
        .await
        .expect("connection_info 应透传");
    let info = serde_json::to_value(&result.structured_content).unwrap();
    assert_eq!(
        info["protocolVersion"], "2026-07-28",
        "经桥的请求没走上新协议（Mcp-* 头很可能没补上）: {info}"
    );
    // 新协议无会话，桥也不该凭空造一个。
    assert!(
        info["sessionId"].is_null(),
        "新协议下不该有 sessionId: {info}"
    );

    handle.close().await;
}
