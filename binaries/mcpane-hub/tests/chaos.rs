//! Chaos 测试子集（TASK-701 可自动化项）。
//!
//! 覆盖：下游启动即退出 / 下游输出非法 JSON / tools/call 超时 /
//! Hub 端口被占用 / 认证暴力限速 / 健康监控自动恢复 /
//! 下游 HTTP 4xx-5xx / SSE 断流 / DB 写锁竞争 / 注入类畸形输入。
//! 配置类故障（只读、写入中修改、rename 失败）已由 mcpane-config
//! 失败注入测试覆盖。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use mcpane_net::ProxyConfig;

use mcpane_hub::app::{HubOptions, build_router};
use mcpane_hub::auth::{ClientIdentity, TokenRegistry};
use mcpane_hub::state::AggregationState;
use mcpane_hub_catalog::CatalogOverrides;
use mcpane_hub_downstream::{DownstreamError, DownstreamHandle, SpawnSpec};
use serde_json::json;

fn devserver_spec(name: &str) -> SpawnSpec {
    SpawnSpec::Stdio {
        command: env!("CARGO_BIN_EXE_mcpane-devserver").to_owned(),
        args: vec![],
        env: BTreeMap::from([("MCPANE_DEVSERVER_NAME".to_owned(), name.to_owned())]),
        cwd: None,
    }
}

/// 下游进程启动即退出：connect 必须在启动超时内返回明确错误。
#[tokio::test]
async fn downstream_that_exits_immediately_fails_cleanly() {
    let spec = SpawnSpec::Stdio {
        command: "/usr/bin/true".into(),
        args: vec![],
        env: BTreeMap::new(),
        cwd: None,
    };
    let result =
        DownstreamHandle::connect(&spec, Duration::from_secs(3), &ProxyConfig::default()).await;
    assert!(
        matches!(
            result,
            Err(DownstreamError::Spawn { .. }) | Err(DownstreamError::Timeout(_))
        ),
        "启动即退出必须得到明确错误"
    );
}

/// 下游输出非法 JSON：initialize 无法完成，明确失败而非悬挂。
#[tokio::test]
async fn downstream_emitting_garbage_fails_within_timeout() {
    let spec = SpawnSpec::Stdio {
        command: "/bin/cat".into(), // 回声非协议内容：客户端发的 JSON-RPC 被原样弹回
        args: vec!["/etc/hosts".into()],
        env: BTreeMap::new(),
        cwd: None,
    };
    let started = std::time::Instant::now();
    let result =
        DownstreamHandle::connect(&spec, Duration::from_secs(3), &ProxyConfig::default()).await;
    assert!(result.is_err(), "非法输出必须失败");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "必须在启动超时内返回，不得悬挂"
    );
}

/// tools/call 超时：慢工具触发 Timeout 错误，且计入崩溃计数。
#[tokio::test]
async fn slow_tool_call_times_out_with_clear_error() {
    let handle = DownstreamHandle::connect(
        &devserver_spec("slow"),
        Duration::from_secs(10),
        &ProxyConfig::default(),
    )
    .await
    .unwrap();
    let mut handle = handle;
    handle.call_timeout = Duration::from_millis(300);

    let mut arguments = serde_json::Map::new();
    arguments.insert("ms".into(), json!(2_000));
    let result = handle.call_tool("sleep", Some(arguments)).await;
    assert!(matches!(result, Err(DownstreamError::Timeout(_))));
    assert_eq!(handle.crash_count(), 1, "超时应计入崩溃计数");

    // 快调用恢复后计数清零。
    let mut arguments = serde_json::Map::new();
    arguments.insert("ms".into(), json!(1));
    let result = handle.call_tool("sleep", Some(arguments)).await;
    assert!(result.is_ok());
    assert_eq!(handle.crash_count(), 0);
    handle.close().await;
}

/// Hub 端口被占用：第二个绑定必须明确失败，第一个不受影响。
#[tokio::test]
async fn occupied_port_fails_second_bind_only() {
    let first = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = first.local_addr().unwrap();
    let second = tokio::net::TcpListener::bind(addr).await;
    assert!(second.is_err(), "端口占用必须明确报错");
    // 第一个监听依然可用。
    let probe = tokio::net::TcpStream::connect(addr).await;
    assert!(probe.is_ok());
}

/// 认证暴力限速：连续 10 次坏 Token 后返回 429 + Retry-After。
#[tokio::test]
async fn auth_bruteforce_gets_rate_limited() {
    let registry = TokenRegistry::new();
    registry.register(
        "chaos-good-token",
        ClientIdentity::from_raw("c1".into(), "claude"),
    );
    let state = Arc::new(AggregationState::new(None));
    let router = build_router(
        Arc::new(registry),
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
    let client = reqwest::Client::new();
    let url = format!("http://{addr}/mcp");

    let mut last_status = 0;
    for i in 0..12 {
        let response = client
            .post(&url)
            .header("Authorization", format!("Bearer wrong-{i}"))
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .json(&json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
            .send()
            .await
            .unwrap();
        last_status = response.status().as_u16();
    }
    assert_eq!(last_status, 429, "连续暴力尝试必须触发限速");

    // 锁定期内即便正确 Token 也被拒（防侧信道确认），错误明确。
    let response = client
        .post(&url)
        .header("Authorization", "Bearer chaos-good-token")
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .json(&json!({"jsonrpc":"2.0","id":2,"method":"ping"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 429);
    assert!(response.headers().contains_key("retry-after"));
}

/// 健康监控自动恢复：下游崩溃后无需人工干预即恢复服务（TASK-404）。
#[tokio::test]
async fn health_monitor_auto_recovers_crashed_downstream() {
    let state = Arc::new(AggregationState::new(None));
    let spec = devserver_spec("autoheal");
    let handle = DownstreamHandle::connect(&spec, Duration::from_secs(10), &ProxyConfig::default())
        .await
        .unwrap();
    state.register_server("srv_heal", "autoheal", spec, handle);
    state
        .rebuild_catalog("ws_chaos", &CatalogOverrides::default())
        .await
        .unwrap();
    state.spawn_health_monitor(Duration::from_millis(200));

    // 杀死下游进程（crash 工具使进程 exit(42)）。
    let handle = state.downstream("srv_heal").unwrap();
    let _ = handle.call_tool("crash", None).await;

    // 等待监控探测（2 次失败）→ 自动重连 → Ready。
    let mut recovered = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let fresh = state.downstream("srv_heal").unwrap();
        let mut arguments = serde_json::Map::new();
        arguments.insert("query".into(), json!("post-heal"));
        if let Ok(result) = fresh.call_tool("search", Some(arguments)).await {
            let content = serde_json::to_string(&result).unwrap();
            assert!(content.contains("autoheal"), "{content}");
            recovered = true;
            break;
        }
    }
    assert!(recovered, "健康监控必须在崩溃后自动恢复下游");
    let status = state
        .registered_servers()
        .into_iter()
        .find(|s| s.server_id == "srv_heal")
        .unwrap()
        .status();
    assert_eq!(status, mcpane_hub::state::ServerStatus::Ready);
}

/// 活着的下游 MCP ping 应立即成功（健康探测不再走 tools/list）。
#[tokio::test]
async fn probe_succeeds_on_live_downstream() {
    let handle = DownstreamHandle::connect(
        &devserver_spec("probe-ok"),
        Duration::from_secs(10),
        &ProxyConfig::default(),
    )
    .await
    .unwrap();
    handle.probe().await.expect("活着的下游 ping 应成功");
    handle.close().await;
}

/// 下游进程退出后 ping 必须失败，健康监控靠此判定重连。
#[tokio::test]
async fn probe_fails_after_downstream_exit() {
    let handle = DownstreamHandle::connect(
        &devserver_spec("probe-dead"),
        Duration::from_secs(10),
        &ProxyConfig::default(),
    )
    .await
    .unwrap();
    let _ = handle.call_tool("crash", None).await;
    let err = handle.probe().await;
    assert!(err.is_err(), "进程退出后 ping 必须失败：{err:?}");
}

// ---------- 下游 HTTP 4xx/5xx ----------

/// 起一个对所有请求返回固定状态码的假 HTTP 下游。
async fn start_erroring_http_downstream(status: u16) -> String {
    let status = axum::http::StatusCode::from_u16(status).unwrap();
    let router = axum::Router::new().fallback(move || async move { status });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{addr}/mcp")
}

/// HTTP 下游返回 401/500：connect 必须在启动超时内明确失败，不悬挂不 panic。
#[tokio::test]
async fn http_downstream_4xx_5xx_fails_cleanly() {
    for status in [401u16, 500] {
        let url = start_erroring_http_downstream(status).await;
        let spec = SpawnSpec::Http {
            url,
            bearer: None,
            bearer_env: None,
            env: BTreeMap::new(),
            headers: BTreeMap::new(),
            oauth: None,
        };
        let started = std::time::Instant::now();
        let result =
            DownstreamHandle::connect(&spec, Duration::from_secs(3), &ProxyConfig::default()).await;
        assert!(result.is_err(), "HTTP {status} 下游必须连接失败");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "HTTP {status} 必须在启动超时内返回"
        );
    }
}

// ---------- SSE 下游断流 ----------

/// SSE 断流实验台：首次握手正常（endpoint 事件 + initialize/tools 应答），
/// `kill` 后消息通道关闭（SSE 流优雅结束），重连拿到的是只发 endpoint 事件的
/// 空流——模拟服务端半死：接受连接但不再应答。
struct FlakySseState {
    tx: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<serde_json::Value>>>,
    rx: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>>>,
}

async fn start_flaky_sse_downstream() -> (String, Arc<FlakySseState>) {
    use axum::response::sse::{Event, Sse};
    use futures::StreamExt;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
    let state = Arc::new(FlakySseState {
        tx: std::sync::Mutex::new(Some(tx)),
        rx: std::sync::Mutex::new(Some(rx)),
    });

    let sse_state = state.clone();
    let sse_handler = move || {
        let rx = sse_state.rx.lock().unwrap().take();
        async move {
            let endpoint = futures::stream::once(async {
                Ok::<_, std::convert::Infallible>(
                    Event::default().event("endpoint").data("/message?sid=f1"),
                )
            });
            // 首连：透传应答流；重连（rx 已被取走）：永远静默。
            let body: futures::stream::BoxStream<'static, Result<Event, std::convert::Infallible>> =
                match rx {
                    Some(mut rx) => endpoint
                        .chain(futures::stream::poll_fn(move |cx| {
                            rx.poll_recv(cx)
                                .map(|opt| opt.map(|v| Ok(Event::default().data(v.to_string()))))
                        }))
                        .boxed(),
                    None => endpoint.chain(futures::stream::pending()).boxed(),
                };
            Sse::new(body)
        }
    };
    let msg_state = state.clone();
    let message_handler = move |body: String| async move {
        let request: serde_json::Value = serde_json::from_str(&body).unwrap();
        let id = request.get("id").cloned();
        let response = match request["method"].as_str().unwrap_or_default() {
            "initialize" => Some(json!({
                "jsonrpc": "2.0", "id": id,
                "result": {
                    "protocolVersion": request["params"]["protocolVersion"],
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "flaky-sse", "version": "0" }
                }
            })),
            "tools/list" => Some(json!({
                "jsonrpc": "2.0", "id": id,
                "result": { "tools": [{ "name": "noop", "inputSchema": { "type": "object" } }] }
            })),
            _ => None,
        };
        if let (Some(response), Some(tx)) = (response, msg_state.tx.lock().unwrap().as_ref()) {
            let _ = tx.send(response);
        }
        axum::http::StatusCode::ACCEPTED
    };
    let router = axum::Router::new()
        .route("/sse", axum::routing::get(sse_handler))
        .route("/message", axum::routing::post(message_handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (format!("http://{addr}/sse"), state)
}

/// SSE 下游断流：已建立的连接在服务端断流后，后续调用必须在 call_timeout 内
/// 报错（自动重连拿不到应答就明确超时），不得无限悬挂。
#[tokio::test]
async fn sse_downstream_stream_drop_fails_calls_within_timeout() {
    let (url, state) = start_flaky_sse_downstream().await;
    let spec = SpawnSpec::Sse {
        url,
        bearer: None,
        bearer_env: None,
        env: BTreeMap::new(),
        headers: BTreeMap::new(),
    };
    let mut handle =
        DownstreamHandle::connect(&spec, Duration::from_secs(5), &ProxyConfig::default())
            .await
            .expect("断流前应能正常握手");
    assert_eq!(handle.list_tools().await.unwrap().len(), 1);

    // 断流：丢弃发送端，SSE 流优雅结束，此后服务端不再应答。
    state.tx.lock().unwrap().take();
    handle.call_timeout = Duration::from_secs(1);

    let started = std::time::Instant::now();
    let result = handle.call_tool("noop", None).await;
    assert!(result.is_err(), "断流后的调用必须失败");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "必须在 call_timeout 量级内返回，不得悬挂：{:?}",
        started.elapsed()
    );
    handle.close().await;
}

// ---------- DB 写锁竞争 ----------

/// 另一连接长持写锁时：请求日志写入被阻塞，但工具调用必须照常成功返回
/// （日志尽力而为，绝不反向拖垮数据面），锁释放后 Hub 依旧健康。
#[tokio::test]
async fn db_write_lock_does_not_break_data_plane() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("mcpane-chaos-db-{nanos}"));
    let db_path = dir.join("mcpane.db");
    let pool = mcpane_storage::open_database(&db_path).await.unwrap();

    let registry = TokenRegistry::new();
    registry.register(
        "chaos-dblock-token-0000000000000000000000000000000000000001",
        ClientIdentity::from_raw("c_dblock".into(), "custom"),
    );
    let state = Arc::new(AggregationState::new(Some(mcpane_storage::LogRepo::new(
        pool.clone(),
    ))));
    let spec = devserver_spec("dblock");
    let handle = DownstreamHandle::connect(&spec, Duration::from_secs(10), &ProxyConfig::default())
        .await
        .unwrap();
    state.register_server("srv_dblock", "dblock", spec, handle);
    state
        .rebuild_catalog("ws_default", &CatalogOverrides::default())
        .await
        .unwrap();
    let router = build_router(
        Arc::new(registry),
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
    let url = format!("http://{addr}/mcp");

    // 另一连接持写锁 2s（模拟备份/大事务）。
    let lock_pool = mcpane_storage::open_database(&db_path).await.unwrap();
    let mut tx = lock_pool.begin().await.unwrap();
    sqlx::query("UPDATE app_settings SET hub_enabled = hub_enabled WHERE id = 1")
        .execute(&mut *tx)
        .await
        .unwrap();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let _ = tx.rollback().await;
    });

    let call = |id: i64, message: &str| {
        let url = url.clone();
        let message = message.to_owned();
        async move {
            reqwest::Client::new()
                .post(&url)
                .header(
                    "Authorization",
                    "Bearer chaos-dblock-token-0000000000000000000000000000000000000001",
                )
                .header("Accept", "application/json, text/event-stream")
                .header("Content-Type", "application/json")
                .header("MCP-Protocol-Version", "2025-11-25")
                .json(&json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
                    "params":{"name":"dblock__search","arguments":{"query":message}}}))
                .send()
                .await
                .unwrap()
        }
    };

    let started = std::time::Instant::now();
    let response = call(1, "under-lock").await;
    assert_eq!(response.status().as_u16(), 200, "持锁期间调用必须成功");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "调用不得被锁拖到失控：{:?}",
        started.elapsed()
    );

    // 锁释放后再调一次：Hub 未被拖垮。
    tokio::time::sleep(Duration::from_millis(2_200)).await;
    let response = call(2, "after-lock").await;
    assert_eq!(response.status().as_u16(), 200);

    std::fs::remove_dir_all(&dir).ok();
}

// ---------- 注入类输入 ----------

/// 畸形/恶意输入必须得到协议级错误或 4xx，绝不 panic：
/// 路径穿越工具名、控制字符工具名、1MB 超长参数、非法 JSON 请求体。
#[tokio::test]
async fn malicious_inputs_get_clean_errors_not_panics() {
    let registry = TokenRegistry::new();
    registry.register(
        "chaos-inject-token-0000000000000000000000000000000000000001",
        ClientIdentity::from_raw("c_inject".into(), "custom"),
    );
    let state = Arc::new(AggregationState::new(None));
    let router = build_router(
        Arc::new(registry),
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
    let url = format!("http://{addr}/mcp");
    let client = reqwest::Client::new();
    let post = |body: serde_json::Value| {
        client
            .post(&url)
            .header(
                "Authorization",
                "Bearer chaos-inject-token-0000000000000000000000000000000000000001",
            )
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .header("MCP-Protocol-Version", "2025-11-25")
            .json(&body)
    };

    // 1. 路径穿越 / 控制字符工具名 → JSON-RPC 错误（unknown tool），非 panic。
    for name in ["../../etc/passwd", "tool\r\nInjected: header", "\u{0}\u{1}"] {
        let response = post(json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":name,"arguments":{}}}))
        .send()
        .await
        .unwrap();
        assert!(
            response.status().is_success(),
            "恶意工具名应到协议层被拒（{name:?}）"
        );
        let text = response.text().await.unwrap();
        assert!(
            text.contains("error") || text.contains("isError"),
            "恶意工具名必须报错：{name:?} → {text}"
        );
    }

    // 2. 1MB 超长参数：干净处理（回显成功或明确错误），不 panic。
    let huge = "x".repeat(1024 * 1024);
    let response = post(json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
        "params":{"name":"mcpane__echo","arguments":{"message":huge}}}))
    .send()
    .await
    .unwrap();
    assert!(
        response.status().as_u16() < 500,
        "超长参数不得引发 5xx：{}",
        response.status()
    );

    // 3. 非法 JSON 体 → 4xx/解析错误，服务不倒。
    let response = client
        .post(&url)
        .header(
            "Authorization",
            "Bearer chaos-inject-token-0000000000000000000000000000000000000001",
        )
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .body("{not-json")
        .send()
        .await
        .unwrap();
    assert!(
        response.status().as_u16() >= 400 || response.text().await.unwrap().contains("error"),
        "非法 JSON 必须得到明确错误"
    );

    // 服务仍健康：正常调用照常成功。
    let response = post(json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
        "params":{"name":"mcpane__echo","arguments":{"message":"still-alive"}}}))
    .send()
    .await
    .unwrap();
    assert_eq!(response.status().as_u16(), 200, "注入尝试后服务必须仍健康");
}
