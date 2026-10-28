//! GATE-3 聚合验收：stdio + Streamable HTTP 双下游、命名空间路由、
//! 崩溃隔离、二次权限校验、恢复、日志无 Secret。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use mcpane_net::ProxyConfig;

use mcpane_hub::app::{HubOptions, build_router};
use mcpane_hub::auth::{ClientIdentity, TokenRegistry};
use mcpane_hub::state::AggregationState;
use mcpane_hub_catalog::CatalogOverrides;
use mcpane_hub_downstream::{DownstreamHandle, SpawnSpec};
use rmcp::{
    ServerHandler,
    handler::server::wrapper::{Json, Parameters},
    model::{Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const CLAUDE_TOKEN: &str = "gate3-claude-token-000000000000000000000000000000000000001";
const CODEX_TOKEN: &str = "gate3-codex-token-00000000000000000000000000000000000000002";

// ---------- 测试用 HTTP 下游（in-process Streamable HTTP MCP Server） ----------

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
struct SearchParams {
    query: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct SearchOutput {
    server: String,
    query: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct ConfirmOutput {
    action: String,
}

/// slow 工具的观测探针：started 供测试确认调用已在飞，cancelled 记录取消路径。
#[derive(Default)]
struct SlowProbe {
    started: std::sync::atomic::AtomicBool,
    cancelled: std::sync::atomic::AtomicBool,
}

#[derive(Clone, Default)]
struct HttpDownstream {
    slow_probe: Arc<SlowProbe>,
}

#[tool_router]
impl HttpDownstream {
    #[tool(name = "search", description = "http downstream search")]
    fn search(
        &self,
        Parameters(SearchParams { query }): Parameters<SearchParams>,
    ) -> Json<SearchOutput> {
        Json(SearchOutput {
            server: "filesystem-http".into(),
            query,
        })
    }

    /// 旧式确认：调用期间向客户端发 elicitation/create，回报用户动作。
    #[tool(name = "confirm", description = "asks the user before acting")]
    async fn confirm(
        &self,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Json<ConfirmOutput> {
        let request = rmcp::model::ElicitRequestParams::FormElicitationParams {
            meta: None,
            message: "proceed?".into(),
            requested_schema: rmcp::model::ElicitationSchema::builder()
                .required_string("reason")
                .build()
                .unwrap(),
        };
        let action = match context.peer.create_elicitation(request).await {
            Ok(result) => format!("{:?}", result.action).to_lowercase(),
            Err(error) => format!("error: {error}"),
        };
        Json(ConfirmOutput { action })
    }

    /// 取消级联样例：等 30s 或被取消。ct 信号与 future 被 drop 两条路径都
    /// 会置位 cancelled——不预设 rmcp 用哪种方式终止 handler。
    #[tool(name = "slow", description = "sleeps until cancelled")]
    async fn slow(
        &self,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Json<ConfirmOutput> {
        use std::sync::atomic::Ordering;
        self.slow_probe.started.store(true, Ordering::SeqCst);
        struct SetOnDrop(Arc<SlowProbe>, bool);
        impl Drop for SetOnDrop {
            fn drop(&mut self) {
                if self.1 {
                    self.0.cancelled.store(true, Ordering::SeqCst);
                }
            }
        }
        let mut armed = SetOnDrop(self.slow_probe.clone(), true);
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(30)) => {
                armed.1 = false;
                Json(ConfirmOutput { action: "slept".into() })
            }
            () = context.ct.cancelled() => {
                armed.1 = false;
                self.slow_probe.cancelled.store(true, Ordering::SeqCst);
                Json(ConfirmOutput { action: "cancelled".into() })
            }
        }
    }

    /// 长任务样例：带 progressToken 调用时先报一次进度再完成。
    #[tool(name = "report", description = "emits progress then completes")]
    async fn report(
        &self,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Json<ConfirmOutput> {
        let action = match context.meta.get_progress_token() {
            Some(token) => {
                let sent = context
                    .peer
                    .notify_progress(
                        rmcp::model::ProgressNotificationParam::new(token, 0.5)
                            .with_total(1.0)
                            .with_message("halfway"),
                    )
                    .await;
                match sent {
                    Ok(()) => "done".to_owned(),
                    Err(error) => format!("notify failed: {error}"),
                }
            }
            None => "no token".to_owned(),
        };
        Json(ConfirmOutput { action })
    }
}

#[tool_handler]
impl ServerHandler for HttpDownstream {
    /// 钉成 legacy：本 fixture 模拟尚未迁移 MRTR 的存量下游——旧式
    /// elicitation/create 只在 legacy 连接上有传递通道，新协议下 Hub 会
    /// 协商到 2026-07-28，该形态根本发不出来。
    fn supported_protocol_versions(
        &self,
    ) -> std::borrow::Cow<'static, [rmcp::model::ProtocolVersion]> {
        std::borrow::Cow::Borrowed(&[rmcp::model::ProtocolVersion::V_2025_11_25])
    }

    fn get_info(&self) -> ServerInfo {
        let mut implementation = Implementation::from_build_env();
        implementation.name = "filesystem-http".into();
        let mut info = ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .enable_resources()
                .enable_completions()
                .build(),
        );
        info.server_info = implementation;
        info.instructions = Some("HTTP 文件检索下游，支持全文 search。".into());
        info
    }

    /// 回显式补全：值加 `-suggestion` 后缀，测试据此确认转发链路。
    async fn complete(
        &self,
        request: rmcp::model::CompleteRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CompleteResult, rmcp::ErrorData> {
        // 断言 Hub 已把暴露名改写回原名（review 而非 filesystem__review）。
        if let rmcp::model::Reference::Prompt(prompt) = &request.r#ref
            && prompt.name != "review"
        {
            return Err(rmcp::ErrorData::invalid_params(
                format!("expected original prompt name, got: {}", prompt.name),
                None,
            ));
        }
        let info = rmcp::model::CompletionInfo::new(vec![format!(
            "{}-suggestion",
            request.argument.value
        )])
        .unwrap();
        Ok(rmcp::model::CompleteResult::new(info))
    }

    // rmcp 的 #[tool_handler] 只生成 tools 两个方法，prompts/resources 手动覆写。
    async fn list_prompts(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListPromptsResult, rmcp::ErrorData> {
        Ok(rmcp::model::ListPromptsResult {
            prompts: vec![rmcp::model::Prompt::new(
                "review",
                Some("review a change"),
                None,
            )],
            ..Default::default()
        })
    }

    async fn get_prompt(
        &self,
        request: rmcp::model::GetPromptRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::GetPromptResponse, rmcp::ErrorData> {
        if request.name != "review" {
            return Err(rmcp::ErrorData::invalid_params(
                format!("unknown prompt: {}", request.name),
                None,
            ));
        }
        Ok(
            rmcp::model::GetPromptResult::new(vec![rmcp::model::PromptMessage::new_text(
                rmcp::model::Role::User,
                "please review",
            )])
            .with_description("review a change")
            .into(),
        )
    }

    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListResourcesResult, rmcp::ErrorData> {
        Ok(rmcp::model::ListResourcesResult {
            resources: vec![rmcp::model::Resource::new(
                "mem://filesystem-http/readme",
                "readme",
            )],
            ..Default::default()
        })
    }

    async fn list_resource_templates(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListResourceTemplatesResult, rmcp::ErrorData> {
        Ok(rmcp::model::ListResourceTemplatesResult {
            resource_templates: vec![rmcp::model::ResourceTemplate::new(
                "mem://filesystem-http/logs/{name}",
                "logs",
            )],
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ReadResourceResponse, rmcp::ErrorData> {
        let uri = request.uri;
        let text = if uri == "mem://filesystem-http/readme" {
            "readme from http downstream".to_owned()
        } else if uri.starts_with("mem://filesystem-http/logs/") {
            format!("log content of {uri}")
        } else {
            return Err(rmcp::ErrorData::invalid_params(
                format!("unknown resource: {uri}"),
                None,
            ));
        };
        Ok(
            rmcp::model::ReadResourceResult::new(vec![rmcp::model::ResourceContents::text(
                text, uri,
            )])
            .into(),
        )
    }
}

/// 真正的 legacy 服务器不认识 server/discover，回 JSON-RPC -32601——Hub 的
/// Auto 探测只认 method_not_found 才回退 legacy initialize。rmcp 的 tower 层
/// 会抢在 handler 之前用 UNSUPPORTED_PROTOCOL_VERSION 拒掉 discover（那会让
/// Auto 直接连接失败），所以在 HTTP 层拦下来忠实模拟。
async fn legacy_discover_gate(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let (parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    if let Ok(message) = serde_json::from_slice::<Value>(&bytes)
        && message["method"] == "server/discover"
    {
        return axum::Json(json!({
            "jsonrpc": "2.0", "id": message["id"],
            "error": { "code": -32601, "message": "Method not found" }
        }))
        .into_response();
    }
    next.run(axum::extract::Request::from_parts(
        parts,
        axum::body::Body::from(bytes),
    ))
    .await
}

async fn start_http_downstream() -> (String, Arc<SlowProbe>) {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };
    // 探针在 handler 工厂外创建：每个 Session 一个 handler 实例，共享同一探针
    // 测试才观察得到。
    let probe = Arc::new(SlowProbe::default());
    let service_probe = probe.clone();
    let service = StreamableHttpService::new(
        move || {
            Ok(HttpDownstream {
                slow_probe: service_probe.clone(),
            })
        },
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default().with_legacy_session_mode(true),
    );
    let router = axum::Router::new()
        .nest_service("/mcp", axum::routing::any_service(service))
        .layer(axum::middleware::from_fn(legacy_discover_gate));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (format!("http://{addr}/mcp"), probe)
}

/// 回归（真机故障复现）：官方 TS SDK 的 Streamable HTTP 服务端在 HTTP 层
/// 校验 `MCP-Protocol-Version` 头，对 2026-07-28 回 400 + JSON-RPC -32600
/// ——不是 -32601，rmcp 的 Auto 不会自动回退。connect 层必须自行回退
/// legacy initialize，否则这类存量服务器一台都连不上。
#[tokio::test]
async fn http_downstream_with_header_version_gate_falls_back_to_legacy() {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };
    async fn ts_sdk_version_gate(
        request: axum::extract::Request,
        next: axum::middleware::Next,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        let modern = request
            .headers()
            .get("mcp-protocol-version")
            .and_then(|v| v.to_str().ok())
            == Some("2026-07-28");
        if modern {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                axum::Json(json!({ "jsonrpc": "2.0", "id": null,
                    "error": { "code": -32600, "message":
                        "Bad Request: Unsupported protocol version: 2026-07-28. \
                         Supported versions: 2024-11-05, 2025-03-26, 2025-06-18, 2025-11-25" } })),
            )
                .into_response();
        }
        next.run(request).await
    }

    let probe = Arc::new(SlowProbe::default());
    let service_probe = probe.clone();
    let service = StreamableHttpService::new(
        move || {
            Ok(HttpDownstream {
                slow_probe: service_probe.clone(),
            })
        },
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default().with_legacy_session_mode(true),
    );
    let router = axum::Router::new()
        .nest_service("/mcp", axum::routing::any_service(service))
        .layer(axum::middleware::from_fn(ts_sdk_version_gate));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let spec = SpawnSpec::Http {
        url: format!("http://{addr}/mcp"),
        bearer: None,
        bearer_env: None,
        env: BTreeMap::new(),
        headers: BTreeMap::new(),
        oauth: None,
    };
    let handle = DownstreamHandle::connect(&spec, Duration::from_secs(10), &ProxyConfig::default())
        .await
        .expect("新协议被 400 拒绝时应回退 legacy 连接成功");
    let tools = handle.list_tools().await.unwrap();
    assert!(
        tools.iter().any(|t| t.name.as_ref() == "search"),
        "{tools:?}"
    );
}

/// 回归（真机故障复现二，learn.microsoft.com 形态）：对 2026-07-28 探测回
/// 200 + SSE 流内嵌 -32601 错误。rmcp 的探测路径把它落成
/// `connection closed: discover response`——不是能自动回退的形态，
/// 靠 connect 层的无条件回退才连得上。
#[tokio::test]
async fn http_downstream_answering_discover_with_sse_error_falls_back() {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };
    async fn ms_learn_style_gate(
        request: axum::extract::Request,
        next: axum::middleware::Next,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        let modern = request
            .headers()
            .get("mcp-protocol-version")
            .and_then(|v| v.to_str().ok())
            == Some("2026-07-28");
        if modern {
            return (
                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                "event: message\ndata: {\"error\":{\"code\":-32601,\"message\":\"Method 'server/discover' is not available.\"},\"id\":1,\"jsonrpc\":\"2.0\"}\n\n",
            )
                .into_response();
        }
        next.run(request).await
    }

    let probe = Arc::new(SlowProbe::default());
    let service_probe = probe.clone();
    let service = StreamableHttpService::new(
        move || {
            Ok(HttpDownstream {
                slow_probe: service_probe.clone(),
            })
        },
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default().with_legacy_session_mode(true),
    );
    let router = axum::Router::new()
        .nest_service("/mcp", axum::routing::any_service(service))
        .layer(axum::middleware::from_fn(ms_learn_style_gate));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let spec = SpawnSpec::Http {
        url: format!("http://{addr}/mcp"),
        bearer: None,
        bearer_env: None,
        env: BTreeMap::new(),
        headers: BTreeMap::new(),
        oauth: None,
    };
    let handle = DownstreamHandle::connect(&spec, Duration::from_secs(10), &ProxyConfig::default())
        .await
        .expect("SSE 形态的探测拒绝也应回退 legacy 连接成功");
    let tools = handle.list_tools().await.unwrap();
    assert!(
        tools.iter().any(|t| t.name.as_ref() == "search"),
        "{tools:?}"
    );
}

/// stdio 版回退回归：粗鲁 legacy 服务器对 discover 回 -32600（Auto 不认），
/// 回退需要**重新 spawn 进程**再走 legacy initialize——覆盖 stdio 分支特有的
/// 重生路径。
#[tokio::test]
async fn stdio_downstream_rude_legacy_falls_back() {
    let spec = SpawnSpec::Stdio {
        command: env!("CARGO_BIN_EXE_mcpane-devserver").to_owned(),
        args: vec![],
        env: BTreeMap::from([("MCPANE_DEVSERVER_LEGACY_RUDE".to_owned(), "1".to_owned())]),
        cwd: None,
    };
    let handle = DownstreamHandle::connect(&spec, Duration::from_secs(10), &ProxyConfig::default())
        .await
        .expect("粗鲁 legacy stdio 服务器应经回退连接成功");
    let tools = handle.list_tools().await.unwrap();
    assert!(
        tools.iter().any(|t| t.name.as_ref() == "search"),
        "{tools:?}"
    );
}

/// 真机联网验证（需外网，CI 不跑）：`cargo test -- --ignored real_microsoft`。
#[tokio::test]
#[ignore = "依赖外网与 learn.microsoft.com 可用性"]
async fn real_microsoft_learn_connects() {
    let spec = SpawnSpec::Http {
        url: "https://learn.microsoft.com/api/mcp".into(),
        bearer: None,
        bearer_env: None,
        env: BTreeMap::new(),
        headers: BTreeMap::new(),
        oauth: None,
    };
    let handle = DownstreamHandle::connect(&spec, Duration::from_secs(30), &ProxyConfig::default())
        .await
        .expect("microsoft-learn 应经 legacy 回退连接成功");
    let tools = handle.list_tools().await.unwrap();
    assert!(!tools.is_empty(), "{tools:?}");
}

// ---------- 测试环境 ----------

struct Gate3 {
    state: Arc<AggregationState>,
    base_url: String,
    client: reqwest::Client,
    stdio_spec: SpawnSpec,
    /// filesystem 下游 slow 工具的观测探针（取消级联测试用）。
    fs_slow: Arc<SlowProbe>,
}

/// TestServer 控制命令的处理端：成功路径返回工具清单，失败路径返回错误说明。
#[tokio::test]
async fn control_test_server_reports_tools_and_errors() {
    // 成功：devserver stdio 配置 connect + tools/list。
    let ok = mcpane_hub::control::test_server(
        "stdio",
        Some(env!("CARGO_BIN_EXE_mcpane-devserver").to_owned()),
        vec![],
        BTreeMap::from([("MCPANE_DEVSERVER_NAME".to_owned(), "github".to_owned())]),
        None,
        None,
        None,
        None,
        BTreeMap::new(),
        &ProxyConfig::default(),
    )
    .await;
    assert_eq!(ok["ok"], json!(true), "devserver 应连通：{ok}");
    assert!(ok["toolCount"].as_u64().unwrap() >= 1);
    assert!(!ok["tools"].as_array().unwrap().is_empty());

    // 失败：命令不存在 → ok=false 且带错误信息。
    let bad = mcpane_hub::control::test_server(
        "stdio",
        Some("/nonexistent/mcpane-no-such-cmd".to_owned()),
        vec![],
        BTreeMap::new(),
        None,
        None,
        None,
        None,
        BTreeMap::new(),
        &ProxyConfig::default(),
    )
    .await;
    assert_eq!(bad["ok"], json!(false));
    assert!(!bad["error"].as_str().unwrap().is_empty());

    // 参数校验：缺 command / 未知 transport。
    let missing = mcpane_hub::control::test_server(
        "stdio",
        None,
        vec![],
        BTreeMap::new(),
        None,
        None,
        None,
        None,
        BTreeMap::new(),
        &ProxyConfig::default(),
    )
    .await;
    assert_eq!(missing["ok"], json!(false));
    let unknown = mcpane_hub::control::test_server(
        "carrier-pigeon",
        None,
        vec![],
        BTreeMap::new(),
        None,
        None,
        None,
        None,
        BTreeMap::new(),
        &ProxyConfig::default(),
    )
    .await;
    assert_eq!(unknown["ok"], json!(false));
}

fn devserver_stdio_spec(name: &str) -> SpawnSpec {
    SpawnSpec::Stdio {
        command: env!("CARGO_BIN_EXE_mcpane-devserver").to_owned(),
        args: vec![],
        env: BTreeMap::from([("MCPANE_DEVSERVER_NAME".to_owned(), name.to_owned())]),
        cwd: None,
    }
}

/// 只讲 2026-07-28 的下游：legacy initialize 会被它直接拒绝。
fn devserver_modern_only_spec(name: &str) -> SpawnSpec {
    SpawnSpec::Stdio {
        command: env!("CARGO_BIN_EXE_mcpane-devserver").to_owned(),
        args: vec![],
        env: BTreeMap::from([
            ("MCPANE_DEVSERVER_NAME".to_owned(), name.to_owned()),
            ("MCPANE_DEVSERVER_MODERN_ONLY".to_owned(), "1".to_owned()),
        ]),
        cwd: None,
    }
}

/// TASK-804：ClientLifecycleMode::Auto 先探 server/discover，因此只讲
/// 2026-07-28 的上游也能被聚合。不做这一步，这类上游我们根本连不上——
/// 而市面上已经有默认只讲新版的 SDK。
#[tokio::test]
async fn connects_to_a_downstream_that_only_speaks_modern_protocol() {
    let spec = devserver_modern_only_spec("modern");
    let handle = DownstreamHandle::connect(&spec, Duration::from_secs(10), &ProxyConfig::default())
        .await
        .expect("应能通过 server/discover 连上只讲 2026-07-28 的下游");

    let tools = handle.list_tools().await.expect("modern 下游 tools/list");
    assert!(
        tools.iter().any(|t| t.name == "search"),
        "没拿到 modern 下游的工具: {:?}",
        tools.iter().map(|t| &t.name).collect::<Vec<_>>()
    );

    let result = handle
        .call_tool(
            "search",
            Some(serde_json::Map::from_iter([(
                "query".to_owned(),
                Value::String("modern".to_owned()),
            )])),
        )
        .await
        .expect("modern 下游 tools/call");
    assert_ne!(result.is_error, Some(true), "modern 下游调用返回错误");
}

async fn setup() -> Gate3 {
    let registry = TokenRegistry::new();
    registry.register(
        CLAUDE_TOKEN,
        ClientIdentity::from_raw("client_claude_test".into(), "claude"),
    );
    registry.register(
        CODEX_TOKEN,
        ClientIdentity::from_raw("client_codex_test".into(), "codex"),
    );

    // 请求日志入内存 SQLite（TASK-407 验证用）。
    let pool = mcpane_storage::open_in_memory().await.unwrap();
    let state = Arc::new(AggregationState::new(Some(mcpane_storage::LogRepo::new(
        pool,
    ))));

    // stdio 下游（github）+ HTTP 下游（filesystem），两者都有同名 tool `search`。
    let stdio_spec = devserver_stdio_spec("github");
    let stdio = DownstreamHandle::connect(
        &stdio_spec,
        Duration::from_secs(10),
        &ProxyConfig::default(),
    )
    .await
    .unwrap();
    state.register_server("srv_github", "github", stdio_spec.clone(), stdio);

    let (http_url, fs_slow) = start_http_downstream().await;
    let http_spec = SpawnSpec::Http {
        url: http_url,
        bearer: None,
        bearer_env: None,
        env: BTreeMap::new(),
        headers: BTreeMap::new(),
        oauth: None,
    };
    let http =
        DownstreamHandle::connect(&http_spec, Duration::from_secs(10), &ProxyConfig::default())
            .await
            .unwrap();
    state.register_server("srv_fs", "filesystem", http_spec, http);

    state
        .rebuild_catalog("ws_default", &CatalogOverrides::default())
        .await
        .unwrap();

    let router = build_router(
        Arc::new(registry),
        state.clone(),
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

    Gate3 {
        state,
        base_url: format!("http://{addr}/mcp"),
        client: reqwest::Client::new(),
        stdio_spec,
        fs_slow,
    }
}

impl Gate3 {
    async fn rpc(&self, token: &str, session: Option<&str>, body: Value) -> (u16, Option<Value>) {
        let mut request = self
            .client
            .post(&self.base_url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .header("MCP-Protocol-Version", "2025-11-25")
            .json(&body);
        if let Some(session) = session {
            request = request.header("Mcp-Session-Id", session);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let text = response.text().await.unwrap();
        if !(200..300).contains(&status) || text.is_empty() {
            return (status, None);
        }
        let value = if content_type.starts_with("text/event-stream") {
            text.lines()
                .filter_map(|l| l.strip_prefix("data: "))
                .filter_map(|d| serde_json::from_str::<Value>(d).ok())
                .find(|v| v.get("id").is_some())
        } else {
            serde_json::from_str(&text).ok()
        };
        (status, value)
    }

    /// 2026-07-28 请求：无 Session、无 initialize，协议版本与客户端能力
    /// 每请求自带在 `_meta` 里（SEP-2575）。
    async fn rpc_modern(&self, token: &str, mut body: Value) -> (u16, Option<Value>) {
        let method = body["method"].as_str().unwrap_or_default().to_owned();
        if !body["params"].is_object() {
            body["params"] = json!({});
        }
        body["params"]["_meta"] = json!({
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
            "io.modelcontextprotocol/clientInfo": { "name": "gate3", "version": "0" }
        });
        let mut request = self
            .client
            .post(&self.base_url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .header("MCP-Protocol-Version", "2026-07-28")
            .header("Mcp-Method", method);
        // SEP-2243：带目标名的方法必须把它也放进 Mcp-Name 头，且与 params 一致
        // （tools/call 等取 name；resources/read 等取 uri）。
        if let Some(name) = body["params"]["name"]
            .as_str()
            .or_else(|| body["params"]["uri"].as_str())
        {
            request = request.header("Mcp-Name", name.to_owned());
        }
        let response = request.json(&body).send().await.unwrap();
        let status = response.status().as_u16();
        let text = response.text().await.unwrap();
        if !(200..300).contains(&status) || text.is_empty() {
            return (status, None);
        }
        (status, serde_json::from_str(&text).ok())
    }

    async fn initialize(&self, token: &str) -> String {
        self.initialize_with(token, json!({})).await
    }

    async fn initialize_with(&self, token: &str, capabilities: Value) -> String {
        let mut request = self
            .client
            .post(&self.base_url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .json(&json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": "2025-11-25", "capabilities": capabilities,
                    "clientInfo": { "name": "gate3", "version": "0" } }
            }));
        request = request.header("MCP-Protocol-Version", "2025-11-25");
        let response = request.send().await.unwrap();
        assert!(response.status().is_success());
        let session = response
            .headers()
            .get("mcp-session-id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let (status, _) = self
            .rpc(
                token,
                Some(&session),
                json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            )
            .await;
        assert_eq!(status, 202);
        session
    }

    async fn call(&self, token: &str, session: &str, id: i64, name: &str, args: Value) -> Value {
        let (status, value) = self
            .rpc(
                token,
                Some(session),
                json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
                    "params": { "name": name, "arguments": args } }),
            )
            .await;
        assert_eq!(status, 200, "tools/call {name}");
        value.unwrap()
    }

    /// tools/call 期间应答服务器发来的 elicitation/create，返回最终调用结果。
    /// 真实 legacy 客户端保持一条常驻 GET SSE 流接收服务器发起的请求——这里
    /// 同样开一条，POST 响应流与 GET 流上出现的确认请求都会被应答。
    async fn call_answering_elicitation(
        &self,
        token: &str,
        session: &str,
        id: i64,
        name: &str,
        elicit_result: Value,
    ) -> Value {
        let listener = {
            let client = self.client.clone();
            let base = self.base_url.clone();
            let token = token.to_owned();
            let session_id = session.to_owned();
            let respond = elicit_result.clone();
            tokio::spawn(async move {
                let mut response = client
                    .get(&base)
                    .header("Authorization", format!("Bearer {token}"))
                    .header("Accept", "text/event-stream")
                    .header("MCP-Protocol-Version", "2025-11-25")
                    .header("Mcp-Session-Id", &session_id)
                    .send()
                    .await
                    .unwrap();
                let mut buffer = String::new();
                loop {
                    let Some(chunk) = response.chunk().await.ok().flatten() else {
                        return;
                    };
                    buffer.push_str(&String::from_utf8_lossy(&chunk));
                    while let Some(pos) = buffer.find("\n\n") {
                        let event: String = buffer.drain(..pos + 2).collect();
                        let Some(data) = event.lines().find_map(|l| l.strip_prefix("data: "))
                        else {
                            continue;
                        };
                        let Ok(message) = serde_json::from_str::<Value>(data) else {
                            continue;
                        };
                        if message["method"] == "elicitation/create" {
                            let status = client
                                .post(&base)
                                .header("Authorization", format!("Bearer {token}"))
                                .header("Accept", "application/json, text/event-stream")
                                .header("Content-Type", "application/json")
                                .header("MCP-Protocol-Version", "2025-11-25")
                                .header("Mcp-Session-Id", &session_id)
                                .json(&json!({ "jsonrpc": "2.0", "id": message["id"],
                                    "result": respond }))
                                .send()
                                .await
                                .unwrap()
                                .status();
                            assert_eq!(status.as_u16(), 202, "elicitation response (GET stream)");
                        }
                    }
                }
            })
        };
        let result = self
            .call_streaming_answering(token, session, id, name, elicit_result)
            .await;
        listener.abort();
        result
    }

    async fn call_streaming_answering(
        &self,
        token: &str,
        session: &str,
        id: i64,
        name: &str,
        elicit_result: Value,
    ) -> Value {
        let mut response = self
            .client
            .post(&self.base_url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .header("MCP-Protocol-Version", "2025-11-25")
            .header("Mcp-Session-Id", session)
            .json(&json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": { "name": name, "arguments": {} } }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200, "tools/call {name}");
        let mut buffer = String::new();
        loop {
            let chunk = response
                .chunk()
                .await
                .unwrap()
                .expect("stream ended before final result");
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buffer.find("\n\n") {
                let event: String = buffer.drain(..pos + 2).collect();
                let Some(data) = event.lines().find_map(|l| l.strip_prefix("data: ")) else {
                    continue;
                };
                let Ok(message) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                if message["method"] == "elicitation/create" {
                    let (status, _) = self
                        .rpc(
                            token,
                            Some(session),
                            json!({ "jsonrpc": "2.0", "id": message["id"],
                                "result": elicit_result }),
                        )
                        .await;
                    assert_eq!(status, 202, "elicitation response");
                } else if message["id"] == json!(id) {
                    return message;
                }
            }
        }
    }
}

// ---------- GATE-3 ----------

/// 前提验证：上游未声明 elicitation 能力时，下游的旧式确认（elicitation/create）
/// 不转发，按拒绝应答——下游工具看到 decline 而非悬挂或报错。
#[tokio::test]
async fn legacy_elicitation_declined_without_upstream_capability() {
    let hub = setup().await;
    let session = hub.initialize(CLAUDE_TOKEN).await;
    let result = hub
        .call(CLAUDE_TOKEN, &session, 70, "filesystem__confirm", json!({}))
        .await;
    assert_eq!(
        result["result"]["structuredContent"]["action"], "decline",
        "{result}"
    );
}

/// 桥接：上游声明 elicitation 能力后，legacy 下游的旧式确认经 Hub 转发上游，
/// 上游 accept 的应答原样回到下游工具手里。
#[tokio::test]
async fn legacy_elicitation_bridged_to_upstream() {
    let hub = setup().await;
    let session = hub
        .initialize_with(CLAUDE_TOKEN, json!({ "elicitation": {} }))
        .await;
    let result = hub
        .call_answering_elicitation(
            CLAUDE_TOKEN,
            &session,
            71,
            "filesystem__confirm",
            json!({ "action": "accept", "content": { "reason": "ok" } }),
        )
        .await;
    assert_eq!(
        result["result"]["structuredContent"]["action"], "accept",
        "{result}"
    );
}

/// completions 转发：prompt 参数补全按暴露名路由到下游（下游收到原名），
/// 未知引用报错。
#[tokio::test]
async fn completion_routed_to_downstream() {
    let hub = setup().await;
    let session = hub.initialize(CLAUDE_TOKEN).await;

    let (status, response) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 95, "method": "completion/complete",
                "params": { "ref": { "type": "ref/prompt", "name": "filesystem__review" },
                    "argument": { "name": "path", "value": "src" } } }),
        )
        .await;
    assert_eq!(status, 200);
    let response = response.unwrap();
    assert_eq!(
        response["result"]["completion"]["values"][0], "src-suggestion",
        "{response}"
    );

    let (_, response) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 96, "method": "completion/complete",
                "params": { "ref": { "type": "ref/prompt", "name": "nope__missing" },
                    "argument": { "name": "path", "value": "x" } } }),
        )
        .await;
    assert!(
        response.unwrap().get("error").is_some(),
        "未知 prompt 引用应报错"
    );
}

/// 取消级联：上游对在飞 tools/call 发 notifications/cancelled，下游同步
/// 收到取消（ct 触发或 future 被终止均可），不再白跑。
#[tokio::test]
async fn upstream_cancel_cascades_to_downstream() {
    use std::sync::atomic::Ordering;
    let hub = setup().await;
    let session = hub.initialize(CLAUDE_TOKEN).await;

    let call = {
        let client = hub.client.clone();
        let base = hub.base_url.clone();
        let session = session.clone();
        tokio::spawn(async move {
            let _ = client
                .post(&base)
                .header("Authorization", format!("Bearer {CLAUDE_TOKEN}"))
                .header("Accept", "application/json, text/event-stream")
                .header("Content-Type", "application/json")
                .header("MCP-Protocol-Version", "2025-11-25")
                .header("Mcp-Session-Id", &session)
                .json(&json!({ "jsonrpc": "2.0", "id": 90, "method": "tools/call",
                    "params": { "name": "filesystem__slow", "arguments": {} } }))
                .send()
                .await;
        })
    };

    // 等到下游 slow 真正开跑再取消，避免取消跑在调用前面。
    let mut started = false;
    for _ in 0..100 {
        if hub.fs_slow.started.load(Ordering::SeqCst) {
            started = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(started, "slow 工具应已在飞");

    let (status, _) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "method": "notifications/cancelled",
                "params": { "requestId": 90, "reason": "user" } }),
        )
        .await;
    assert_eq!(status, 202);

    let mut cancelled = false;
    for _ in 0..100 {
        if hub.fs_slow.cancelled.load(Ordering::SeqCst) {
            cancelled = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    call.abort();
    assert!(cancelled, "下游应在上游取消后收到 notifications/cancelled");
}

/// 取消兜底：Hub 的调用 future 被直接终止（2026-07-28 上游断流即取消的
/// 形态，rmcp 会 abort 处理任务）时，drop 守卫仍向下游发 cancelled。
#[tokio::test]
async fn dropped_call_still_cancels_downstream() {
    use std::sync::atomic::Ordering;
    let hub = setup().await;
    let handle = hub.state.downstream("srv_fs").unwrap();
    let call = tokio::spawn(async move {
        let _ = handle
            .call_tool_once(
                "slow",
                None,
                None,
                None,
                tokio_util::sync::CancellationToken::new(),
            )
            .await;
    });

    let mut started = false;
    for _ in 0..100 {
        if hub.fs_slow.started.load(Ordering::SeqCst) {
            started = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(started, "slow 工具应已在飞");

    call.abort();
    let mut cancelled = false;
    for _ in 0..100 {
        if hub.fs_slow.cancelled.load(Ordering::SeqCst) {
            cancelled = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(cancelled, "调用 future 被 drop 后下游仍应收到取消");
}

/// 分层验证：下游发出的 notifications/progress 能到达 Hub 的 ProgressRelay
/// （不含上游转发段）。
#[tokio::test]
async fn downstream_progress_reaches_relay() {
    let hub = setup().await;
    let handle = hub.state.downstream("srv_fs").unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let _guard = handle.progress_relay().register(Arc::new(move |params| {
        let _ = tx.send(params);
    }));
    let response = handle
        .call_tool_once(
            "report",
            None,
            None,
            None,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    let rmcp::model::CallToolResponse::Complete(result) = response else {
        panic!("report 应直接完成");
    };
    assert_eq!(
        result.structured_content.as_ref().unwrap()["action"],
        "done",
        "{result:?}"
    );
    let forwarded = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("5s 内应收到进度")
        .expect("通道不应关闭");
    assert_eq!(forwarded.progress, 0.5);
    assert_eq!(forwarded.message.as_deref(), Some("halfway"));
}

/// progress 转发：上游带 progressToken 的调用，下游报的进度经 Hub 换回
/// 上游 token 转发（可能走 POST 响应流或 GET 常驻流，两条都收）。
#[tokio::test]
async fn downstream_progress_forwarded_to_upstream() {
    let hub = setup().await;
    let session = hub.initialize(CLAUDE_TOKEN).await;

    let seen = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let listener = {
        let client = hub.client.clone();
        let base = hub.base_url.clone();
        let session_id = session.clone();
        let seen = seen.clone();
        tokio::spawn(async move {
            let mut response = client
                .get(&base)
                .header("Authorization", format!("Bearer {CLAUDE_TOKEN}"))
                .header("Accept", "text/event-stream")
                .header("MCP-Protocol-Version", "2025-11-25")
                .header("Mcp-Session-Id", &session_id)
                .send()
                .await
                .unwrap();
            let mut buffer = String::new();
            loop {
                let Some(chunk) = response.chunk().await.ok().flatten() else {
                    return;
                };
                buffer.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(pos) = buffer.find("\n\n") {
                    let event: String = buffer.drain(..pos + 2).collect();
                    if let Some(data) = event.lines().find_map(|l| l.strip_prefix("data: "))
                        && let Ok(message) = serde_json::from_str::<Value>(data)
                        && message["method"] == "notifications/progress"
                    {
                        seen.lock().unwrap().push(message);
                    }
                }
            }
        })
    };

    let mut response = hub
        .client
        .post(&hub.base_url)
        .header("Authorization", format!("Bearer {CLAUDE_TOKEN}"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("MCP-Protocol-Version", "2025-11-25")
        .header("Mcp-Session-Id", &session)
        .json(&json!({ "jsonrpc": "2.0", "id": 80, "method": "tools/call",
            "params": { "name": "filesystem__report", "arguments": {},
                "_meta": { "progressToken": 7 } } }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let mut buffer = String::new();
    let mut result = None;
    while result.is_none() {
        let chunk = response
            .chunk()
            .await
            .unwrap()
            .expect("stream ended before final result");
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(pos) = buffer.find("\n\n") {
            let event: String = buffer.drain(..pos + 2).collect();
            let Some(data) = event.lines().find_map(|l| l.strip_prefix("data: ")) else {
                continue;
            };
            let Ok(message) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            if message["method"] == "notifications/progress" {
                seen.lock().unwrap().push(message);
            } else if message["id"] == json!(80) {
                result = Some(message);
            }
        }
    }
    listener.abort();

    let result = result.unwrap();
    assert_eq!(
        result["result"]["structuredContent"]["action"], "done",
        "{result}"
    );
    let progress = seen.lock().unwrap();
    assert!(
        progress.iter().any(|p| p["params"]["progressToken"] == 7
            && p["params"]["progress"] == 0.5
            && p["params"]["message"] == "halfway"),
        "上游应收到换回自身 token 的进度通知：{progress:?}"
    );
}

/// instructions 能力总览：initialize 不再回固定文案，而是聚合各下游的
/// slug/工具数/自述；发现模式下附 meta 工具使用提示。
#[tokio::test]
async fn initialize_instructions_aggregate_downstreams() {
    let hub = setup().await;

    let init_body = |id: i64| {
        json!({ "jsonrpc": "2.0", "id": id, "method": "initialize",
            "params": { "protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": { "name": "gate3", "version": "0" } } })
    };
    let (status, response) = hub.rpc(CLAUDE_TOKEN, None, init_body(60)).await;
    assert_eq!(status, 200);
    let instructions = response.unwrap()["result"]["instructions"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(instructions.contains("github"), "{instructions}");
    assert!(instructions.contains("filesystem"), "{instructions}");
    assert!(instructions.contains("HTTP 文件检索下游"), "{instructions}");
    assert!(
        !instructions.contains("mcpane__tool_discovery"),
        "{instructions}"
    );
    // 平铺形态不附工具目录（工具本就全量在 tools/list 里）。
    assert!(!instructions.contains("github__search"), "{instructions}");
    // 主动性教义两种形态都注入：让模型「想起来用」，不只是「知道有」。
    assert!(instructions.contains("使用准则"), "{instructions}");
    assert!(instructions.contains("优先调用"), "{instructions}");
    // 可观测性：连接的协商协议版本被记录（控制面 Status 暴露给桌面端）。
    assert_eq!(
        hub.state
            .connection_protocols_snapshot()
            .get("client_claude_test")
            .map(String::as_str),
        Some("2025-11-25")
    );

    // 开启发现模式后，新会话的总览要附 meta 工具使用提示与全量目录。
    // ADR-020：auto 模式下 claude 连接固定走平铺，测网关路径须显式设 gateway。
    hub.state
        .rebuild_catalog_scoped("ws_default", &CatalogOverrides::default(), None, true)
        .await
        .unwrap();
    hub.state
        .set_connection_exposures(std::collections::HashMap::from([(
            "client_claude_test".to_owned(),
            mcpane_hub::state::ExposureMode::Gateway,
        )]));
    let (_, response) = hub.rpc(CLAUDE_TOKEN, None, init_body(61)).await;
    let instructions = response.unwrap()["result"]["instructions"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        instructions.contains("mcpane__tool_discovery"),
        "{instructions}"
    );
    // 网关形态目录直给：全量工具名 + describe 工作流提示（ADR-020）。
    assert!(instructions.contains("github__search"), "{instructions}");
    assert!(
        instructions.contains("filesystem__search"),
        "{instructions}"
    );
    assert!(
        instructions.contains("mcpane__tool_describe"),
        "{instructions}"
    );
    assert!(instructions.contains("使用准则"), "{instructions}");

    // 用户自定义使用准则注入（ADR-020 主动性轴）：配置后随 instructions 下发。
    hub.state
        .set_workspace_guidance(std::collections::HashMap::from([(
            "ws_default".to_owned(),
            "代码搜索一律先用 fast_context_search".to_owned(),
        )]));
    let (_, response) = hub.rpc(CLAUDE_TOKEN, None, init_body(62)).await;
    let instructions = response.unwrap()["result"]["instructions"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        instructions.contains("代码搜索一律先用 fast_context_search"),
        "{instructions}"
    );
    assert!(instructions.contains("工作区使用准则"), "{instructions}");
}

#[tokio::test]
async fn gate3_aggregation_end_to_end() {
    let hub = setup().await;

    // 1+2+3. stdio + HTTP 双下游已聚合：tools/list 含命名空间隔离的同名 Tool。
    let claude = hub.initialize(CLAUDE_TOKEN).await;
    let (_, tools) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&claude),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        )
        .await;
    let tools = tools.unwrap();
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"mcpane__echo"), "{names:?}");
    assert!(names.contains(&"github__search"), "{names:?}");
    assert!(names.contains(&"filesystem__search"), "{names:?}");
    assert!(names.contains(&"github__crash"), "{names:?}");

    // 6. 同名 Tool 路由正确 + 5. 双客户端并发调用不同下游。
    let codex = hub.initialize(CODEX_TOKEN).await;
    let (claude_result, codex_result) = tokio::join!(
        hub.call(
            CLAUDE_TOKEN,
            &claude,
            3,
            "github__search",
            json!({ "query": "q1" })
        ),
        hub.call(
            CODEX_TOKEN,
            &codex,
            3,
            "filesystem__search",
            json!({ "query": "q2" })
        ),
    );
    assert_eq!(
        claude_result["result"]["structuredContent"]["server"], "github",
        "{claude_result}"
    );
    assert_eq!(
        codex_result["result"]["structuredContent"]["server"], "filesystem-http",
        "{codex_result}"
    );

    // 7. 崩溃隔离：github 下游进程退出，filesystem 不受影响。
    let (status, crash_result) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&claude),
            json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call",
                "params": { "name": "github__crash", "arguments": {} } }),
        )
        .await;
    // 下游死亡表现为 JSON-RPC error 或 isError，绝不是 Hub 挂掉。
    let crashed = status != 200
        || crash_result
            .as_ref()
            .is_some_and(|v| v.get("error").is_some() || v["result"]["isError"] == json!(true));
    assert!(crashed, "crash 调用应表现为错误: {status} {crash_result:?}");

    let alive = hub
        .call(
            CODEX_TOKEN,
            &codex,
            5,
            "filesystem__search",
            json!({ "query": "still-alive" }),
        )
        .await;
    assert_eq!(
        alive["result"]["structuredContent"]["server"], "filesystem-http",
        "单 Server 崩溃不得影响其他下游"
    );

    // github 再调用 → 明确错误（进程已死），Hub 本体依然健康。
    let (status, dead) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&claude),
            json!({ "jsonrpc": "2.0", "id": 6, "method": "tools/call",
                "params": { "name": "github__search", "arguments": { "query": "x" } } }),
        )
        .await;
    let dead_error = status != 200 || dead.as_ref().is_some_and(|v| v.get("error").is_some());
    assert!(dead_error, "死亡下游必须返回明确错误: {status} {dead:?}");

    // 9. 恢复：重连 stdio 下游并重建 Catalog，调用恢复。
    let restarted = DownstreamHandle::connect(
        &hub.stdio_spec,
        Duration::from_secs(10),
        &ProxyConfig::default(),
    )
    .await
    .unwrap();
    hub.state
        .register_server("srv_github", "github", hub.stdio_spec.clone(), restarted);
    hub.state
        .rebuild_catalog("ws_default", &CatalogOverrides::default())
        .await
        .unwrap();
    // 新 Session pin 新 Revision。
    let claude2 = hub.initialize(CLAUDE_TOKEN).await;
    let recovered = hub
        .call(
            CLAUDE_TOKEN,
            &claude2,
            7,
            "github__search",
            json!({ "query": "recovered" }),
        )
        .await;
    assert_eq!(recovered["result"]["structuredContent"]["server"], "github");

    // 8. 权限隔离：禁用 filesystem search 后——
    //    新 Catalog 不再暴露；旧 Session（已 pin）调用被二次校验拦截。
    let mut overrides: mcpane_hub_catalog::ToolOverrides = BTreeMap::new();
    overrides.insert(
        ("srv_fs".into(), "search".into()),
        mcpane_hub_catalog::ItemOverride::disabled(),
    );
    hub.state
        .rebuild_catalog("ws_default", &CatalogOverrides::from_tools(overrides))
        .await
        .unwrap();

    let claude3 = hub.initialize(CLAUDE_TOKEN).await;
    let (_, tools) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&claude3),
            json!({ "jsonrpc": "2.0", "id": 8, "method": "tools/list" }),
        )
        .await;
    let names: Vec<String> = tools.unwrap()["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        !names.contains(&"filesystem__search".to_owned()),
        "新 Session 的 tools/list 只返回允许项: {names:?}"
    );

    // 旧 Session（codex，pin 的是禁用前的 Revision）直接调用 → 明确拒绝。
    let (status, denied) = hub
        .rpc(
            CODEX_TOKEN,
            Some(&codex),
            json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call",
                "params": { "name": "filesystem__search", "arguments": { "query": "sneak" } } }),
        )
        .await;
    let denied_error = status != 200 || denied.as_ref().is_some_and(|v| v.get("error").is_some());
    assert!(
        denied_error,
        "禁用 Tool 的直接调用必须明确报错: {status} {denied:?}"
    );

    // 10. 请求日志：完整入库且不含任何 Token。
    // 带 secret 字段的调用：摘要必须脱敏（token 键 → [REDACTED]）。
    let _ = hub
        .call(
            CLAUDE_TOKEN,
            &claude,
            10,
            "github__search",
            json!({ "query": "q-secret-scan", "token": CLAUDE_TOKEN }),
        )
        .await;
    // 缺必填参数：下游以 isError 返回，日志必须记失败而不是绿灯。
    let missing = hub
        .call(CLAUDE_TOKEN, &claude, 11, "github__search", json!({}))
        .await;
    assert_eq!(missing["result"]["isError"], true, "{missing}");
    let _ = hub
        .call(
            CLAUDE_TOKEN,
            &claude,
            12,
            "mcpane__echo",
            json!({ "message": "log-builtin" }),
        )
        .await;

    let logs = hub.state.logs.as_ref().unwrap().recent(100).await.unwrap();
    assert!(!logs.is_empty(), "下游调用必须留下请求日志");
    let dump = serde_json::to_string(&logs).unwrap();
    assert!(
        !dump.contains(CLAUDE_TOKEN) && !dump.contains(CODEX_TOKEN),
        "列表日志不得含 Token"
    );
    assert!(
        logs.iter()
            .any(|l| l.exposed_name.as_deref() == Some("github__search"))
    );
    assert!(
        logs.iter()
            .any(|l| l.client_connection_id.as_deref() == Some("client_claude_test")),
        "日志应记录客户端连接 ID"
    );
    assert!(
        logs.iter()
            .any(|l| l.exposed_name.as_deref() == Some("mcpane__echo") && l.success),
        "内建工具调用也应入日志"
    );
    assert!(
        logs.iter().any(|l| {
            l.exposed_name.as_deref() == Some("github__search")
                && !l.success
                && l.error_category.as_deref() == Some("tool")
        }),
        "下游 isError 必须记为失败: {logs:?}"
    );
    assert!(
        logs.iter().any(|l| {
            l.exposed_name.as_deref() == Some("filesystem__search")
                && !l.success
                && l.error_category.as_deref() == Some("policy")
        }),
        "策略拒绝必须入失败日志: {logs:?}"
    );

    let repo = hub.state.logs.as_ref().unwrap();
    let mut payload_dump = String::new();
    let mut saw_search_query = false;
    let mut saw_redacted_secret = false;
    for entry in &logs {
        let detail = repo.get(&entry.id).await.unwrap();
        payload_dump.push_str(&serde_json::to_string(&detail).unwrap());
        if detail.exposed_name.as_deref() == Some("github__search")
            && let Some(req) = &detail.request_summary
            && req.contains("q-secret-scan")
        {
            saw_search_query = true;
            if req.contains("[REDACTED]") {
                saw_redacted_secret = true;
            }
        }
    }
    assert!(
        !payload_dump.contains(CLAUDE_TOKEN) && !payload_dump.contains(CODEX_TOKEN),
        "详情摘要不得含 Token"
    );
    assert!(saw_search_query, "github__search 摘要应含 query");
    assert!(saw_redacted_secret, "token 字段必须脱敏");
}

/// Hub 重启恢复：下游定义持久化在 SQLite，重启后可据此重建聚合状态。
#[tokio::test]
async fn hub_restart_recovers_downstreams_from_sqlite() {
    let pool = mcpane_storage::open_in_memory().await.unwrap();
    let servers = mcpane_storage::ServerRepo::new(pool.clone());

    // "重启前"：持久化下游定义。
    servers
        .upsert(mcpane_storage::McpServerDef {
            id: "srv_github".into(),
            slug: "github".into(),
            name: "GitHub Dev".into(),
            transport: mcpane_storage::ServerTransport::Stdio,
            command: Some(env!("CARGO_BIN_EXE_mcpane-devserver").to_owned()),
            args: vec![],
            env: BTreeMap::from([("MCPANE_DEVSERVER_NAME".to_owned(), "github".to_owned())]),
            cwd: None,
            url: None,
            bearer: None,
            bearer_env: None,
            headers: BTreeMap::new(),
            // 本例测的是「重启后从 SQLite 恢复下游」，与 policy 无关；用
            // shared 是因为它是唯一已实现的一档，写 per-client-connection
            // 会读成「按连接隔离已通过验证」，而 Hub 侧根本不读这个字段。
            runtime_policy: "shared".into(),
            enabled: true,
        })
        .await
        .unwrap();

    // "重启后"：从 SQLite 读回定义并重建状态。
    let defs = servers.list().await.unwrap();
    assert_eq!(defs.len(), 1);
    let def = &defs[0];
    let spec = SpawnSpec::Stdio {
        command: def.command.clone().unwrap(),
        args: def.args.clone(),
        env: def.env.clone(),
        cwd: def.cwd.clone(),
    };
    let state = AggregationState::new(None);
    let handle = DownstreamHandle::connect(&spec, Duration::from_secs(10), &ProxyConfig::default())
        .await
        .unwrap();
    state.register_server(&def.id, &def.slug, spec, handle);
    let revision = state
        .rebuild_catalog("ws_default", &CatalogOverrides::default())
        .await
        .unwrap();
    assert!(
        revision
            .tools
            .iter()
            .any(|t| t.exposed_name == "github__search"),
        "重启后 Catalog 恢复"
    );
}

/// 多 Workspace 隔离：不同连接映射到不同 Workspace，
/// tools/list 与 tools/call 均按各自 Catalog 隔离。
#[tokio::test]
async fn workspaces_isolate_tool_visibility_per_connection() {
    let hub = setup().await;

    // ws_limited 只包含 stdio 下游（srv_github）。
    let members: std::collections::HashSet<String> =
        std::collections::HashSet::from(["srv_github".to_owned()]);
    hub.state
        .rebuild_catalog_scoped(
            "ws_limited",
            &CatalogOverrides::default(),
            Some(&members),
            false,
        )
        .await
        .unwrap();
    // codex 连接映射到受限 Workspace；claude 走默认（全量）。
    hub.state
        .set_connection_workspaces(std::collections::HashMap::from([(
            "client_codex_test".to_owned(),
            "ws_limited".to_owned(),
        )]));

    let claude = hub.initialize(CLAUDE_TOKEN).await;
    let codex = hub.initialize(CODEX_TOKEN).await;

    let list_names = |value: Value| -> Vec<String> {
        value["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect()
    };

    let (_, claude_tools) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&claude),
            json!({ "jsonrpc": "2.0", "id": 20, "method": "tools/list" }),
        )
        .await;
    let claude_names = list_names(claude_tools.unwrap());
    assert!(claude_names.contains(&"filesystem__search".to_owned()));
    assert!(claude_names.contains(&"github__search".to_owned()));

    let (_, codex_tools) = hub
        .rpc(
            CODEX_TOKEN,
            Some(&codex),
            json!({ "jsonrpc": "2.0", "id": 21, "method": "tools/list" }),
        )
        .await;
    let codex_names = list_names(codex_tools.unwrap());
    assert!(codex_names.contains(&"github__search".to_owned()));
    assert!(
        !codex_names.contains(&"filesystem__search".to_owned()),
        "受限 Workspace 不得暴露非成员 Server 的工具: {codex_names:?}"
    );

    // 受限连接直接调用不可见工具 → 明确拒绝。
    let (status, denied) = hub
        .rpc(
            CODEX_TOKEN,
            Some(&codex),
            json!({ "jsonrpc": "2.0", "id": 22, "method": "tools/call",
                "params": { "name": "filesystem__search", "arguments": { "query": "sneak" } } }),
        )
        .await;
    let denied_error = status != 200 || denied.as_ref().is_some_and(|v| v.get("error").is_some());
    assert!(
        denied_error,
        "跨 Workspace 调用必须拒绝: {status} {denied:?}"
    );

    // 受限 Workspace 内正常调用不受影响。
    let ok = hub
        .call(
            CODEX_TOKEN,
            &codex,
            23,
            "github__search",
            json!({ "query": "scoped" }),
        )
        .await;
    assert_eq!(ok["result"]["structuredContent"]["server"], "github");
}

/// ADR-004 Session-Pinned：Session 的 Workspace 在首个请求后固定；中途
/// `set-connection-workspace` 改绑**不**改变已建 Session 的路由目标——否则
/// 客户端手里的工具名（旧 Workspace）会被路由到新 Workspace 的 Revision。
#[tokio::test]
async fn session_workspace_is_pinned_against_mid_session_switch() {
    let hub = setup().await;

    let members: std::collections::HashSet<String> =
        std::collections::HashSet::from(["srv_github".to_owned()]);
    hub.state
        .rebuild_catalog_scoped(
            "ws_limited",
            &CatalogOverrides::default(),
            Some(&members),
            false,
        )
        .await
        .unwrap();
    // codex 起初绑定受限 Workspace。
    hub.state
        .set_connection_workspaces(std::collections::HashMap::from([(
            "client_codex_test".to_owned(),
            "ws_limited".to_owned(),
        )]));

    let codex = hub.initialize(CODEX_TOKEN).await;
    let list_names = |value: Value| -> Vec<String> {
        value["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect()
    };

    // 首个请求把 Session pin 到 ws_limited（只见 github）。
    let (_, before) = hub
        .rpc(
            CODEX_TOKEN,
            Some(&codex),
            json!({ "jsonrpc": "2.0", "id": 40, "method": "tools/list" }),
        )
        .await;
    let before_names = list_names(before.unwrap());
    assert!(before_names.contains(&"github__search".to_owned()));
    assert!(!before_names.contains(&"filesystem__search".to_owned()));

    // 中途把该连接改绑到默认（全量）Workspace。
    hub.state
        .set_connection_workspaces(std::collections::HashMap::from([(
            "client_codex_test".to_owned(),
            "ws_default".to_owned(),
        )]));

    // 同一 Session 再次 tools/list：仍只应看到 pin 时 ws_limited 的工具，
    // 不得因中途改绑而冒出 ws_default 的 filesystem（ADR-004 §4）。
    let (_, after) = hub
        .rpc(
            CODEX_TOKEN,
            Some(&codex),
            json!({ "jsonrpc": "2.0", "id": 41, "method": "tools/list" }),
        )
        .await;
    let after_names = list_names(after.unwrap());
    assert!(after_names.contains(&"github__search".to_owned()));
    assert!(
        !after_names.contains(&"filesystem__search".to_owned()),
        "中途改绑不得改变已 pin Session 的可见工具: {after_names:?}"
    );
}

/// 上一个测试的 2026-07-28 对照：新协议没有 Session 可 pin，改绑对**下一个
/// 请求**即生效。这不是退化——ADR-004 的 pin 是为了让客户端手里的工具名不
/// 失效，新协议改由 `subscriptions/listen`（TASK-803）+ `ttlMs` 缓存契约
/// （本 TASK）承担同一职责，且是协议级可靠通道而非 best-effort（ADR-019）。
#[tokio::test]
async fn modern_protocol_reflects_workspace_rebinding_on_next_request() {
    let hub = setup().await;

    let members: std::collections::HashSet<String> =
        std::collections::HashSet::from(["srv_github".to_owned()]);
    hub.state
        .rebuild_catalog_scoped(
            "ws_limited",
            &CatalogOverrides::default(),
            Some(&members),
            false,
        )
        .await
        .unwrap();
    hub.state
        .set_connection_workspaces(std::collections::HashMap::from([(
            "client_codex_test".to_owned(),
            "ws_limited".to_owned(),
        )]));

    let list_names = |value: Value| -> Vec<String> {
        value["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect()
    };

    let (_, before) = hub
        .rpc_modern(
            CODEX_TOKEN,
            json!({ "jsonrpc": "2.0", "id": 40, "method": "tools/list" }),
        )
        .await;
    let before_names = list_names(before.unwrap());
    assert!(before_names.contains(&"github__search".to_owned()));
    assert!(!before_names.contains(&"filesystem__search".to_owned()));

    hub.state
        .set_connection_workspaces(std::collections::HashMap::from([(
            "client_codex_test".to_owned(),
            "ws_default".to_owned(),
        )]));

    let (_, after) = hub
        .rpc_modern(
            CODEX_TOKEN,
            json!({ "jsonrpc": "2.0", "id": 41, "method": "tools/list" }),
        )
        .await;
    let after_names = list_names(after.unwrap());
    assert!(
        after_names.contains(&"filesystem__search".to_owned()),
        "新协议无 Session 可 pin，改绑应对下一个请求即生效: {after_names:?}"
    );
}

/// 安全回归：Session 的 pin 必须绑定发起首个请求的连接身份，不能被借用了
/// 同一个 `Mcp-Session-Id` 的另一个合法 Token 冒用。
///
/// 此前 pin 只存 Workspace 字符串、不比对身份：rmcp 的 `StreamableHttpService`
/// 纯按 `Mcp-Session-Id` 查找/复用已有 Session worker（核实过 2.2.0 源码，
/// `handle_post` 与 `LocalSessionManager` 均无鉴权/身份校验），任何持有
/// *别的*合法 Token、又拿到了这个 Session-Id 的连接（Session-Id 会写进
/// `hub_request_logs`，同机有文件读权限的进程都能捞到）就能借用这个 Session
/// 已 pin 的 Workspace，越过自己真正被分配的 Workspace/访问控制边界——
/// 这不是 ADR-004 已覆盖的"中途改绑"场景，是"从一开始就不是同一个连接"。
#[tokio::test]
async fn session_rejects_requests_from_a_different_token_than_the_one_that_pinned_it() {
    let hub = setup().await;

    // claude 建立 Session；首个 tools/list 把它 pin 到 client_claude_test
    // 这个身份 + 其对应的 Workspace。
    let claude_session = hub.initialize(CLAUDE_TOKEN).await;
    let (status, pinned) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&claude_session),
            json!({ "jsonrpc": "2.0", "id": 50, "method": "tools/list" }),
        )
        .await;
    assert_eq!(status, 200);
    assert!(pinned.unwrap()["result"]["tools"].is_array());

    // codex 用自己的合法 Token，却带上 claude 的 Session-Id 发请求——
    // 必须被拒绝，绝不能借用 claude 已 pin 的身份/Workspace。
    let (status, hijacked) = hub
        .rpc(
            CODEX_TOKEN,
            Some(&claude_session),
            json!({ "jsonrpc": "2.0", "id": 51, "method": "tools/list" }),
        )
        .await;
    let rejected = status != 200 || hijacked.as_ref().is_some_and(|v| v.get("error").is_some());
    assert!(
        rejected,
        "借用别人已 pin 的 Session 必须被拒绝: {status} {hijacked:?}"
    );

    // 冒用尝试之后，claude 自己用同一个 Session 继续正常工作——拒绝
    // 不该反过来破坏原持有者的 pin。
    let (status, still_ok) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&claude_session),
            json!({ "jsonrpc": "2.0", "id": 52, "method": "tools/list" }),
        )
        .await;
    assert_eq!(status, 200, "被冒用尝试后，原持有者的 Session 必须仍然可用");
    assert!(still_ok.unwrap()["result"]["tools"].is_array());
}

/// 禁用集必须按 Workspace 隔离：同一工具可在 ws A 禁用、ws B 启用；且移除
/// override 后重建须清掉陈旧禁用项（此前全局并集会跨 Workspace 相互覆盖、
/// 并残留已删项）。直接在 state 层验证，无需下游进程。
#[tokio::test]
async fn disabled_tools_are_isolated_per_workspace_and_cleared_when_reenabled() {
    let state = AggregationState::new(None);

    // ws_a 禁用 (srv_x, tool)；ws_b 不禁用（override 缺席即启用）。
    let mut ov_a: mcpane_hub_catalog::ToolOverrides = BTreeMap::new();
    ov_a.insert(
        ("srv_x".into(), "tool".into()),
        mcpane_hub_catalog::ItemOverride::disabled(),
    );
    state
        .rebuild_catalog("ws_a", &CatalogOverrides::from_tools(ov_a))
        .await
        .unwrap();
    state
        .rebuild_catalog("ws_b", &CatalogOverrides::default())
        .await
        .unwrap();

    assert!(
        state.is_tool_disabled("ws_a", "srv_x", "tool"),
        "ws_a 应禁用"
    );
    assert!(
        !state.is_tool_disabled("ws_b", "srv_x", "tool"),
        "ws_b 不得被 ws_a 的禁用波及"
    );

    // ws_a 移除该 override 后重建：整体替换须清掉陈旧禁用项。
    state
        .rebuild_catalog("ws_a", &CatalogOverrides::default())
        .await
        .unwrap();
    assert!(
        !state.is_tool_disabled("ws_a", "srv_x", "tool"),
        "移除 override 重建后禁用项必须清除，不得残留"
    );
}

/// 回归：覆盖读库失败时必须保留旧禁用集，不得整体清空。
///
/// 空覆盖会被 `rebuild_catalog_scoped` 当成「本 Workspace 没有任何禁用项」，
/// 一次 DB 抖动就让用户明确关掉的工具重新进 tools/list 并通过调用时的二次
/// 校验——这是全仓唯一一条「关掉的东西会自己回来」的路径。
#[tokio::test]
async fn override_read_failure_keeps_previous_disabled_tools() {
    let pool = mcpane_storage::open_in_memory().await.unwrap();
    let workspaces = mcpane_storage::WorkspaceRepo::new(pool.clone());
    let ws = workspaces.create("限定").await.unwrap();
    workspaces
        .set_item_override(
            &ws.id,
            mcpane_storage::ItemKind::Tool,
            &mcpane_storage::ToolOverride {
                server_id: "srv_x".into(),
                original_name: "tool".into(),
                enabled: false,
                alias: None,
                description_suffix: None,
            },
        )
        .await
        .unwrap();

    let state = AggregationState::new(None);
    mcpane_hub::control::load_downstreams(&state, &pool).await;
    assert!(
        state.is_tool_disabled(&ws.id, "srv_x", "tool"),
        "前置：禁用应先生效"
    );

    // 模拟 DB 故障：覆盖表读不到（权限、损坏、迁移中途都会长这样）。
    sqlx::query("DROP TABLE workspace_tools")
        .execute(&pool)
        .await
        .unwrap();
    mcpane_hub::control::load_downstreams(&state, &pool).await;

    assert!(
        state.is_tool_disabled(&ws.id, "srv_x", "tool"),
        "覆盖读取失败时必须保留旧禁用集，不得让被禁用的工具重新可调用"
    );
}

/// 访问控制（deny-list）：按连接隔离 Server 可见性与调用权，
/// 覆盖鉴权、会话隔离、并发与活跃 Session 即时生效。
#[tokio::test]
async fn access_control_denies_server_per_connection() {
    let hub = setup().await;

    // codex 连接被拒绝访问 srv_fs；claude 不受限。
    hub.state
        .set_connection_denied_map(std::collections::HashMap::from([(
            "client_codex_test".to_owned(),
            std::collections::HashSet::from(["srv_fs".to_owned()]),
        )]));

    let claude = hub.initialize(CLAUDE_TOKEN).await;
    let codex = hub.initialize(CODEX_TOKEN).await;

    let list_names = |value: Value| -> Vec<String> {
        value["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect()
    };

    // 会话隔离：claude 全量可见，codex 看不到被拒 Server 的工具。
    let (_, claude_tools) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&claude),
            json!({ "jsonrpc": "2.0", "id": 30, "method": "tools/list" }),
        )
        .await;
    let claude_names = list_names(claude_tools.unwrap());
    assert!(claude_names.contains(&"filesystem__search".to_owned()));
    assert!(claude_names.contains(&"github__search".to_owned()));

    let (_, codex_tools) = hub
        .rpc(
            CODEX_TOKEN,
            Some(&codex),
            json!({ "jsonrpc": "2.0", "id": 31, "method": "tools/list" }),
        )
        .await;
    let codex_names = list_names(codex_tools.unwrap());
    assert!(codex_names.contains(&"github__search".to_owned()));
    assert!(
        codex_names.contains(&"mcpane__echo".to_owned()),
        "内建工具不受访问控制影响"
    );
    assert!(
        !codex_names.contains(&"filesystem__search".to_owned()),
        "被拒 Server 的工具不得出现在列表: {codex_names:?}"
    );

    // 被拒连接直调 → 明确拒绝（工具仍在已 pin 的 Revision 里，靠实时校验拦截）。
    let (status, denied) = hub
        .rpc(
            CODEX_TOKEN,
            Some(&codex),
            json!({ "jsonrpc": "2.0", "id": 32, "method": "tools/call",
                "params": { "name": "filesystem__search", "arguments": { "query": "sneak" } } }),
        )
        .await;
    let denied_value = denied.unwrap();
    assert_eq!(status, 200);
    let message = denied_value["error"]["message"]
        .as_str()
        .unwrap_or_default();
    assert!(
        message.contains("access control"),
        "拒绝原因必须可辨识: {denied_value}"
    );

    // 并发：两连接各自的合法调用互不串扰。
    let (claude_result, codex_result) = tokio::join!(
        hub.call(
            CLAUDE_TOKEN,
            &claude,
            33,
            "filesystem__search",
            json!({ "query": "allowed" })
        ),
        hub.call(
            CODEX_TOKEN,
            &codex,
            33,
            "github__search",
            json!({ "query": "allowed" })
        ),
    );
    assert_eq!(
        claude_result["result"]["structuredContent"]["server"],
        "filesystem-http"
    );
    assert_eq!(
        codex_result["result"]["structuredContent"]["server"],
        "github"
    );

    // 内建工具调用不受影响（诊断保底）。
    let echo = hub
        .call(
            CODEX_TOKEN,
            &codex,
            34,
            "mcpane__echo",
            json!({ "message": "ping" }),
        )
        .await;
    assert_eq!(echo["result"]["structuredContent"]["message"], "ping");

    // 解除 denial：同一个旧 Session 立即恢复可见与可调用（即时生效语义）。
    hub.state
        .set_server_denied("client_codex_test", "srv_fs", false);
    let (_, codex_tools) = hub
        .rpc(
            CODEX_TOKEN,
            Some(&codex),
            json!({ "jsonrpc": "2.0", "id": 35, "method": "tools/list" }),
        )
        .await;
    assert!(
        list_names(codex_tools.unwrap()).contains(&"filesystem__search".to_owned()),
        "解除后旧 Session 应立即恢复可见"
    );
    let restored = hub
        .call(
            CODEX_TOKEN,
            &codex,
            36,
            "filesystem__search",
            json!({ "query": "restored" }),
        )
        .await;
    assert_eq!(
        restored["result"]["structuredContent"]["server"],
        "filesystem-http"
    );
}

// ---------- Prompts/Resources 聚合 ----------

/// prompts/resources 聚合端到端：命名空间、URI 透传、模板前缀路由、
/// 访问控制过滤、无能力下游（stdio devserver）不受影响。
#[tokio::test]
async fn prompts_and_resources_aggregate_end_to_end() {
    let hub = setup().await;
    let session = hub.initialize(CLAUDE_TOKEN).await;

    // prompts/list：HTTP 下游的 review 以命名空间暴露；stdio 下游无 prompts 能力，
    // 不产生条目也不报错。
    let (status, prompts) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 20, "method": "prompts/list" }),
        )
        .await;
    assert_eq!(status, 200);
    let prompts = prompts.unwrap();
    let names: Vec<&str> = prompts["result"]["prompts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["filesystem__review"], "{names:?}");

    // prompts/get：按暴露名路由，下游收到原始名。
    let (status, got) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 21, "method": "prompts/get",
                "params": { "name": "filesystem__review" } }),
        )
        .await;
    assert_eq!(status, 200);
    let got = got.unwrap();
    assert_eq!(got["result"]["description"], "review a change");
    assert_eq!(
        got["result"]["messages"][0]["content"]["text"],
        "please review"
    );

    // 未知 prompt → JSON-RPC error。
    let (_, unknown) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 22, "method": "prompts/get",
                "params": { "name": "review" } }),
        )
        .await;
    assert!(
        unknown.unwrap().get("error").is_some(),
        "未命名空间的原始名不可路由"
    );

    // resources/list：URI 原样透传，不改写。
    let (status, resources) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 23, "method": "resources/list" }),
        )
        .await;
    assert_eq!(status, 200);
    let resources = resources.unwrap();
    let uris: Vec<&str> = resources["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["uri"].as_str().unwrap())
        .collect();
    assert_eq!(uris, vec!["mem://filesystem-http/readme"], "{uris:?}");

    // resources/read：静态条目精确路由。
    let (status, read) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 24, "method": "resources/read",
                "params": { "uri": "mem://filesystem-http/readme" } }),
        )
        .await;
    assert_eq!(status, 200);
    let read = read.unwrap();
    assert_eq!(
        read["result"]["contents"][0]["text"],
        "readme from http downstream"
    );

    // resources/templates/list + 模板字面前缀路由未在静态清单中的 URI。
    let (status, templates) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 25, "method": "resources/templates/list" }),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        templates.unwrap()["result"]["resourceTemplates"][0]["uriTemplate"],
        "mem://filesystem-http/logs/{name}"
    );
    let (status, log_read) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 26, "method": "resources/read",
                "params": { "uri": "mem://filesystem-http/logs/today.log" } }),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        log_read.unwrap()["result"]["contents"][0]["text"],
        "log content of mem://filesystem-http/logs/today.log"
    );

    // 完全未知的 URI → error。
    let (_, missing) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 27, "method": "resources/read",
                "params": { "uri": "mem://nope/x" } }),
        )
        .await;
    assert!(missing.unwrap().get("error").is_some());

    // 访问控制：拒绝 srv_fs 后，prompts/resources 从列表消失，读取被拒。
    hub.state
        .set_server_denied("client_claude_test", "srv_fs", true);
    let (_, prompts) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 28, "method": "prompts/list" }),
        )
        .await;
    assert!(
        prompts.unwrap()["result"]["prompts"]
            .as_array()
            .unwrap()
            .is_empty(),
        "被拒 Server 的 prompts 不应出现在列表"
    );
    let (_, denied_read) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 29, "method": "resources/read",
                "params": { "uri": "mem://filesystem-http/readme" } }),
        )
        .await;
    assert!(
        denied_read.unwrap().get("error").is_some(),
        "被拒 Server 的 resource 读取必须报错"
    );
}

// ---------- Tool Discovery 模式 ----------

/// 发现模式端到端：list 只暴露 meta 工具、discovery 检索、execute 透传、
/// 访问控制照常拦截；全量模式下 meta 工具不出现。
#[tokio::test]
async fn tool_discovery_mode_end_to_end() {
    let hub = setup().await;

    // 全量模式（setup 默认）：meta 工具不进列表。
    let session = hub.initialize(CLAUDE_TOKEN).await;
    let (_, tools) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 40, "method": "tools/list" }),
        )
        .await;
    let names: Vec<String> = tools.unwrap()["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(names.contains(&"github__search".into()), "{names:?}");
    assert!(
        !names.iter().any(|n| n == "mcpane__tool_discovery"),
        "全量模式不暴露 meta 工具：{names:?}"
    );

    // 开启发现模式后重建，新 Session 只见内建工具。
    // ADR-020：auto 模式下 claude 连接固定走平铺，测网关路径须显式设 gateway。
    hub.state
        .rebuild_catalog_scoped("ws_default", &CatalogOverrides::default(), None, true)
        .await
        .unwrap();
    hub.state
        .set_connection_exposures(std::collections::HashMap::from([(
            "client_claude_test".to_owned(),
            mcpane_hub::state::ExposureMode::Gateway,
        )]));
    let session = hub.initialize(CLAUDE_TOKEN).await;
    let (_, tools) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 41, "method": "tools/list" }),
        )
        .await;
    let tool_list = tools.unwrap()["result"]["tools"]
        .as_array()
        .unwrap()
        .clone();
    let names: Vec<String> = tool_list
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        names.contains(&"mcpane__tool_discovery".into()),
        "{names:?}"
    );
    assert!(names.contains(&"mcpane__tool_execute".into()), "{names:?}");
    assert!(
        !names.iter().any(|n| n.contains("__search")),
        "发现模式不平铺下游工具：{names:?}"
    );
    // 能力索引：discovery 工具的 description 须罗列各 Server 及其工具名——
    // 全隐藏会让模型不知道有什么可搜（技能式渐进披露）。
    let discovery_desc = tool_list
        .iter()
        .find(|t| t["name"] == "mcpane__tool_discovery")
        .unwrap()["description"]
        .as_str()
        .unwrap();
    assert!(
        discovery_desc.contains("github") && discovery_desc.contains("search"),
        "能力索引缺失：{discovery_desc}"
    );

    // discovery 检索：命中两个下游的同名 search。
    let found = hub
        .call(
            CLAUDE_TOKEN,
            &session,
            42,
            "mcpane__tool_discovery",
            json!({ "query": "search" }),
        )
        .await;
    let match_items = found["result"]["structuredContent"]["matches"]
        .as_array()
        .unwrap()
        .clone();
    let matches = match_items
        .iter()
        .map(|m| m["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(matches.contains(&"github__search".into()), "{matches:?}");
    assert!(
        matches.contains(&"filesystem__search".into()),
        "{matches:?}"
    );
    // 检索结果自带参数 schema，免去「执行报错才知道参数」的一轮试错。
    assert!(
        match_items.iter().all(|m| m["inputSchema"].is_object()),
        "{match_items:?}"
    );

    // execute 透传到目标下游。
    let executed = hub
        .call(
            CLAUDE_TOKEN,
            &session,
            43,
            "mcpane__tool_execute",
            json!({ "name": "filesystem__search", "arguments": { "query": "via-meta" } }),
        )
        .await;
    assert_eq!(
        executed["result"]["structuredContent"]["server"], "filesystem-http",
        "{executed}"
    );
    assert_eq!(executed["result"]["structuredContent"]["query"], "via-meta");

    // execute 不允许套娃内建工具。
    let nested = hub
        .call(
            CLAUDE_TOKEN,
            &session,
            44,
            "mcpane__tool_execute",
            json!({ "name": "mcpane__echo", "arguments": { "message": "x" } }),
        )
        .await;
    assert!(
        nested.get("error").is_some(),
        "套娃内建应报 JSON-RPC error：{nested}"
    );

    // 访问控制：拒绝 srv_fs 后，检索不再返回其工具，execute 被拒。
    hub.state
        .set_server_denied("client_claude_test", "srv_fs", true);
    let found = hub
        .call(
            CLAUDE_TOKEN,
            &session,
            45,
            "mcpane__tool_discovery",
            json!({ "query": "search" }),
        )
        .await;
    let matches = found["result"]["structuredContent"]["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(
        !matches.contains(&"filesystem__search".into()),
        "被拒 Server 的工具不应被检索到：{matches:?}"
    );
    let denied = hub
        .call(
            CLAUDE_TOKEN,
            &session,
            46,
            "mcpane__tool_execute",
            json!({ "name": "filesystem__search", "arguments": { "query": "x" } }),
        )
        .await;
    assert!(
        denied.get("error").is_some(),
        "被拒 Server 的 execute 应报错：{denied}"
    );

    /* 网关模式下一次调用只留一条日志。
     *
     * mcpane__tool_execute 是个信封，真正干活的是它转交出去的下游调用，而那条
     * 路径无论成败都自己记账。信封若也记一条，同一次调用就在日志里出现两遍——
     * 时刻相同、耗时只差一两毫秒，看着像重复记录，SQL 侧的请求数与耗时分布还会
     * 跟着翻倍（网关模式下每次调用都如此）。
     *
     * 反过来也要守住：信封不记账，就不能有哪条 execute 路径整个查无此事。这里
     * 三种结局各来一次——转发成功、套娃被拒、访问控制拒绝——每一种都得留下痕迹。
     */
    let logged = hub.state.logs.as_ref().unwrap().recent(200).await.unwrap();
    let names: Vec<&str> = logged
        .iter()
        .filter_map(|l| l.exposed_name.as_deref())
        .collect();
    assert!(
        !names.contains(&"mcpane__tool_execute"),
        "信封不该自己记一笔，否则每次网关调用都是双份：{names:?}"
    );
    assert!(
        logged
            .iter()
            .any(|l| { l.exposed_name.as_deref() == Some("filesystem__search") && l.success }),
        "转发成功的那次要按真实工具名记账：{names:?}"
    );
    assert!(
        logged
            .iter()
            .any(|l| { l.exposed_name.as_deref() == Some("mcpane__echo") && !l.success }),
        "套娃被拒不经下游路径，信封不记就整个丢了：{names:?}"
    );
    assert!(
        logged.iter().any(|l| {
            l.exposed_name.as_deref() == Some("filesystem__search")
                && !l.success
                && l.error_category.as_deref() == Some("policy")
        }),
        "访问控制拒绝也要留痕：{names:?}"
    );
}

// ---------- ADR-020：暴露模式 / 目录直给 / 升格 / execute 自愈 ----------

/// browse 兜底「永不为空」：无 query、空 query、零命中查询都返回全量目录，
/// 且目录条目不带 inputSchema（带了就退化成平铺）。检索命中时仍内联 schema。
#[tokio::test]
async fn discovery_browse_never_returns_empty() {
    let hub = setup().await;
    hub.state
        .rebuild_catalog_scoped("ws_default", &CatalogOverrides::default(), None, true)
        .await
        .unwrap();
    // codex 连接 auto → 跟随 workspace 开关 → gateway。
    let session = hub.initialize(CODEX_TOKEN).await;

    for (id, args) in [
        (70, json!({})),
        (71, json!({ "query": "  " })),
        // 实测暴露过的假阴性场景：语义对口的中文查询，BM25 零命中。
        (72, json!({ "query": "网页抓取" })),
    ] {
        let found = hub
            .call(CODEX_TOKEN, &session, id, "mcpane__tool_discovery", args)
            .await;
        let matches = found["result"]["structuredContent"]["matches"]
            .as_array()
            .unwrap()
            .clone();
        let total = found["result"]["structuredContent"]["totalTools"]
            .as_u64()
            .unwrap() as usize;
        assert_eq!(matches.len(), total, "兜底必须是全量目录：{found}");
        assert!(!matches.is_empty(), "永不为空：{found}");
        let names: Vec<&str> = matches
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"github__search"), "{names:?}");
        assert!(names.contains(&"filesystem__search"), "{names:?}");
        assert!(
            matches.iter().all(|m| m.get("inputSchema").is_none()),
            "目录条目不得带 schema：{matches:?}"
        );
    }

    // 命中路径不受兜底影响：仍内联 schema。
    let found = hub
        .call(
            CODEX_TOKEN,
            &session,
            73,
            "mcpane__tool_discovery",
            json!({ "query": "search" }),
        )
        .await;
    let matches = found["result"]["structuredContent"]["matches"]
        .as_array()
        .unwrap()
        .clone();
    assert!(
        matches.iter().all(|m| m["inputSchema"].is_object()),
        "{matches:?}"
    );

    // 双栈：新协议无 initialize 直接走同一条兜底路径。
    let (status, response) = hub
        .rpc_modern(
            CODEX_TOKEN,
            json!({ "jsonrpc": "2.0", "id": 74, "method": "tools/call",
                "params": { "name": "mcpane__tool_discovery", "arguments": {} } }),
        )
        .await;
    assert_eq!(status, 200);
    let body = response.unwrap();
    assert!(
        !body["result"]["structuredContent"]["matches"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{body}"
    );
}

/// tool_describe：精确取 schema + 网关模式下升格进 tools/list（幂等），
/// 打错名字附最近似候选而不是死胡同。
#[tokio::test]
async fn tool_describe_returns_schema_and_promotes() {
    let hub = setup().await;
    hub.state
        .rebuild_catalog_scoped("ws_default", &CatalogOverrides::default(), None, true)
        .await
        .unwrap();
    // 显式 gateway：本用例验的是网关形态的行为，不该被 auto 判定的启发式牵着走
    // （夹具里的工具数低于平铺上限，auto 现在会判成 flat）。
    hub.state
        .set_connection_exposures(std::collections::HashMap::from([(
            "client_codex_test".to_owned(),
            mcpane_hub::state::ExposureMode::Gateway,
        )]));
    let session = hub.initialize(CODEX_TOKEN).await;

    let described = hub
        .call(
            CODEX_TOKEN,
            &session,
            80,
            "mcpane__tool_describe",
            json!({ "name": "github__search" }),
        )
        .await;
    let output = &described["result"]["structuredContent"];
    assert_eq!(output["name"], "github__search", "{described}");
    assert_eq!(output["server"], "github");
    assert!(output["inputSchema"].is_object(), "{described}");
    assert_eq!(
        output["promoted"], true,
        "首次 describe 应升格：{described}"
    );

    // 升格后 tools/list 追加真实 schema 的该工具，meta 工具仍在。
    let (_, tools) = hub
        .rpc(
            CODEX_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 81, "method": "tools/list" }),
        )
        .await;
    let names: Vec<String> = tools.unwrap()["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(names.contains(&"github__search".into()), "{names:?}");
    assert!(names.contains(&"mcpane__tool_describe".into()), "{names:?}");
    assert!(
        !names.contains(&"filesystem__search".into()),
        "未 describe 的工具不应出现：{names:?}"
    );

    // 幂等：重复 describe 不再标记升格。
    let again = hub
        .call(
            CODEX_TOKEN,
            &session,
            82,
            "mcpane__tool_describe",
            json!({ "name": "github__search" }),
        )
        .await;
    assert_eq!(again["result"]["structuredContent"]["promoted"], false);

    // 打错名字：错误信息附最近似候选。
    let missed = hub
        .call(
            CODEX_TOKEN,
            &session,
            83,
            "mcpane__tool_describe",
            json!({ "name": "github__serch" }),
        )
        .await;
    let message = missed["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("github__search"), "应附候选：{missed}");
}

/// auto 模式分流：claude 连接固定平铺（无视 workspace 发现开关），
/// codex 等其余客户端跟随 workspace 开关。
#[tokio::test]
async fn auto_mode_claude_flat_codex_follows_workspace() {
    let hub = setup().await;
    hub.state
        .rebuild_catalog_scoped("ws_default", &CatalogOverrides::default(), None, true)
        .await
        .unwrap();

    let claude = hub.initialize(CLAUDE_TOKEN).await;
    let (_, tools) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&claude),
            json!({ "jsonrpc": "2.0", "id": 90, "method": "tools/list" }),
        )
        .await;
    let names: Vec<String> = tools.unwrap()["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        names.contains(&"github__search".into()),
        "claude auto→flat：{names:?}"
    );
    assert!(
        !names.contains(&"mcpane__tool_discovery".into()),
        "平铺不暴露 meta 工具：{names:?}"
    );

    // codex 走 workspace 开关，但开关只是「可以用网关」，不是「必须用」：
    // 夹具的工具数远低于平铺上限，auto 判定该把它留在平铺——网关的三跳漏斗
    // 只在平铺装不下时才划算。
    let codex = hub.initialize(CODEX_TOKEN).await;
    let (_, tools) = hub
        .rpc(
            CODEX_TOKEN,
            Some(&codex),
            json!({ "jsonrpc": "2.0", "id": 91, "method": "tools/list" }),
        )
        .await;
    let names: Vec<String> = tools.unwrap()["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        names.contains(&"github__search".into()),
        "工具数低于上限时 codex 也该平铺：{names:?}"
    );
    assert!(
        !names.contains(&"mcpane__tool_discovery".into()),
        "平铺不暴露 meta 工具：{names:?}"
    );

    // 判定依据必须可自诊断：光看 effectiveExposure 说不出「为什么是 flat」。
    let info = hub
        .call(
            CODEX_TOKEN,
            &codex,
            92,
            "mcpane__connection_info",
            json!({}),
        )
        .await;
    let info = &info["result"]["structuredContent"];
    assert_eq!(info["effectiveExposure"], "flat", "{info}");
    let visible = info["visibleToolCount"].as_u64().expect("应回显可见工具数");
    let limit = info["autoFlatToolLimit"].as_u64().expect("应回显平铺上限");
    assert!(
        visible <= limit,
        "flat 的理由应当自洽：{visible} <= {limit}"
    );
}

/// 连接显式模式覆盖一切：gateway 覆盖平铺工作区，flat 覆盖发现工作区。
#[tokio::test]
async fn explicit_mode_overrides_workspace_flag() {
    let hub = setup().await;

    // 默认工作区未开发现模式：显式 gateway 仍走网关。
    hub.state
        .set_connection_exposures(std::collections::HashMap::from([(
            "client_codex_test".to_owned(),
            mcpane_hub::state::ExposureMode::Gateway,
        )]));
    let codex = hub.initialize(CODEX_TOKEN).await;
    let (_, tools) = hub
        .rpc(
            CODEX_TOKEN,
            Some(&codex),
            json!({ "jsonrpc": "2.0", "id": 95, "method": "tools/list" }),
        )
        .await;
    let names: Vec<String> = tools.unwrap()["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        names.contains(&"mcpane__tool_discovery".into()),
        "显式 gateway 应覆盖平铺工作区：{names:?}"
    );

    // 发现工作区里显式 flat：平铺全量。
    hub.state
        .rebuild_catalog_scoped("ws_default", &CatalogOverrides::default(), None, true)
        .await
        .unwrap();
    hub.state
        .set_connection_exposures(std::collections::HashMap::from([(
            "client_codex_test".to_owned(),
            mcpane_hub::state::ExposureMode::Flat,
        )]));
    let codex = hub.initialize(CODEX_TOKEN).await;
    let (_, tools) = hub
        .rpc(
            CODEX_TOKEN,
            Some(&codex),
            json!({ "jsonrpc": "2.0", "id": 96, "method": "tools/list" }),
        )
        .await;
    let names: Vec<String> = tools.unwrap()["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        names.contains(&"github__search".into()),
        "显式 flat 应覆盖发现工作区：{names:?}"
    );
}

/// execute 自愈：缺必填参数时错误附本地校验发现与完整 inputSchema；
/// 直连调用路径不加料（客户端手里本就有 schema，响应形状不得漂移）。
#[tokio::test]
async fn execute_missing_required_param_gets_schema_hint() {
    let hub = setup().await;
    hub.state
        .rebuild_catalog_scoped("ws_default", &CatalogOverrides::default(), None, true)
        .await
        .unwrap();
    let codex = hub.initialize(CODEX_TOKEN).await;

    // rmcp 下游把参数反序列化失败作为 isError 结果返回（而非协议错误），
    // 自愈提示追加在 content 末尾。
    let failed = hub
        .call(
            CODEX_TOKEN,
            &codex,
            100,
            "mcpane__tool_execute",
            json!({ "name": "github__search", "arguments": {} }),
        )
        .await;
    assert_eq!(failed["result"]["isError"], true, "{failed}");
    let content_text = failed["result"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        content_text.contains("本地参数校验发现")
            && content_text.contains("缺少必填参数 query")
            && content_text.contains("inputSchema"),
        "execute 缺参错误应附校验发现与完整 schema：{failed}"
    );

    // execute 打错工具名：附最近似候选。
    let unknown = hub
        .call(
            CODEX_TOKEN,
            &codex,
            101,
            "mcpane__tool_execute",
            json!({ "name": "github__serch", "arguments": { "query": "x" } }),
        )
        .await;
    let message = unknown["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("github__search"), "应附候选：{unknown}");

    // 直连路径（claude auto→flat）同样缺参：响应逐字节不加料。
    let claude = hub.initialize(CLAUDE_TOKEN).await;
    let direct = hub
        .call(CLAUDE_TOKEN, &claude, 102, "github__search", json!({}))
        .await;
    assert_eq!(direct["result"]["isError"], true, "{direct}");
    assert!(
        !serde_json::to_string(&direct)
            .unwrap()
            .contains("本地参数校验发现"),
        "直连响应不得加料：{direct}"
    );
}

/// catalog 重建移除下游后，已升格的工具名被惰性过滤：不再出现在 tools/list，
/// 也不报错。
#[tokio::test]
async fn promoted_tool_dropped_after_catalog_rebuild_removes_server() {
    let hub = setup().await;
    hub.state
        .rebuild_catalog_scoped("ws_default", &CatalogOverrides::default(), None, true)
        .await
        .unwrap();
    // 同上：显式 gateway，与 auto 判定解耦。
    hub.state
        .set_connection_exposures(std::collections::HashMap::from([(
            "client_codex_test".to_owned(),
            mcpane_hub::state::ExposureMode::Gateway,
        )]));
    let session = hub.initialize(CODEX_TOKEN).await;
    hub.call(
        CODEX_TOKEN,
        &session,
        110,
        "mcpane__tool_describe",
        json!({ "name": "github__search" }),
    )
    .await;

    // 重建为只含 srv_fs 的成员集：github 的工具从 Revision 消失。
    let members = std::collections::HashSet::from(["srv_fs".to_owned()]);
    hub.state
        .rebuild_catalog_scoped(
            "ws_default",
            &CatalogOverrides::default(),
            Some(&members),
            true,
        )
        .await
        .unwrap();
    let session = hub.initialize(CODEX_TOKEN).await;
    let (_, tools) = hub
        .rpc(
            CODEX_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 111, "method": "tools/list" }),
        )
        .await;
    let names: Vec<String> = tools.unwrap()["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        !names.contains(&"github__search".into()),
        "失效的升格名应被惰性过滤：{names:?}"
    );
    assert!(
        names.contains(&"mcpane__tool_discovery".into()),
        "{names:?}"
    );
}

// ---------- 下游 list_changed → Catalog 自动重建 ----------

/// 工具集可变的 mock 下游：`mutate` 调用翻转开关（多出一个 `bonus` 工具）
/// 并向客户端发 tools/list_changed 通知。
#[derive(Clone)]
struct MutableDownstream {
    extra: Arc<std::sync::atomic::AtomicBool>,
}

impl ServerHandler for MutableDownstream {
    fn get_info(&self) -> ServerInfo {
        let mut implementation = Implementation::from_build_env();
        implementation.name = "mutable".into();
        let mut info = ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .build(),
        );
        info.server_info = implementation;
        info
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
        let schema: rmcp::model::JsonObject =
            serde_json::Map::from_iter([("type".to_owned(), json!("object"))]);
        let mut tools = vec![rmcp::model::Tool::new(
            "mutate",
            "flip toolset and notify",
            Arc::new(schema.clone()),
        )];
        tools[0].icons = Some(vec![rmcp::model::Icon::new(
            "https://example.com/mutate.png",
        )]);
        if self.extra.load(std::sync::atomic::Ordering::SeqCst) {
            tools.push(rmcp::model::Tool::new(
                "bonus",
                "appears after mutate",
                Arc::new(schema),
            ));
        }
        Ok(rmcp::model::ListToolsResult {
            tools,
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        params: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
        if params.name.as_ref() != "mutate" {
            return Err(rmcp::ErrorData::invalid_params("unknown tool", None));
        }
        self.extra.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = context.peer.notify_tool_list_changed().await;
        Ok(
            rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text("mutated")])
                .into(),
        )
    }
}

async fn start_mutable_downstream() -> String {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };
    let shared = MutableDownstream {
        extra: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let service = StreamableHttpService::new(
        move || Ok(shared.clone()),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default().with_legacy_session_mode(true),
    );
    let router = axum::Router::new().nest_service("/mcp", axum::routing::any_service(service));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{addr}/mcp")
}

/// 下游工具集变化后发 tools/list_changed：Hub 必须**自动**重建 Catalog
/// （无需手动 reload），新会话立即看到新工具；Session-Pinned 语义不受影响。
#[tokio::test]
async fn downstream_list_changed_triggers_auto_rebuild() {
    let state = Arc::new(AggregationState::new(None));
    state.spawn_catalog_refresher();

    let url = start_mutable_downstream().await;
    let spec = SpawnSpec::Http {
        url,
        bearer: None,
        bearer_env: None,
        env: BTreeMap::new(),
        headers: BTreeMap::new(),
        oauth: None,
    };
    let handle = DownstreamHandle::connect(&spec, Duration::from_secs(10), &ProxyConfig::default())
        .await
        .unwrap();
    state.register_server("srv_mut", "mutable", spec, handle);
    state
        .rebuild_catalog("ws_default", &CatalogOverrides::default())
        .await
        .unwrap();
    let before = state.pinned_catalog();
    assert_eq!(before.tools.len(), 1, "初始只有 mutate");
    // 下游申报的 icons 原样进入 Catalog（供上游 tools/list 与桌面端透传）。
    assert_eq!(
        before.tools[0].icons.as_ref().unwrap()[0]["src"],
        "https://example.com/mutate.png",
        "{:?}",
        before.tools[0].icons
    );

    // 触发下游工具集变化 + 通知。
    let handle = state.downstream("srv_mut").unwrap();
    handle.call_tool("mutate", None).await.unwrap();

    // 等待通知 → debounce → 自动重建（上限 10s 防慢 CI 假失败，正常 <1s）。
    let mut rebuilt = false;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let current = state.pinned_catalog();
        if current.id != before.id && current.tools.len() == 2 {
            assert!(
                current
                    .tools
                    .iter()
                    .any(|t| t.exposed_name == "mutable__bonus"),
                "新 Revision 应含 bonus 工具"
            );
            rebuilt = true;
            break;
        }
    }
    assert!(rebuilt, "list_changed 后 Catalog 必须自动重建");
}

/// 回归：自动重建（健康恢复 / 下游 list_changed 共用的
/// rebuild_all_workspaces）必须保持自建 Workspace 的成员集——
/// 此前 members=None 全量重建会让非成员 Server 的工具泄入。
#[tokio::test]
async fn auto_rebuild_preserves_workspace_membership() {
    let state = Arc::new(AggregationState::new(None));

    // 两个下游：ws_limited 只纳入 github。
    for (id, slug) in [("srv_a", "github"), ("srv_b", "filesystem")] {
        let spec = devserver_stdio_spec(slug);
        let handle =
            DownstreamHandle::connect(&spec, Duration::from_secs(10), &ProxyConfig::default())
                .await
                .unwrap();
        state.register_server(id, slug, spec, handle);
    }
    let members: std::collections::HashSet<String> = std::iter::once("srv_a".to_owned()).collect();
    state
        .rebuild_catalog_scoped(
            "ws_limited",
            &CatalogOverrides::default(),
            Some(&members),
            false,
        )
        .await
        .unwrap();

    let only_github = |revision: &mcpane_hub_catalog::CatalogRevision| {
        revision.tools.iter().all(|t| t.server_id == "srv_a")
            && revision
                .tools
                .iter()
                .any(|t| t.exposed_name == "github__search")
    };
    let before = state.pinned_catalog_map();
    assert!(only_github(before.get("ws_limited").unwrap()));

    // 自动重建路径（通知/健康恢复走的就是它）。
    state.rebuild_all_workspaces().await;

    let after = state.pinned_catalog_map();
    let revision = after.get("ws_limited").unwrap();
    assert!(
        only_github(revision),
        "自动重建后成员隔离必须保持，实际工具：{:?}",
        revision
            .tools
            .iter()
            .map(|t| t.exposed_name.as_str())
            .collect::<Vec<_>>()
    );
}

/// 用户声明的注解真的会出现在 `tools/list` 上，而下游自己申报的那份不被改写。
///
/// 一条 tools/list 里同时验两件事，靠的是两个下游的申报状态天然相反：
/// stdio devserver 的 `search` 带完整注解，HTTP 下游的 `search` 一个字没申报
/// ——后者正是真机上让 `codex exec` 报 `user cancelled MCP tool call` 的形态
/// （openai/codex#16685）。给两者都写上用户声明，出口该长什么样一目了然。
#[tokio::test]
async fn declared_annotations_reach_tools_list_without_overriding_downstream() {
    let hub = setup().await;

    // 两个下游给**不同**的声明值，且给 srv_github 的那份与 devserver 自己
    // 申报的处处相反：值撞上了，"下游优先"与"用户优先"两种实现会算出同一份
    // JSON，这条断言就等于没写（本测试第一版正是如此，反向验证时才现形）。
    let filling = mcpane_hub_catalog::DeclaredAnnotations {
        read_only_hint: true,
        destructive_hint: false,
        idempotent_hint: true,
        open_world_hint: false,
    };
    let contradicting = mcpane_hub_catalog::DeclaredAnnotations {
        read_only_hint: false,
        destructive_hint: true,
        idempotent_hint: false,
        open_world_hint: true,
    };
    let mut overrides = CatalogOverrides::default();
    for (server_id, declared) in [("srv_fs", filling), ("srv_github", contradicting)] {
        overrides.tools.insert(
            (server_id.into(), "search".into()),
            mcpane_hub_catalog::ItemOverride {
                declared_annotations: Some(declared),
                ..Default::default()
            },
        );
    }
    hub.state
        .rebuild_catalog("ws_default", &overrides)
        .await
        .unwrap();

    let session = hub.initialize(CLAUDE_TOKEN).await;
    let (_, listed) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 90, "method": "tools/list" }),
        )
        .await;
    let listed = listed.unwrap();
    let tools = listed["result"]["tools"].as_array().unwrap();
    let find = |name: &str| {
        tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} 应在 tools/list 里"))
            .clone()
    };

    // 下游没申报 → 用户声明补位，客户端拿到的是完整注解（Codex exec 由此可用）。
    let filled = find("filesystem__search");
    assert_eq!(
        filled["annotations"],
        json!({
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false
        }),
        "下游没申报注解时，用户声明的那份必须真的发出去"
    );

    // 下游申报了 → 原样透传，用户声明让位（不伪造下游，ADR-002 Gate）。
    let untouched = find("github__search");
    assert_eq!(
        untouched["annotations"],
        json!({
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false
        }),
        "下游申报的注解必须原样透传——用户声明的相反值一个字都不许盖进来"
    );

    // 没声明也没申报的工具依旧一个字都不编。
    let bare = find("filesystem__slow");
    assert!(
        bare.get("annotations").is_none_or(Value::is_null),
        "Hub 不得替没人申报也没人声明的工具编注解，实际：{bare}"
    );
}

/// Prompts/Resources 治理：覆盖经重建生效——被禁用的 prompt 从聚合端点缺席，
/// resource 同理（治理只在重建时生效：只读拉取无副作用，晚一个会话可接受）。
#[tokio::test]
async fn prompt_and_resource_overrides_take_effect_on_rebuild() {
    let hub = setup().await;

    let mut overrides = CatalogOverrides::default();
    overrides.prompts.insert(
        ("srv_fs".into(), "review".into()),
        mcpane_hub_catalog::ItemOverride::disabled(),
    );
    overrides.resources.insert(
        ("srv_fs".into(), "mem://filesystem-http/readme".into()),
        false,
    );
    hub.state
        .rebuild_catalog("ws_default", &overrides)
        .await
        .unwrap();

    let session = hub.initialize(CLAUDE_TOKEN).await;
    let (_, prompts) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 60, "method": "prompts/list" }),
        )
        .await;
    assert!(
        prompts.unwrap()["result"]["prompts"]
            .as_array()
            .unwrap()
            .is_empty(),
        "被禁用的 prompt 不应出现在聚合列表"
    );
    let (_, resources) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 61, "method": "resources/list" }),
        )
        .await;
    assert!(
        resources.unwrap()["result"]["resources"]
            .as_array()
            .unwrap()
            .is_empty(),
        "被禁用的 resource 不应出现在聚合列表"
    );
}

fn workspaces_repo(pool: &mcpane_storage::SqlitePool) -> mcpane_storage::WorkspaceRepo {
    mcpane_storage::WorkspaceRepo::new(pool.clone())
}

/// 全局注解声明经 `load_overrides` 合并进每个工作区的 Catalog。
///
/// 这条守的是 DB → Catalog 那一段：其余守卫都直接构造 `CatalogOverrides`，
/// 绕过了真正的读库合并——合并一旦漏写，那些守卫照样全绿，而线上功能是死的。
///
/// 断言落在**下游已申报**的工具上（devserver 的工具都带注解）：这样一条测试
/// 同时钉住两件事——声明确实被读进了目录（`declared_annotations` 非空），
/// 以及走完整读库路径后优先级依然是下游赢。
#[tokio::test]
async fn global_declarations_are_merged_into_every_workspace_catalog() {
    let pool = mcpane_storage::open_in_memory().await.unwrap();
    let servers = mcpane_storage::ServerRepo::new(pool.clone());
    servers
        .upsert(mcpane_storage::McpServerDef {
            id: "srv_github".into(),
            slug: "github".into(),
            name: "GitHub Dev".into(),
            transport: mcpane_storage::ServerTransport::Stdio,
            command: Some(env!("CARGO_BIN_EXE_mcpane-devserver").to_owned()),
            args: vec![],
            env: BTreeMap::from([("MCPANE_DEVSERVER_NAME".to_owned(), "github".to_owned())]),
            cwd: None,
            url: None,
            bearer: None,
            bearer_env: None,
            headers: BTreeMap::new(),
            runtime_policy: "shared".into(),
            enabled: true,
        })
        .await
        .unwrap();

    // 与 devserver 自己申报的处处相反——值撞上了就分不出是谁赢的。
    let contradicting = mcpane_hub_catalog::DeclaredAnnotations {
        read_only_hint: false,
        destructive_hint: true,
        idempotent_hint: false,
        open_world_hint: true,
    };
    servers
        .set_tool_annotations("srv_github", "search", Some(contradicting))
        .await
        .unwrap();

    // 同一个工具既被工作区禁用、又有全局声明：合并必须是"补上注解"，不是
    // "整条覆盖项换成默认值"。后者会把禁用悄悄翻回启用——本仓最忌讳的那类
    // 「关掉的东西自己回来」（见 override_read_failure_keeps_previous_disabled_tools）。
    workspaces_repo(&pool)
        .set_item_override(
            "ws_default",
            mcpane_storage::ItemKind::Tool,
            &mcpane_storage::ToolOverride {
                server_id: "srv_github".into(),
                original_name: "sleep".into(),
                enabled: false,
                alias: None,
                description_suffix: None,
            },
        )
        .await
        .unwrap();
    servers
        .set_tool_annotations("srv_github", "sleep", Some(contradicting))
        .await
        .unwrap();

    // 自建工作区一并验：声明不含 workspace 键，两个工作区都该拿到同一份。
    let ws = workspaces_repo(&pool).create("新项目").await.unwrap();

    let state = Arc::new(AggregationState::new(None));
    mcpane_hub::control::load_downstreams(&state, &pool).await;

    assert!(
        state.is_tool_disabled("ws_default", "srv_github", "sleep"),
        "带声明的工具被禁用后必须仍是禁用——合并只补注解，不得整条覆盖项换默认值"
    );
    let catalogs = state.pinned_catalog_map();
    assert!(
        catalogs
            .get("ws_default")
            .is_some_and(|r| r.route("github__sleep").is_none()),
        "被禁用的工具不得因为有注解声明而重新出现在 Catalog 里"
    );
    for workspace in ["ws_default", ws.id.as_str()] {
        let revision = catalogs
            .get(workspace)
            .unwrap_or_else(|| panic!("{workspace} 应有 Catalog"));
        let Some(tool) = revision.route("github__search") else {
            // 自建工作区默认无成员，没有工具是预期的——只对有工具的那个断言。
            continue;
        };
        assert_eq!(
            tool.declared_annotations,
            Some(contradicting),
            "{workspace}：全局声明必须被读库合并进目录"
        );
        let (value, source) = tool.effective_annotations().expect("下游申报了注解");
        assert_eq!(
            source,
            mcpane_hub_catalog::AnnotationSource::Downstream,
            "{workspace}：走完整读库路径后，优先级仍必须是下游赢"
        );
        assert_eq!(
            value["destructiveHint"],
            json!(false),
            "{workspace}：下游申报的值不得被用户声明的相反值盖过"
        );
    }
}

/// 增量装载：`load_downstreams` 不得重连规格没变的下游。
///
/// ReloadServers 有十余处调用点——逐工具开关、Prompt/Resource 开关、发现模式
/// 开关、Workspace 增删与成员变更、导入、云同步全都发它。它此前无条件重连
/// 每个已启用下游，于是「在编辑弹窗里关掉某个工具」会把所有 stdio Server 的
/// 子进程一并杀掉重起：打断它们在途的 tools/call，丢掉进程内状态（memory
/// Server 的图谱、跑长任务的 Server 的进度），耗时还随 Server 数线性叠加
/// （连接超时 10s × N，且装载循环是串行的）。
///
/// 用 `Arc::ptr_eq` 断言句柄同一性——它精确对应「这个下游有没有被换掉」，
/// 不依赖计时或进程探测。
#[tokio::test]
async fn reload_keeps_connections_whose_spec_did_not_change() {
    let pool = mcpane_storage::open_in_memory().await.unwrap();
    let servers = mcpane_storage::ServerRepo::new(pool.clone());
    let def = mcpane_storage::McpServerDef {
        id: "srv_github".into(),
        slug: "github".into(),
        name: "GitHub Dev".into(),
        transport: mcpane_storage::ServerTransport::Stdio,
        command: Some(env!("CARGO_BIN_EXE_mcpane-devserver").to_owned()),
        args: vec![],
        env: BTreeMap::from([("MCPANE_DEVSERVER_NAME".to_owned(), "github".to_owned())]),
        cwd: None,
        url: None,
        bearer: None,
        bearer_env: None,
        headers: BTreeMap::new(),
        runtime_policy: "per-client-connection".into(),
        enabled: true,
    };
    servers.upsert(def.clone()).await.unwrap();

    let state = Arc::new(AggregationState::new(None));
    mcpane_hub::control::load_downstreams(&state, &pool).await;
    let first = state.downstream("srv_github").expect("首轮装载应连上下游");

    // 与该 Server 的连接规格无关的一次改动（工具治理），触发第二轮 ReloadServers。
    mcpane_storage::WorkspaceRepo::new(pool.clone())
        .set_tool_override(
            "ws_default",
            &mcpane_storage::ToolOverride {
                server_id: "srv_github".into(),
                original_name: "search".into(),
                enabled: false,
                alias: None,
                description_suffix: None,
            },
        )
        .await
        .unwrap();
    mcpane_hub::control::load_downstreams(&state, &pool).await;
    let second = state.downstream("srv_github").expect("下游仍应在注册表里");
    assert!(
        Arc::ptr_eq(&first, &second),
        "规格未变的下游不得被重连——句柄换新即意味着 stdio 子进程被杀掉重起"
    );
    // 顺带确认这轮 reload 该做的事做了：治理生效，工具从 Catalog 缺席。
    assert!(
        !state
            .pinned_catalog()
            .tools
            .iter()
            .any(|t| t.original_name == "search"),
        "跳过重连不能让 Catalog 重建也跟着被跳过"
    );

    // 规格真的变了（改环境变量）→ 必须重连，否则新配置永远不生效。
    servers
        .upsert(mcpane_storage::McpServerDef {
            env: BTreeMap::from([
                ("MCPANE_DEVSERVER_NAME".to_owned(), "github".to_owned()),
                ("EXTRA".to_owned(), "1".to_owned()),
            ]),
            ..def
        })
        .await
        .unwrap();
    mcpane_hub::control::load_downstreams(&state, &pool).await;
    let third = state.downstream("srv_github").expect("重连后应有新句柄");
    assert!(
        !Arc::ptr_eq(&second, &third),
        "规格变了必须重连，否则用户改的配置不生效"
    );
}

/// 首连失败必须登记 Failed 占位，不能「本轮跳过」后诊断里缺席。
///
/// 缺席时前端在 Hub 可达下会合成「启动中」——stdio/`npx` 冷启动失败或
/// GUI 薄 PATH 找不到命令时，徽标会永远停在启动中，与 CHANGELOG
/// 「启动中 → 运行中/失败」的承诺相反。
#[tokio::test]
async fn load_downstreams_registers_failed_when_connect_errors() {
    let pool = mcpane_storage::open_in_memory().await.unwrap();
    let servers = mcpane_storage::ServerRepo::new(pool.clone());
    servers
        .upsert(mcpane_storage::McpServerDef {
            id: "srv_missing".into(),
            slug: "missing".into(),
            name: "Missing Bin".into(),
            transport: mcpane_storage::ServerTransport::Stdio,
            command: Some("__mcpane_definitely_not_a_real_binary__".into()),
            args: vec![],
            env: BTreeMap::new(),
            cwd: None,
            url: None,
            bearer: None,
            bearer_env: None,
            headers: BTreeMap::new(),
            runtime_policy: "per-client-connection".into(),
            enabled: true,
        })
        .await
        .unwrap();

    let state = Arc::new(AggregationState::new(None));
    mcpane_hub::control::load_downstreams(&state, &pool).await;

    assert!(
        state.downstream("srv_missing").is_none(),
        "连接失败不得留下可用句柄"
    );
    let registered = state
        .registered("srv_missing")
        .expect("失败下游必须占注册表槽位");
    assert_eq!(
        registered.status(),
        mcpane_hub::state::ServerStatus::Failed,
        "首连失败应落 Failed，而不是从诊断里消失"
    );
    assert!(
        registered.last_error().is_some(),
        "诊断需要 lastError 给用户看原因"
    );

    let diag = state.diagnostics();
    let entry = &diag["downstreams"]["srv_missing"];
    assert_eq!(entry["status"], "failed");
    assert!(
        entry["lastError"].as_str().is_some_and(|s| !s.is_empty()),
        "diagnostics 必须带上 lastError，got {entry}"
    );
}

/// 重连失败必须保留旧句柄：此前 `mark_server_failed` 走 `register_runtime(None)`
/// 会 shutdown 掉仍可用的旧连接，代理变更/改坏 command 后工具整页蒸发。
#[tokio::test]
async fn reload_connect_failure_keeps_previous_handle() {
    let pool = mcpane_storage::open_in_memory().await.unwrap();
    let servers = mcpane_storage::ServerRepo::new(pool.clone());
    let def = mcpane_storage::McpServerDef {
        id: "srv_github".into(),
        slug: "github".into(),
        name: "GitHub Dev".into(),
        transport: mcpane_storage::ServerTransport::Stdio,
        command: Some(env!("CARGO_BIN_EXE_mcpane-devserver").to_owned()),
        args: vec![],
        env: BTreeMap::from([("MCPANE_DEVSERVER_NAME".to_owned(), "github".to_owned())]),
        cwd: None,
        url: None,
        bearer: None,
        bearer_env: None,
        headers: BTreeMap::new(),
        runtime_policy: "per-client-connection".into(),
        enabled: true,
    };
    servers.upsert(def.clone()).await.unwrap();

    let state = Arc::new(AggregationState::new(None));
    mcpane_hub::control::load_downstreams(&state, &pool).await;
    let first = state.downstream("srv_github").expect("首轮装载应连上下游");
    assert_eq!(
        state.registered("srv_github").unwrap().status(),
        mcpane_hub::state::ServerStatus::Ready
    );

    // 故意改成找不到的命令，迫使重连并失败。
    servers
        .upsert(mcpane_storage::McpServerDef {
            command: Some("__mcpane_definitely_not_a_real_binary__".into()),
            ..def
        })
        .await
        .unwrap();
    mcpane_hub::control::load_downstreams(&state, &pool).await;

    let second = state
        .downstream("srv_github")
        .expect("重连失败须保留旧句柄，不能 shutdown");
    assert!(
        Arc::ptr_eq(&first, &second),
        "重连失败时句柄必须与重连前是同一份"
    );
    let registered = state.registered("srv_github").unwrap();
    assert_eq!(
        registered.status(),
        mcpane_hub::state::ServerStatus::Failed,
        "状态应落到 Failed，让诊断收敛而不是永久启动中"
    );
    assert!(
        registered.last_error().is_some(),
        "须带上本次重连失败的原因"
    );
}

// ---------- 治理禁用对「已 pin 的活跃会话」即时生效（tool / prompt / resource 同规） ----------
//
// ADR-004 让每个 Session 在创建时 pin 住 Catalog 快照，好处是路由目标不会中途
// 改变；代价是**治理变更不会自动进入已建立的会话**。对「禁用」这类收紧型动作
// 这个代价不可接受，所以调用路径上必须有一次实时的二次校验。
//
// 三条路径此前只有 tools/call 做了这件事：prompts/get 完全没有 Prompt 禁用检查，
// resources/read 查的是**已 pin 的快照**（只挡得住 pin 时就已禁用的）。于是
// 「在界面上禁用一个 Prompt / Resource」对已连接的客户端毫无作用——它只是从
// list 里消失，按名字/URI 照样读得到内容，而同样的操作对 Tool 立刻生效。
// 用户没有任何理由预期这三类行为不同。下面三条锁住它们的一致性。

/// Tool：会话建立**之后**禁用，同一会话再调用必须被拒。
#[tokio::test]
async fn tool_disabled_after_session_pin_is_rejected() {
    let hub = setup().await;
    let session = hub.initialize(CLAUDE_TOKEN).await;

    let mut overrides = CatalogOverrides::default();
    overrides.tools.insert(
        ("srv_fs".into(), "search".into()),
        mcpane_hub_catalog::ItemOverride::disabled(),
    );
    hub.state
        .rebuild_catalog("ws_default", &overrides)
        .await
        .unwrap();

    let (_, called) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 900, "method": "tools/call",
                "params": { "name": "filesystem__search", "arguments": { "query": "x" } } }),
        )
        .await;
    let called = called.unwrap();
    assert!(
        called.get("error").is_some(),
        "pin 之后被禁用的 Tool 必须拒绝：{called}"
    );
}

/// Prompt：会话建立**之后**禁用，同一会话再 prompts/get 必须被拒。
///
/// 回归——此前 `get_prompt` 只查 Server 访问控制，没有任何 Prompt 禁用检查，
/// 已连接的客户端能原样读到被禁用 Prompt 的全文。
#[tokio::test]
async fn prompt_disabled_after_session_pin_is_rejected() {
    let hub = setup().await;
    let session = hub.initialize(CLAUDE_TOKEN).await;

    let mut overrides = CatalogOverrides::default();
    overrides.prompts.insert(
        ("srv_fs".into(), "review".into()),
        mcpane_hub_catalog::ItemOverride::disabled(),
    );
    hub.state
        .rebuild_catalog("ws_default", &overrides)
        .await
        .unwrap();

    let (_, got) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 901, "method": "prompts/get",
                "params": { "name": "filesystem__review" } }),
        )
        .await;
    let got = got.unwrap();
    assert!(
        got.get("error").is_some(),
        "pin 之后被禁用的 Prompt 必须拒绝（与 Tool 同规），实际拿到了内容：{got}"
    );
}

/// completion 与 prompts/get 同规：pin 之后被禁用的 Prompt，参数补全同样拒绝。
/// 回归——此前 complete() 只查服务器级访问控制，禁用单个 Prompt 后补全照常返回。
#[tokio::test]
async fn completion_for_disabled_prompt_is_rejected() {
    let hub = setup().await;
    let session = hub.initialize(CLAUDE_TOKEN).await;

    let mut overrides = CatalogOverrides::default();
    overrides.prompts.insert(
        ("srv_fs".into(), "review".into()),
        mcpane_hub_catalog::ItemOverride::disabled(),
    );
    hub.state
        .rebuild_catalog("ws_default", &overrides)
        .await
        .unwrap();

    let (_, got) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 903, "method": "completion/complete",
                "params": { "ref": { "type": "ref/prompt", "name": "filesystem__review" },
                    "argument": { "name": "path", "value": "src" } } }),
        )
        .await;
    let got = got.unwrap();
    assert!(
        got.get("error").is_some(),
        "被禁用 Prompt 的补全必须拒绝（治理不能被补全旁路），实际拿到：{got}"
    );
}

/// Resource：会话建立**之后**禁用，同一会话再 resources/read 必须被拒。
///
/// 回归——此前 `read_resource` 的禁用判断查的是已 pin 的 Revision 快照，只在
/// 「精确未命中」的兜底分支生效；会话之后才禁用的 Resource 在快照里还在，
/// 精确命中直接放行。
#[tokio::test]
async fn resource_disabled_after_session_pin_is_rejected() {
    let hub = setup().await;
    let session = hub.initialize(CLAUDE_TOKEN).await;

    let mut overrides = CatalogOverrides::default();
    overrides.resources.insert(
        ("srv_fs".into(), "mem://filesystem-http/readme".into()),
        false,
    );
    hub.state
        .rebuild_catalog("ws_default", &overrides)
        .await
        .unwrap();

    let (_, read) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 902, "method": "resources/read",
                "params": { "uri": "mem://filesystem-http/readme" } }),
        )
        .await;
    let read = read.unwrap();
    assert!(
        read.get("error").is_some(),
        "pin 之后被禁用的 Resource 必须拒绝（与 Tool 同规），实际读到了内容：{read}"
    );
}

/// 三类禁用互不串扰：禁用同名的 Prompt 不得把 Tool 也拦掉。
///
/// 禁用集按类别分开存正是为了这个——storage 的 `workspace_tools.kind` 自 0010
/// 起就把同名 tool/prompt 当作两个条目，Hub 侧的二次校验不能把它们并回一张表。
#[tokio::test]
async fn disabling_a_prompt_does_not_disable_a_same_named_tool() {
    let hub = setup().await;

    let mut overrides = CatalogOverrides::default();
    // 禁用 prompt 「search」——与 srv_fs 的 tool 「search」同名。
    overrides.prompts.insert(
        ("srv_fs".into(), "search".into()),
        mcpane_hub_catalog::ItemOverride::disabled(),
    );
    hub.state
        .rebuild_catalog("ws_default", &overrides)
        .await
        .unwrap();

    let session = hub.initialize(CLAUDE_TOKEN).await;
    let (_, called) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 903, "method": "tools/call",
                "params": { "name": "filesystem__search", "arguments": { "query": "x" } } }),
        )
        .await;
    let called = called.unwrap();
    assert!(
        called.get("error").is_none(),
        "禁用同名 Prompt 不得连累 Tool：{called}"
    );
}

/// 下游侧的 MRTR 往返基线：`call_tool_once` 不替调用方驱动多轮。
#[tokio::test]
async fn downstream_mrtr_round_trip_is_not_auto_driven() {
    let handle = DownstreamHandle::connect(
        &devserver_stdio_spec("mrtr"),
        Duration::from_secs(10),
        &ProxyConfig::default(),
    )
    .await
    .unwrap();

    // 首轮：下游要求确认。
    let first = handle
        .call_tool_once(
            "confirm",
            Some(serde_json::Map::from_iter([(
                "action".to_owned(),
                Value::String("delete".to_owned()),
            )])),
            None,
            None,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("首轮调用");
    let state = match first {
        rmcp::model::CallToolResponse::InputRequired(pending) => {
            pending.request_state.expect("下游应给出 requestState")
        }
        other => panic!("首轮应是 input_required，实际: {other:?}"),
    };

    // 次轮：规范要求重发**原始请求**（同样的参数）外加 requestState。
    let second = handle
        .call_tool_once(
            "confirm",
            Some(serde_json::Map::from_iter([(
                "action".to_owned(),
                Value::String("delete".to_owned()),
            )])),
            None,
            Some(state.clone()),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("次轮调用");
    match second {
        rmcp::model::CallToolResponse::Complete(result) => {
            let text = serde_json::to_string(&result.content).unwrap();
            assert!(text.contains("confirmed via"), "次轮未执行: {text}");
        }
        other => panic!("次轮应是 Complete，实际: {other:?}"),
    }
}

/// TASK-805 端到端：下游要求补充输入时，Hub 把 `input_required` 透传给上游，
/// 且 `requestState` 必须是**重新密封**过的——上游看不到下游那份，也伪造不了。
/// 不通的话，任何带确认/授权交互的下游工具在 Hub 后面都是废的。
#[tokio::test]
async fn hub_passes_mrtr_through_with_a_resealed_request_state() {
    let hub = setup().await;
    let call = |args: Value, state: Option<&str>| {
        let mut params = json!({ "name": "github__confirm", "arguments": args });
        if let Some(state) = state {
            params["requestState"] = Value::String(state.to_owned());
        }
        json!({ "jsonrpc": "2.0", "id": 70, "method": "tools/call", "params": params })
    };

    // 首轮：拿到 input_required 与密封后的 state。
    let (status, first) = hub
        .rpc_modern(CLAUDE_TOKEN, call(json!({ "action": "delete" }), None))
        .await;
    let first = first.unwrap_or_else(|| panic!("首轮请求失败: HTTP {status}"));
    assert_eq!(
        first["result"]["resultType"], "input_required",
        "Hub 没把 input_required 透传上来: {first}"
    );
    let sealed = first["result"]["requestState"]
        .as_str()
        .expect("透传的 input_required 必须带 requestState")
        .to_owned();
    // 下游那份原文是 "pending:delete"——泄给上游就等于让它能伪造别的下游的 state。
    assert!(
        !sealed.contains("pending:delete"),
        "下游的 requestState 原文泄给了上游: {sealed}"
    );

    // 次轮：带着密封的 state 重试，Hub 解封后转给下游，这次真正执行。
    let (_, second) = hub
        .rpc_modern(
            CLAUDE_TOKEN,
            call(json!({ "action": "delete" }), Some(&sealed)),
        )
        .await;
    let second = second.unwrap();
    let text = serde_json::to_string(&second["result"]["content"]).unwrap();
    assert!(
        text.contains("confirmed via pending:delete"),
        "次轮没能带着解封后的 state 执行: {second}"
    );

    // 篡改：改掉密文任意一段，HMAC 必须认不出来。
    let tampered = format!("{}x", &sealed[..sealed.len() - 1]);
    let (status, forged) = hub
        .rpc_modern(
            CLAUDE_TOKEN,
            call(json!({ "action": "delete" }), Some(&tampered)),
        )
        .await;
    // 新协议把协议层错误映射成 HTTP 4xx，legacy 则是 200 + JSON-RPC error；
    // 两种拒绝都算数，被放行才是问题。
    assert!(
        status >= 400 || forged.is_some_and(|v| v["error"].is_object()),
        "被篡改的 requestState 必须拒绝，实际 HTTP {status}"
    );

    // 跨下游重放：拿 github 的 state 去调 filesystem 的同名工具。
    // associated_data 绑的是 server_id，这里必须解封失败。
    let mut cross = call(json!({ "action": "delete" }), Some(&sealed));
    cross["params"]["name"] = Value::String("filesystem__confirm".to_owned());
    let (status, replayed) = hub.rpc_modern(CLAUDE_TOKEN, cross).await;
    assert!(
        status >= 400 || replayed.is_some_and(|v| v["error"].is_object()),
        "一个下游的 requestState 不得在另一个下游上重放，实际 HTTP {status}"
    );
}

/// legacy 上游既不认识 `input_required` 也不会回传 `requestState`，透传过去
/// 只会变成一个无从解释的错误。必须在 Hub 这里给出说得清原因的拒绝。
#[tokio::test]
async fn legacy_upstream_gets_a_readable_error_for_mrtr_tools() {
    let hub = setup().await;
    let session = hub.initialize(CLAUDE_TOKEN).await;
    let (_, response) = hub
        .rpc(
            CLAUDE_TOKEN,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 71, "method": "tools/call",
                    "params": { "name": "github__confirm",
                                "arguments": { "action": "delete" } } }),
        )
        .await;
    let response = response.unwrap();
    let message = response["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("2026-07-28"),
        "legacy 上游该被告知这需要新协议，实际: {response}"
    );
}

/// 发现模式下经 `mcpane__tool_execute` 调用需要确认的工具，也必须能走完
/// 多轮往返。发现模式恰恰用在工具很多的场景，需要确认的工具在那里更常见，
/// 不是可以放弃的边角。
#[tokio::test]
async fn discovery_mode_tool_execute_completes_the_mrtr_round_trip() {
    let hub = setup().await;
    hub.state
        .rebuild_catalog_scoped("ws_default", &CatalogOverrides::default(), None, true)
        .await
        .unwrap();

    let call = |state: Option<&str>| {
        let mut params = json!({
            "name": "mcpane__tool_execute",
            "arguments": { "name": "github__confirm", "arguments": { "action": "delete" } }
        });
        if let Some(state) = state {
            params["requestState"] = Value::String(state.to_owned());
        }
        json!({ "jsonrpc": "2.0", "id": 80, "method": "tools/call", "params": params })
    };

    let (status, first) = hub.rpc_modern(CLAUDE_TOKEN, call(None)).await;
    let first = first.unwrap_or_else(|| panic!("首轮失败: HTTP {status}"));
    assert_eq!(
        first["result"]["resultType"], "input_required",
        "发现模式没把 input_required 透传上来: {first}"
    );
    let sealed = first["result"]["requestState"]
        .as_str()
        .expect("应带密封后的 requestState")
        .to_owned();

    let (_, second) = hub.rpc_modern(CLAUDE_TOKEN, call(Some(&sealed))).await;
    let second = second.unwrap();
    let text = serde_json::to_string(&second["result"]["content"]).unwrap();
    assert!(
        text.contains("confirmed via pending:delete"),
        "发现模式的次轮没能带着解封后的 state 执行: {second}"
    );
}

/// SEP-2549 把 `ttlMs`/`cacheScope` 定为 `resources/read` 结果的**必填**字段
/// （不只是四个 list）。TTL 取 0：资源内容属于下游、变化不带通知，Hub 无法
/// 为其新鲜度作保，0（立即过期）是唯一诚实的值。legacy 侧照旧一个字段不带。
#[tokio::test]
async fn resources_read_carries_required_cache_fields_only_for_modern() {
    let hub = setup().await;

    let read = json!({ "jsonrpc": "2.0", "id": 90, "method": "resources/read",
                       "params": { "uri": "mem://filesystem-http/readme" } });

    let (status, modern) = hub.rpc_modern(CLAUDE_TOKEN, read.clone()).await;
    let modern = modern.unwrap_or_else(|| panic!("modern read 失败: HTTP {status}"));
    assert_eq!(
        modern["result"]["ttlMs"], 0,
        "modern resources/read 缺必填的 ttlMs: {modern}"
    );
    assert_eq!(
        modern["result"]["cacheScope"], "private",
        "modern resources/read 缺必填的 cacheScope: {modern}"
    );

    let session = hub.initialize(CLAUDE_TOKEN).await;
    let (_, legacy) = hub.rpc(CLAUDE_TOKEN, Some(&session), read).await;
    let legacy = legacy.unwrap();
    assert!(
        legacy["result"]["ttlMs"].is_null() && legacy["result"]["cacheScope"].is_null(),
        "legacy resources/read 不该带新协议字段: {legacy}"
    );
}
