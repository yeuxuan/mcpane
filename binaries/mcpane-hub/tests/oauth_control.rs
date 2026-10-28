//! 远程 MCP OAuth 经 Hub 控制面的端到端（ADR-024）：
//! 装载受保护的服务器 → needs-auth（不重连循环）→ begin / 浏览器同意 / complete →
//! 连上并出工具 → 退出登录回到 needs-auth → 服务器被删时回收令牌文件。
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use mcpane_hub::state::{AggregationState, ServerStatus};

#[path = "../../../crates/mcpane-hub-downstream/tests/common/oauth_fake.rs"]
mod fake;

const SERVER_ID: &str = "srv_remote";
const REDIRECT: &str = "http://127.0.0.1:9/callback";

async fn remote_server(url: &str) -> (mcpane_storage::SqlitePool, mcpane_storage::ServerRepo) {
    let pool = mcpane_storage::open_in_memory().await.unwrap();
    let repo = mcpane_storage::ServerRepo::new(pool.clone());
    repo.upsert(mcpane_storage::McpServerDef {
        id: SERVER_ID.into(),
        slug: "remote".into(),
        name: "Remote".into(),
        transport: mcpane_storage::ServerTransport::Http,
        command: None,
        args: vec![],
        env: BTreeMap::new(),
        cwd: None,
        url: Some(url.to_owned()),
        bearer: None,
        bearer_env: None,
        headers: BTreeMap::new(),
        runtime_policy: "shared".into(),
        enabled: true,
    })
    .await
    .unwrap();
    (pool, repo)
}

fn entry(state: &AggregationState) -> serde_json::Value {
    state.diagnostics()["downstreams"][SERVER_ID].clone()
}

#[tokio::test]
async fn oauth_flow_through_the_control_plane() {
    let (fake, url) = fake::spawn_fake().await;
    let (pool, repo) = remote_server(&url).await;
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(AggregationState::new(None));
    state.set_oauth_dir(dir.path().to_path_buf());

    // 1. 装载：受保护的服务器回 401 → needs-auth，不是 failed；诊断带原因与登录态。
    mcpane_hub::control::load_downstreams(&state, &pool).await;
    assert_eq!(
        state.registered(SERVER_ID).unwrap().status(),
        ServerStatus::NeedsAuth
    );
    let diagnostics = entry(&state);
    assert_eq!(diagnostics["status"], "needs-auth");
    assert_eq!(diagnostics["lastErrorKind"], "needs_auth");
    assert_eq!(diagnostics["signedIn"], false);
    assert!(state.downstream(SERVER_ID).is_none(), "没登录就没有句柄");

    // 2. 健康监控不对 needs-auth 反复重连：等几轮，服务器收到的请求数不再增长。
    state.spawn_health_monitor(Duration::from_millis(40));
    tokio::time::sleep(Duration::from_millis(60)).await;
    let before = fake.mcp_hits.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        fake.mcp_hits.load(Ordering::SeqCst),
        before,
        "需要登录期间不该再撞服务器"
    );

    // 3. 登录：begin → 浏览器同意 → complete，成功后自动重连。
    let begun = mcpane_hub::control::oauth_begin(&state, SERVER_ID, REDIRECT).await;
    assert_eq!(begun["ok"], true, "{begun}");
    let auth_url = begun["authorizationUrl"].as_str().unwrap();
    let (code, oauth_state) = fake::approve_in_browser(auth_url).await;
    let done =
        mcpane_hub::control::oauth_complete(&state, &pool, SERVER_ID, &code, &oauth_state).await;
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(done["status"], "ready");
    assert_eq!(entry(&state)["signedIn"], true);
    let tools = state
        .downstream(SERVER_ID)
        .unwrap()
        .list_tools()
        .await
        .unwrap();
    assert!(tools.iter().any(|t| t.name == "echo"));

    // 4. 退出登录：令牌删除，服务器回到 needs-auth（旧连接不得继续用已删的令牌）。
    let out = mcpane_hub::control::oauth_logout(&state, &pool, SERVER_ID).await;
    assert_eq!(out["ok"], true);
    assert_eq!(
        state.registered(SERVER_ID).unwrap().status(),
        ServerStatus::NeedsAuth
    );
    assert_eq!(entry(&state)["signedIn"], false);

    // 5. 再登录一次，然后把服务器从库里删掉：令牌文件被回收，不留在盘上。
    let begun = mcpane_hub::control::oauth_begin(&state, SERVER_ID, REDIRECT).await;
    let (code, oauth_state) =
        fake::approve_in_browser(begun["authorizationUrl"].as_str().unwrap()).await;
    mcpane_hub::control::oauth_complete(&state, &pool, SERVER_ID, &code, &oauth_state).await;
    let token_file = state.oauth().spec(SERVER_ID).credentials_file();
    assert!(token_file.is_file());
    repo.delete(SERVER_ID).await.unwrap();
    mcpane_hub::control::load_downstreams(&state, &pool).await;
    assert!(!token_file.exists(), "孤儿令牌应被回收");
}

/// 对没登录过 / 不存在的服务器发起登录要如实报错，不 panic。
#[tokio::test]
async fn begin_for_unknown_or_non_http_server_is_refused() {
    let state = Arc::new(AggregationState::new(None));
    let unknown = mcpane_hub::control::oauth_begin(&state, "nope", REDIRECT).await;
    assert_eq!(unknown["ok"], false);
    assert!(unknown["error"].as_str().unwrap().contains("找不到"));
}
