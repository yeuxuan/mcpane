//! 性能基线（TASK-702 自动化子集）。
//!
//! 阈值取执行计划目标；实测远低于阈值时以计划值为断言上限，
//! 实测数字记录到 docs/interop/perf-baseline.md。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mcpane_net::ProxyConfig;

use mcpane_hub::app::{HubOptions, build_router};
use mcpane_hub::auth::{ClientIdentity, TokenRegistry};
use mcpane_hub::state::AggregationState;
use mcpane_hub_catalog::{CatalogOverrides, ServerTools, build_revision};
use mcpane_hub_downstream::{DownstreamHandle, SpawnSpec};
use serde_json::json;

const TOKEN: &str = "perf-token-0000000000000000000000000000000000000000000001";

async fn start_hub(state: Arc<AggregationState>) -> (String, reqwest::Client) {
    let registry = TokenRegistry::new();
    registry.register(TOKEN, ClientIdentity::from_raw("perf".into(), "custom"));
    let router = build_router(
        Arc::new(registry),
        state,
        HubOptions {
            stateful: false, // 无状态：每请求独立，测纯路由路径
            session_keep_alive: None,
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (format!("http://{addr}/mcp"), reqwest::Client::new())
}

async fn rpc(client: &reqwest::Client, url: &str, body: serde_json::Value) -> serde_json::Value {
    let response = client
        .post(url)
        .header("Authorization", format!("Bearer {TOKEN}"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("MCP-Protocol-Version", "2025-11-25")
        .json(&body)
        .send()
        .await
        .unwrap();
    response.json().await.unwrap()
}

fn p95(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() * 95 / 100]
}

/// 100 个 Tool 的 Catalog：tools/list P95 < 100ms（计划目标）。
#[tokio::test]
async fn tools_list_p95_under_100ms_with_100_tools() {
    let tool_names: Vec<String> = (0..100).map(|i| format!("tool_{i}")).collect();
    let tools: Vec<rmcp::model::Tool> = tool_names
        .iter()
        .map(|name| {
            rmcp::model::Tool::new(
                name.clone(),
                "perf tool",
                Arc::new(serde_json::Map::from_iter([(
                    "type".to_owned(),
                    json!("object"),
                )])),
            )
        })
        .collect();
    let revision = build_revision(
        "ws_perf",
        &[ServerTools {
            server_id: "srv_perf".into(),
            server_slug: "perf".into(),
            tools,
            ..Default::default()
        }],
        &CatalogOverrides::default(),
    )
    .unwrap();
    let state = Arc::new(AggregationState::new(None));
    state.install_catalog(Arc::new(revision));
    state.set_connection_workspaces(std::collections::HashMap::from([(
        "perf".to_owned(),
        "ws_perf".to_owned(),
    )]));

    let (url, client) = start_hub(state).await;
    // 预热。
    rpc(
        &client,
        &url,
        json!({"jsonrpc":"2.0","id":0,"method":"tools/list"}),
    )
    .await;

    let mut samples = Vec::with_capacity(100);
    for i in 0..100 {
        let started = Instant::now();
        let value = rpc(
            &client,
            &url,
            json!({"jsonrpc":"2.0","id":i,"method":"tools/list"}),
        )
        .await;
        samples.push(started.elapsed());
        assert_eq!(value["result"]["tools"].as_array().unwrap().len(), 102); // 100 + 2 内建
    }
    let p95 = p95(samples);
    println!("tools/list(102 tools) P95 = {p95:?}");
    assert!(
        p95 < Duration::from_millis(100),
        "P95 {p95:?} 超出 100ms 目标"
    );
}

/// 内建工具端到端 P95 < 20ms（路由开销代理指标，不含下游执行）。
#[tokio::test]
async fn builtin_call_p95_under_20ms() {
    let state = Arc::new(AggregationState::new(None));
    let (url, client) = start_hub(state).await;
    rpc(
        &client,
        &url,
        json!({"jsonrpc":"2.0","id":0,"method":"tools/call",
            "params":{"name":"mcpane__echo","arguments":{"message":"warmup"}}}),
    )
    .await;

    let mut samples = Vec::with_capacity(100);
    for i in 0..100 {
        let started = Instant::now();
        let value = rpc(
            &client,
            &url,
            json!({"jsonrpc":"2.0","id":i,"method":"tools/call",
                "params":{"name":"mcpane__echo","arguments":{"message":format!("m{i}")}}}),
        )
        .await;
        samples.push(started.elapsed());
        assert_eq!(
            value["result"]["structuredContent"]["message"],
            format!("m{i}")
        );
    }
    let p95 = p95(samples);
    println!("mcpane__echo 端到端 P95 = {p95:?}");
    assert!(
        p95 < Duration::from_millis(20),
        "P95 {p95:?} 超出 20ms 目标"
    );
}

/// 50 个并发下游 Tool Call：全部成功且响应不串线。
#[tokio::test]
async fn fifty_concurrent_downstream_calls_do_not_cross() {
    let state = Arc::new(AggregationState::new(None));
    let spec = SpawnSpec::Stdio {
        command: env!("CARGO_BIN_EXE_mcpane-devserver").to_owned(),
        args: vec![],
        env: BTreeMap::from([("MCPANE_DEVSERVER_NAME".to_owned(), "perf".to_owned())]),
        cwd: None,
    };
    let handle = DownstreamHandle::connect(&spec, Duration::from_secs(10), &ProxyConfig::default())
        .await
        .unwrap();
    state.register_server("srv_perf", "perf", spec, handle);
    state
        .rebuild_catalog("ws_perf", &CatalogOverrides::default())
        .await
        .unwrap();
    state.set_connection_workspaces(std::collections::HashMap::from([(
        "perf".to_owned(),
        "ws_perf".to_owned(),
    )]));

    let (url, client) = start_hub(state).await;
    let url = Arc::new(url);

    let started = Instant::now();
    let mut jobs = Vec::new();
    for i in 0..50 {
        let client = client.clone();
        let url = url.clone();
        jobs.push(tokio::spawn(async move {
            let value = rpc(
                &client,
                &url,
                json!({"jsonrpc":"2.0","id":i,"method":"tools/call",
                    "params":{"name":"perf__search","arguments":{"query":format!("q{i}")}}}),
            )
            .await;
            (i, value)
        }));
    }
    for job in jobs {
        let (i, value) = job.await.unwrap();
        assert_eq!(value["id"], i, "JSON-RPC ID 串线");
        assert_eq!(
            value["result"]["structuredContent"]["query"],
            format!("q{i}"),
            "响应串线"
        );
    }
    println!("50 并发下游调用总耗时 = {:?}", started.elapsed());
}
