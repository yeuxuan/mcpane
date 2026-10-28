//! 下游远程 MCP 的 OAuth 端到端（ADR-024）：
//! 假授权服务器（元数据发现、动态注册、授权码 + PKCE、刷新令牌）+ 受令牌保护的 MCP 服务器。
//! 「浏览器」由测试代劳：对授权链接发一次不跟随重定向的请求，从 `Location` 取回调参数。
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::time::Duration;

use mcpane_hub_downstream::oauth::{OAuthLogins, OAuthSpec};
use mcpane_hub_downstream::{DownstreamError, DownstreamHandle, SpawnSpec};
use mcpane_net::ProxyConfig;

#[path = "common/oauth_fake.rs"]
mod fake;
use fake::{approve_in_browser, spawn_fake};

// ---------- 测试辅助 ----------

const SERVER_ID: &str = "srv_oauth";
const REDIRECT: &str = "http://127.0.0.1:9/callback";

fn spec(url: &str, dir: &std::path::Path) -> SpawnSpec {
    SpawnSpec::Http {
        url: url.to_owned(),
        bearer: None,
        bearer_env: None,
        env: BTreeMap::new(),
        headers: BTreeMap::new(),
        oauth: Some(OAuthSpec {
            dir: dir.to_path_buf(),
            server_id: SERVER_ID.to_owned(),
        }),
    }
}

async fn connect(spec: &SpawnSpec) -> Result<DownstreamHandle, DownstreamError> {
    DownstreamHandle::connect(spec, Duration::from_secs(15), &ProxyConfig::default()).await
}

/// 完整走一遍登录：未登录连接拿到挑战 → begin → 浏览器同意 → complete。
async fn log_in(logins: &OAuthLogins, spec: &SpawnSpec, url: &str) {
    let Err(DownstreamError::AuthRequired { challenge }) = connect(spec).await.map(|_| ()) else {
        panic!("未登录时应报需要登录");
    };
    let auth_url = logins
        .begin(SERVER_ID, url, challenge, REDIRECT, &ProxyConfig::default())
        .await
        .unwrap();
    let (code, state) = approve_in_browser(&auth_url).await;
    logins.complete(SERVER_ID, &code, &state).await.unwrap();
}

// ---------- 用例 ----------

/// 受保护的服务器回 401：不是普通故障，要带着挑战上报「需要登录」。
#[tokio::test]
async fn unauthenticated_connect_reports_auth_required_with_the_challenge() {
    let (_fake, url) = spawn_fake().await;
    let dir = tempfile::tempdir().unwrap();
    let error = connect(&spec(&url, dir.path())).await.err().unwrap();
    assert!(error.needs_auth(), "{error}");
    let DownstreamError::AuthRequired { challenge } = error else {
        panic!()
    };
    assert!(
        challenge.unwrap().contains("resource_metadata"),
        "挑战要带受保护资源元数据地址，登录才能据此发现授权服务器"
    );
}

/// 登录一次：发现 → 动态注册 → PKCE → 换令牌 → 落盘；之后连接带着令牌通过，工具可用。
#[tokio::test]
async fn login_then_connect_lists_the_protected_tools() {
    let (fake, url) = spawn_fake().await;
    let dir = tempfile::tempdir().unwrap();
    let logins = OAuthLogins::new(dir.path().to_path_buf());
    let spec = spec(&url, dir.path());
    log_in(&logins, &spec, &url).await;

    let handle = connect(&spec)
        .await
        .unwrap_or_else(|e| panic!("登录后应连得上：{e}"));
    let tools = handle.list_tools().await.unwrap();
    assert!(tools.iter().any(|t| t.name == "echo"), "{tools:?}");
    assert_eq!(fake.registrations.load(Ordering::SeqCst), 1);
    assert_eq!(*fake.grants.lock().unwrap(), vec!["authorization_code"]);
}

/// 令牌文件仅属主可读写，且不含在别处（只在 OAuth 目录里）。
#[cfg(unix)]
#[tokio::test]
async fn token_file_is_private_after_login() {
    use std::os::unix::fs::PermissionsExt;
    let (_fake, url) = spawn_fake().await;
    let dir = tempfile::tempdir().unwrap();
    let logins = OAuthLogins::new(dir.path().join("mcp-oauth"));
    let spec = spec(&url, &dir.path().join("mcp-oauth"));
    log_in(&logins, &spec, &url).await;
    let file = logins.spec(SERVER_ID).credentials_file();
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("access_token"), "令牌确实落盘了");
}

/// 令牌临近过期：连接时自动用 refresh_token 换新，用户无感。
#[tokio::test]
async fn expiring_token_is_refreshed_transparently() {
    let (fake, url) = spawn_fake().await;
    fake.expires_in.store(5, Ordering::SeqCst); // 低于 SDK 的 30 秒刷新缓冲
    let dir = tempfile::tempdir().unwrap();
    let logins = OAuthLogins::new(dir.path().to_path_buf());
    let spec = spec(&url, dir.path());
    log_in(&logins, &spec, &url).await;

    let handle = connect(&spec)
        .await
        .unwrap_or_else(|e| panic!("过期前应已自动刷新：{e}"));
    assert!(
        handle
            .list_tools()
            .await
            .unwrap()
            .iter()
            .any(|t| t.name == "echo")
    );
    assert!(
        fake.grants
            .lock()
            .unwrap()
            .iter()
            .any(|g| g == "refresh_token"),
        "{:?}",
        fake.grants.lock().unwrap()
    );
}

/// 刷新令牌被授权服务器明确拒绝：只能重新登录，要上报「需要登录」而不是笼统失败。
#[tokio::test]
async fn rejected_refresh_token_falls_back_to_needs_login() {
    let (fake, url) = spawn_fake().await;
    fake.expires_in.store(5, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let logins = OAuthLogins::new(dir.path().to_path_buf());
    let spec = spec(&url, dir.path());
    log_in(&logins, &spec, &url).await;
    fake.refresh_rejects.store(true, Ordering::SeqCst);

    let error = connect(&spec)
        .await
        .err()
        .unwrap_or_else(|| panic!("刷新被拒应连不上"));
    assert!(error.needs_auth(), "{error}");
}

/// 回调里的 state 对不上（可能是伪造的回调）：拒绝，且不落盘任何令牌。
#[tokio::test]
async fn forged_state_is_rejected_and_stores_nothing() {
    let (_fake, url) = spawn_fake().await;
    let dir = tempfile::tempdir().unwrap();
    let logins = OAuthLogins::new(dir.path().to_path_buf());
    let auth_url = logins
        .begin(SERVER_ID, &url, None, REDIRECT, &ProxyConfig::default())
        .await
        .unwrap();
    let (code, _state) = approve_in_browser(&auth_url).await;
    let error = logins
        .complete(SERVER_ID, &code, "not-the-state-we-issued")
        .await
        .unwrap_err();
    assert!(matches!(error, DownstreamError::Auth(_)), "{error}");
    assert!(!logins.spec(SERVER_ID).signed_in());
}

/// 退出登录：令牌文件删除，之后连接回到「需要登录」。
#[tokio::test]
async fn logout_returns_to_needs_login() {
    let (_fake, url) = spawn_fake().await;
    let dir = tempfile::tempdir().unwrap();
    let logins = OAuthLogins::new(dir.path().to_path_buf());
    let spec = spec(&url, dir.path());
    log_in(&logins, &spec, &url).await;
    assert!(connect(&spec).await.is_ok());

    logins.logout(SERVER_ID);
    assert!(!logins.spec(SERVER_ID).signed_in());
    assert!(connect(&spec).await.err().unwrap().needs_auth());
}

/// 用户手配了 Bearer：以它为准、不走 OAuth；被 401 时说的是「配置的凭据被拒」，
/// 不是「需要登录」（登录帮不上忙）。
#[tokio::test]
async fn configured_bearer_wins_and_a_401_blames_the_credential() {
    let (_fake, url) = spawn_fake().await;
    let dir = tempfile::tempdir().unwrap();
    let SpawnSpec::Http {
        url,
        env,
        headers,
        oauth,
        ..
    } = spec(&url, dir.path())
    else {
        unreachable!()
    };
    let with_bearer = SpawnSpec::Http {
        url,
        bearer: Some("not-a-valid-token".into()),
        bearer_env: None,
        env,
        headers,
        oauth,
    };
    let error = connect(&with_bearer).await.err().unwrap();
    assert!(!error.needs_auth(), "{error}");
    assert!(error.to_string().contains("已配置的认证信息"), "{error}");
}
