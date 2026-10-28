//! 双栈协议面集成测试（ADR-019）。
//!
//! 覆盖两件事：`2026-07-28` 无状态路径可用；以及全部历史版本的 legacy 路径
//! 一字未变——后者是「升级不影响旧客户端」这一承诺的守卫。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use mcpane_hub::app::{HubOptions, build_router};
use mcpane_hub::auth::{ClientIdentity, TokenRegistry};
use serde_json::{Value, json};

const TOKEN: &str = "test-claude-token-0000000000000000000000000000000000000001";

/// rmcp `KNOWN_VERSIONS` 全集：每一个都必须能走通 legacy initialize。
const LEGACY_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

/// 新协议才有的字段，legacy 响应里一个都不该出现。
const MODERN_ONLY_KEYS: &[&str] = &["resultType", "ttlMs", "cacheScope"];

struct Hub {
    url: String,
    client: reqwest::Client,
    state: Arc<mcpane_hub::state::AggregationState>,
}

async fn start() -> Hub {
    let registry = TokenRegistry::new();
    registry.register(
        TOKEN,
        ClientIdentity::from_raw("client_claude_test".into(), "claude"),
    );
    let state = Arc::new(mcpane_hub::state::AggregationState::new(None));
    let router = build_router(
        Arc::new(registry),
        state.clone(),
        HubOptions {
            stateful: false,
            session_keep_alive: None,
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    Hub {
        url: format!("http://{addr}/mcp"),
        client: reqwest::Client::new(),
        state,
    }
}

impl Hub {
    /// 发一个 POST，附带可选的 MCP 标准头，返回解析后的 JSON。
    async fn post(&self, body: Value, headers: &[(&str, &str)]) -> Value {
        let mut request = self
            .client
            .post(&self.url)
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {TOKEN}"));
        for (k, v) in headers {
            request = request.header(*k, *v);
        }
        let text = request
            .json(&body)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("非 JSON 响应({e}): {text}"))
    }
}

/// 新协议每请求自带的 `_meta` 信封（SEP-2575）。
fn modern_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": { "name": "probe", "version": "0.0.1" }
    })
}

fn assert_no_modern_keys(label: &str, value: &Value) {
    fn walk(label: &str, path: &str, value: &Value) {
        match value {
            Value::Object(map) => {
                for (k, v) in map {
                    assert!(
                        !MODERN_ONLY_KEYS.contains(&k.as_str()),
                        "{label}: legacy 响应在 {path}.{k} 掺入了新协议字段",
                    );
                    walk(label, &format!("{path}.{k}"), v);
                }
            }
            Value::Array(items) => {
                for (i, v) in items.iter().enumerate() {
                    walk(label, &format!("{path}[{i}]"), v);
                }
            }
            _ => {}
        }
    }
    walk(label, "", value);
}

#[tokio::test]
async fn discover_advertises_modern_protocol_first_class() {
    let hub = start().await;
    let response = hub
        .post(
            json!({"jsonrpc":"2.0","id":1,"method":"server/discover",
                   "params":{"_meta": modern_meta()}}),
            &[
                ("MCP-Protocol-Version", "2026-07-28"),
                ("Mcp-Method", "server/discover"),
            ],
        )
        .await;
    let versions = response["result"]["supportedVersions"]
        .as_array()
        .unwrap_or_else(|| panic!("discover 未返回 supportedVersions: {response}"));
    let versions: Vec<&str> = versions.iter().map(|v| v.as_str().unwrap()).collect();
    assert!(
        versions.contains(&"2026-07-28"),
        "discover 必须宣告 2026-07-28：{versions:?}"
    );
    // 历史版本一个都不能少——这是旧客户端的生命线（ADR-019 决策 3）。
    for legacy in LEGACY_VERSIONS {
        assert!(
            versions.contains(legacy),
            "discover 丢了历史版本 {legacy}：{versions:?}"
        );
    }
    // 2026-07-28 起服务端身份在 _meta 里，没有顶层 serverInfo 字段。
    assert_eq!(
        response["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "mcpane-hub",
        "discover 未在 _meta 里带服务端身份: {response}"
    );
    // 与 legacy 相反：新协议这边必须宣告，否则 subscriptions/listen 会被
    // `supported_by` 过滤成空订阅。三类一起：Catalog 重建同时改变三张列表，
    // 三类治理的行为必须一致。
    for kind in ["tools", "prompts", "resources"] {
        assert_eq!(
            response["result"]["capabilities"][kind]["listChanged"], true,
            "新协议未宣告 {kind} 的 listChanged: {response}"
        );
    }
}

#[tokio::test]
async fn modern_protocol_works_without_initialize() {
    let hub = start().await;
    let headers = [("MCP-Protocol-Version", "2026-07-28")];

    // 无 initialize、无 Session，直接 tools/list。
    let tools = hub
        .post(
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list",
                   "params":{"_meta": modern_meta()}}),
            &[headers[0], ("Mcp-Method", "tools/list")],
        )
        .await;
    let list = tools["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("新协议 tools/list 失败: {tools}"));
    assert!(!list.is_empty());

    let call = hub
        .post(
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
                   "params":{"name":"mcpane__echo","arguments":{"message":"modern"},
                             "_meta": modern_meta()}}),
            &[
                headers[0],
                ("Mcp-Method", "tools/call"),
                ("Mcp-Name", "mcpane__echo"),
            ],
        )
        .await;
    assert_eq!(
        call["result"]["structuredContent"]["message"], "modern",
        "新协议 tools/call 失败: {call}"
    );
}

/// SEP-2549 缓存契约：新协议拿得到 `ttlMs`/`cacheScope`，legacy 一个都看不到。
/// 后半句由 `every_legacy_version_still_initializes_unchanged` 的
/// `assert_no_modern_keys` 守住，这里只验前半句。
#[tokio::test]
async fn modern_list_results_carry_cache_hints() {
    let hub = start().await;
    let tools = hub
        .post(
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list",
                   "params":{"_meta": modern_meta()}}),
            &[
                ("MCP-Protocol-Version", "2026-07-28"),
                ("Mcp-Method", "tools/list"),
            ],
        )
        .await;
    assert!(
        tools["result"]["ttlMs"].as_u64().is_some_and(|v| v > 0),
        "新协议 tools/list 缺 ttlMs: {tools}"
    );
    // private：列表按 Workspace/连接隔离，共享中间层不得缓存后端复用。
    assert_eq!(
        tools["result"]["cacheScope"], "private",
        "新协议 tools/list 的 cacheScope 必须是 private: {tools}"
    );

    // 另采样一个列表端点，确认契约不是只挂在 tools/list 上。
    let prompts = hub
        .post(
            json!({"jsonrpc":"2.0","id":2,"method":"prompts/list",
                   "params":{"_meta": modern_meta()}}),
            &[
                ("MCP-Protocol-Version", "2026-07-28"),
                ("Mcp-Method", "prompts/list"),
            ],
        )
        .await;
    assert!(
        prompts["result"]["ttlMs"].as_u64().is_some_and(|v| v > 0),
        "新协议 prompts/list 缺 ttlMs: {prompts}"
    );
}

/// `mcpane__connection_info` 是互操作验证工具，必须回显本次请求真正协商到的
/// 版本；同时新协议无 Session，`sessionId` 必须是 null。
#[tokio::test]
async fn connection_info_reports_the_negotiated_modern_version() {
    let hub = start().await;
    let call = hub
        .post(
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                   "params":{"name":"mcpane__connection_info","arguments":{},
                             "_meta": modern_meta()}}),
            &[
                ("MCP-Protocol-Version", "2026-07-28"),
                ("Mcp-Method", "tools/call"),
                ("Mcp-Name", "mcpane__connection_info"),
            ],
        )
        .await;
    let info = &call["result"]["structuredContent"];
    assert_eq!(
        info["protocolVersion"], "2026-07-28",
        "connection_info 回显了错误的协议版本: {call}"
    );
    assert!(
        info["sessionId"].is_null(),
        "新协议无 Session，sessionId 必须为 null: {call}"
    );
}

/// TASK-803 核心验收：订阅建立后，Catalog 重建必须变成一条真正推到客户端的
/// `tools/list_changed`。这是 ADR-019「残留窗口」收窄的主要手段——不通就等于
/// 无状态下 Workspace 改绑只能靠 60s TTL 兜底。
#[tokio::test]
async fn subscriptions_listen_pushes_tools_list_changed() {
    use futures::StreamExt;

    let hub = start().await;
    let response = hub
        .client
        .post(&hub.url)
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {TOKEN}"))
        .header("MCP-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", "subscriptions/listen")
        .json(&json!({
            "jsonrpc":"2.0","id":1,"method":"subscriptions/listen",
            "params":{"notifications":{"toolsListChanged": true}, "_meta": modern_meta()}
        }))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "subscriptions/listen 未被受理: {}",
        response.status()
    );

    let mut stream = response.bytes_stream();
    // 先收下受理通知（SDK 在调用 listen() 前发出），之后才触发重建，避免竞态。
    let ack = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
        .await
        .expect("等待 listen 受理应答超时")
        .expect("流提前结束")
        .unwrap();
    let ack = String::from_utf8_lossy(&ack);
    assert!(
        ack.contains("notifications/subscriptions/acknowledged"),
        "首帧不是受理通知: {ack}"
    );
    assert!(
        ack.contains("\"toolsListChanged\":true"),
        "服务端没接受 toolsListChanged 订阅: {ack}"
    );

    // 先重建**别的** Workspace：本连接绑的是 ws_default，不该收到任何东西。
    hub.state
        .rebuild_catalog("ws_other", &mcpane_hub_catalog::CatalogOverrides::default())
        .await
        .unwrap();
    let leaked = tokio::time::timeout(std::time::Duration::from_millis(500), stream.next()).await;
    assert!(
        leaked.is_err(),
        "别的 Workspace 的 Catalog 重建串给了本连接: {leaked:?}"
    );

    let state = hub.state.clone();
    tokio::spawn(async move {
        state
            .rebuild_catalog(
                mcpane_hub::state::DEFAULT_WORKSPACE,
                &mcpane_hub_catalog::CatalogOverrides::default(),
            )
            .await
            .unwrap();
    });

    let deadline = std::time::Duration::from_secs(10);
    let notified = tokio::time::timeout(deadline, async {
        while let Some(chunk) = stream.next().await {
            let text = String::from_utf8_lossy(&chunk.unwrap()).into_owned();
            if text.contains("notifications/tools/list_changed") {
                return text;
            }
        }
        panic!("流结束但没等到 tools/list_changed");
    })
    .await
    .expect("等待 tools/list_changed 超时——Catalog 重建没能推到订阅者");

    // 通知必须带订阅号，客户端要靠它把通知归到这条 listen 流上。
    assert!(
        notified.contains("io.modelcontextprotocol/subscriptionId"),
        "通知缺 subscriptionId: {notified}"
    );
}

/// ADR-020 升格广播守卫：promote_tool 只通知被升格的那条连接的订阅者；
/// 重复升格同名（幂等）不产生通知。describe→升格的端到端行为见 aggregation.rs
/// `tool_describe_returns_schema_and_promotes`。
#[tokio::test]
async fn tool_promotion_notifies_only_the_promoted_connection() {
    use futures::StreamExt;

    let hub = start().await;
    let response = hub
        .client
        .post(&hub.url)
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {TOKEN}"))
        .header("MCP-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", "subscriptions/listen")
        .json(&json!({
            "jsonrpc":"2.0","id":1,"method":"subscriptions/listen",
            "params":{"notifications":{"toolsListChanged": true}, "_meta": modern_meta()}
        }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let mut stream = response.bytes_stream();
    let ack = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
        .await
        .expect("等待 listen 受理应答超时")
        .expect("流提前结束")
        .unwrap();
    assert!(
        String::from_utf8_lossy(&ack).contains("notifications/subscriptions/acknowledged"),
        "首帧不是受理通知"
    );

    // 别的连接升格：本连接不该收到任何东西。
    assert!(hub.state.promote_tool("client_other", "srv__tool"));
    let leaked = tokio::time::timeout(std::time::Duration::from_millis(500), stream.next()).await;
    assert!(leaked.is_err(), "别的连接的升格串给了本连接: {leaked:?}");

    // 本连接升格：收到 tools/list_changed 且带订阅号。
    assert!(hub.state.promote_tool("client_claude_test", "srv__tool"));
    let notified = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Some(chunk) = stream.next().await {
            let text = String::from_utf8_lossy(&chunk.unwrap()).into_owned();
            if text.contains("notifications/tools/list_changed") {
                return text;
            }
        }
        panic!("流结束但没等到 tools/list_changed");
    })
    .await
    .expect("等待 tools/list_changed 超时——升格没能推到订阅者");
    assert!(
        notified.contains("io.modelcontextprotocol/subscriptionId"),
        "通知缺 subscriptionId: {notified}"
    );

    // 幂等：重复升格同名不再广播。
    assert!(!hub.state.promote_tool("client_claude_test", "srv__tool"));
    let repeated = tokio::time::timeout(std::time::Duration::from_millis(500), stream.next()).await;
    assert!(repeated.is_err(), "幂等升格不该再通知: {repeated:?}");
}

#[tokio::test]
async fn modern_request_without_meta_envelope_is_rejected() {
    let hub = start().await;
    let response = hub
        .post(
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            &[
                ("MCP-Protocol-Version", "2026-07-28"),
                ("Mcp-Method", "tools/list"),
            ],
        )
        .await;
    let code = response["error"]["code"]
        .as_i64()
        .unwrap_or_else(|| panic!("缺 _meta 信封的新协议请求必须被拒绝，实际: {response}"));
    // rmcp 3.1.4：缺/残缺 _meta 一律 JSON-RPC invalid_params（-32602），
    // 不再用 -32020 段或内部错误。码一漂，客户端会把契约违规当成别的故障。
    assert_eq!(
        code, -32602,
        "缺 _meta 必须是 invalid_params，实际: {response}"
    );
    let message = response["error"]["message"].as_str().unwrap_or("");
    assert!(
        message.contains("_meta"),
        "错误信息应指出 _meta，实际: {response}"
    );
}

#[tokio::test]
async fn modern_request_missing_client_capabilities_is_rejected() {
    let hub = start().await;
    let response = hub
        .post(
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list",
            "params":{"_meta":{
                "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                "io.modelcontextprotocol/clientInfo":{"name":"probe","version":"0.0.1"}
            }}}),
            &[
                ("MCP-Protocol-Version", "2026-07-28"),
                ("Mcp-Method", "tools/list"),
            ],
        )
        .await;
    assert_eq!(
        response["error"]["code"], -32602,
        "缺 clientCapabilities 必须是 invalid_params，实际: {response}"
    );
    let message = response["error"]["message"].as_str().unwrap_or("");
    assert!(
        message.contains("io.modelcontextprotocol/clientCapabilities"),
        "错误信息应指出缺的键，实际: {response}"
    );
}

#[tokio::test]
async fn every_legacy_version_still_initializes_unchanged() {
    let hub = start().await;
    for version in LEGACY_VERSIONS {
        let init = hub
            .post(
                json!({"jsonrpc":"2.0","id":1,"method":"initialize",
                       "params":{"protocolVersion": version, "capabilities":{},
                                 "clientInfo":{"name":"probe","version":"0.0.1"}}}),
                &[],
            )
            .await;
        // 协商结果绝不能高于客户端所请求的版本：实测 Claude Code 2.1.224 收到
        // 高于自己请求的版本会直接断连，这是双栈方案的硬约束（ADR-019）。
        assert_eq!(
            init["result"]["protocolVersion"], *version,
            "{version} 的协商结果被改写: {init}"
        );
        // listChanged 靠 subscriptions/listen 兑现，legacy 没有那个方法。
        // 宣告了却永远不推，比不宣告更坏。三类一起裁。
        for kind in ["tools", "prompts", "resources"] {
            assert!(
                init["result"]["capabilities"][kind]["listChanged"].is_null(),
                "{version} 不该看到 {kind} 的 listChanged 声明: {init}"
            );
        }
        assert_no_modern_keys(version, &init);

        let tools = hub
            .post(
                json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
                &[("MCP-Protocol-Version", version)],
            )
            .await;
        assert!(!tools["result"]["tools"].as_array().unwrap().is_empty());
        assert_no_modern_keys(version, &tools);

        let call = hub
            .post(
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
                       "params":{"name":"mcpane__echo","arguments":{"message":"legacy"}}}),
                &[("MCP-Protocol-Version", version)],
            )
            .await;
        assert_eq!(call["result"]["structuredContent"]["message"], "legacy");
        assert_no_modern_keys(version, &call);

        // 其余三个带缓存契约的列表端点同样一个新协议字段都不许漏——
        // apply_cache_hints 的版本门控被谁删了，这里就红给谁看。
        for (id, method) in [
            (4, "prompts/list"),
            (5, "resources/list"),
            (6, "resources/templates/list"),
        ] {
            let listed = hub
                .post(
                    json!({"jsonrpc":"2.0","id":id,"method":method}),
                    &[("MCP-Protocol-Version", version)],
                )
                .await;
            assert!(
                listed["result"].is_object(),
                "{version} 的 {method} 失败: {listed}"
            );
            assert_no_modern_keys(version, &listed);
        }
    }
}

/// 改绑 Workspace 是"工具列表整个换掉"最典型的场景——ADR-004 整篇讲的就是它。
/// 若它不触发 listChanged，那么 ADR-019 用「listChanged 即时通知」替代
/// Session-Pin 的论证就落空了：客户端只能干等 60s 的 ttlMs。
#[tokio::test]
async fn rebinding_a_connection_notifies_its_subscriber() {
    use futures::StreamExt;

    let hub = start().await;
    let response = hub
        .client
        .post(&hub.url)
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {TOKEN}"))
        .header("MCP-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", "subscriptions/listen")
        .json(&json!({
            "jsonrpc":"2.0","id":1,"method":"subscriptions/listen",
            "params":{"notifications":{"toolsListChanged": true}, "_meta": modern_meta()}
        }))
        .send()
        .await
        .unwrap();
    let mut stream = response.bytes_stream();
    let ack = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
        .await
        .expect("等待受理超时")
        .expect("流提前结束")
        .unwrap();
    assert!(String::from_utf8_lossy(&ack).contains("acknowledged"));

    // 把这条连接改绑到另一个 Workspace：它能看见的工具从此完全不同。
    hub.state
        .set_connection_workspaces(std::collections::HashMap::from([(
            "client_claude_test".to_owned(),
            "ws_other".to_owned(),
        )]));

    let notified = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Some(chunk) = stream.next().await {
            let text = String::from_utf8_lossy(&chunk.unwrap()).into_owned();
            if text.contains("notifications/tools/list_changed") {
                return true;
            }
        }
        false
    })
    .await;
    assert!(
        matches!(notified, Ok(true)),
        "改绑 Workspace 后订阅者没收到 tools/list_changed，只能等 ttlMs 过期"
    );
}

/// 访问控制启停与改绑是同一类"连接可见范围变了"的事件——撤销某连接对某
/// Server 的访问后，它的工具列表立即少一截，订阅者必须马上知道，而不是
/// 干等 ttlMs 过期（call 时校验只能挡调用，挡不住客户端拿着过期列表规划）。
#[tokio::test]
async fn access_control_change_notifies_only_the_affected_connection() {
    use futures::StreamExt;

    let hub = start().await;
    let subscribe = |hub: &Hub| {
        let client = hub.client.clone();
        let url = hub.url.clone();
        async move {
            let response = client
                .post(&url)
                .header("Accept", "application/json, text/event-stream")
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {TOKEN}"))
                .header("MCP-Protocol-Version", "2026-07-28")
                .header("Mcp-Method", "subscriptions/listen")
                .json(&json!({
                    "jsonrpc":"2.0","id":1,"method":"subscriptions/listen",
                    "params":{"notifications":{"toolsListChanged": true}, "_meta": modern_meta()}
                }))
                .send()
                .await
                .unwrap();
            let mut stream = response.bytes_stream();
            let ack = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
                .await
                .expect("等待受理超时")
                .expect("流提前结束")
                .unwrap();
            assert!(String::from_utf8_lossy(&ack).contains("acknowledged"));
            stream
        }
    };
    let mut stream = subscribe(&hub).await;

    // 别的连接被改访问控制：与本连接无关，不得串扰。
    hub.state
        .set_server_denied("client_other_test", "srv_x", true);
    let leaked = tokio::time::timeout(std::time::Duration::from_millis(500), stream.next()).await;
    assert!(leaked.is_err(), "别的连接的访问控制变更串给了本连接");

    // 本连接被拒绝访问某 Server：必须立刻收到通知。
    hub.state
        .set_server_denied("client_claude_test", "srv_x", true);
    let notified = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(chunk) = stream.next().await {
            if String::from_utf8_lossy(&chunk.unwrap()).contains("notifications/tools/list_changed")
            {
                return true;
            }
        }
        false
    })
    .await;
    assert!(
        matches!(notified, Ok(true)),
        "访问控制变更后订阅者没收到 tools/list_changed"
    );

    // 幂等操作（重复拒绝同一个）没有实际变化，不该再发一轮。
    hub.state
        .set_server_denied("client_claude_test", "srv_x", true);
    let noop = tokio::time::timeout(std::time::Duration::from_millis(500), stream.next()).await;
    assert!(noop.is_err(), "无实际变化的幂等操作不该触发通知");
}
