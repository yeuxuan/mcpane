//! 假 OAuth 授权服务器 + 受令牌保护的 MCP 服务器（ADR-024 的测试夹具）。
//! 被 `mcpane-hub-downstream` 与 `mcpane-hub` 两个测试 crate 以 `#[path]` 共用。
#![allow(dead_code, clippy::unwrap_used)]

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use base64::Engine;
use rmcp::handler::server::wrapper::{Json as ToolJson, Parameters};
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

// ---------- 受保护的 MCP 服务器 ----------

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
struct EchoParams {
    text: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct EchoOutput {
    text: String,
}

#[derive(Clone, Default)]
struct Protected;

#[tool_router]
impl Protected {
    #[tool(name = "echo", description = "echo behind OAuth")]
    fn echo(
        &self,
        Parameters(EchoParams { text }): Parameters<EchoParams>,
    ) -> ToolJson<EchoOutput> {
        ToolJson(EchoOutput { text })
    }
}

#[tool_handler]
impl ServerHandler for Protected {
    /// 钉成 legacy：与现有 HTTP 下游夹具一致，握手走 initialize。
    fn supported_protocol_versions(
        &self,
    ) -> std::borrow::Cow<'static, [rmcp::model::ProtocolVersion]> {
        std::borrow::Cow::Borrowed(&[rmcp::model::ProtocolVersion::V_2025_11_25])
    }

    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }
}

// ---------- 假授权服务器 ----------

#[derive(Default)]
pub struct Fake {
    pub base: Mutex<String>,
    /// 当前有效的 access token。
    pub tokens: Mutex<HashSet<String>>,
    /// 授权码 → (code_challenge, redirect_uri)。
    pub codes: Mutex<HashMap<String, (String, String)>>,
    /// 下发令牌的 `expires_in`（秒）；0 = 一小时。
    pub expires_in: AtomicU64,
    pub refresh_rejects: std::sync::atomic::AtomicBool,
    pub grants: Mutex<Vec<String>>,
    pub issued: AtomicUsize,
    pub registrations: AtomicUsize,
    /// MCP 端点（受保护）收到的请求数，含被 401 的。
    pub mcp_hits: AtomicUsize,
}

impl Fake {
    pub fn base(&self) -> String {
        self.base.lock().unwrap().clone()
    }

    fn issue(&self) -> Value {
        let n = self.issued.fetch_add(1, Ordering::SeqCst) + 1;
        let token = format!("at-{n}");
        self.tokens.lock().unwrap().insert(token.clone());
        let ttl = match self.expires_in.load(Ordering::SeqCst) {
            0 => 3600,
            secs => secs,
        };
        json!({
            "access_token": token,
            "token_type": "Bearer",
            "expires_in": ttl,
            "refresh_token": format!("rt-{n}"),
        })
    }
}

/// 调试端点：独立运行（examples/oauth_fake_server）时用来观察授权服务器收到过什么。
async fn debug_state(State(fake): State<Arc<Fake>>) -> Json<Value> {
    Json(json!({
        "grants": *fake.grants.lock().unwrap(),
        "registrations": fake.registrations.load(Ordering::SeqCst),
        "mcpHits": fake.mcp_hits.load(Ordering::SeqCst),
        "issued": fake.issued.load(Ordering::SeqCst),
    }))
}

async fn protected_resource(State(fake): State<Arc<Fake>>) -> Json<Value> {
    let base = fake.base();
    Json(json!({
        "resource": format!("{base}/mcp"),
        "authorization_servers": [base],
        "bearer_methods_supported": ["header"],
    }))
}

async fn authorization_server(State(fake): State<Arc<Fake>>) -> Json<Value> {
    let base = fake.base();
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "registration_endpoint": format!("{base}/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
    }))
}

async fn register(State(fake): State<Arc<Fake>>, Json(body): Json<Value>) -> Response {
    fake.registrations.fetch_add(1, Ordering::SeqCst);
    (
        StatusCode::CREATED,
        Json(json!({
            "client_id": "test-client",
            "client_name": "MCPane",
            "redirect_uris": body["redirect_uris"],
            "token_endpoint_auth_method": "none",
        })),
    )
        .into_response()
}

async fn authorize(
    State(fake): State<Arc<Fake>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["code_challenge_method"], "S256", "必须用 PKCE S256");
    assert_eq!(query["client_id"], "test-client");
    let code = format!("code-{}", fake.codes.lock().unwrap().len() + 1);
    fake.codes.lock().unwrap().insert(
        code.clone(),
        (
            query["code_challenge"].clone(),
            query["redirect_uri"].clone(),
        ),
    );
    let location = format!(
        "{}?code={code}&state={}",
        query["redirect_uri"], query["state"]
    );
    (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
}

fn invalid_grant() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": "invalid_grant"})),
    )
        .into_response()
}

async fn token(
    State(fake): State<Arc<Fake>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let grant = form["grant_type"].clone();
    fake.grants.lock().unwrap().push(grant.clone());
    match grant.as_str() {
        "authorization_code" => {
            let Some((challenge, redirect)) = fake.codes.lock().unwrap().remove(&form["code"])
            else {
                return invalid_grant();
            };
            let verifier = form.get("code_verifier").cloned().unwrap_or_default();
            let digest = Sha256::digest(verifier.as_bytes());
            let computed = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
            if computed != challenge || form["redirect_uri"] != redirect {
                return invalid_grant();
            }
            Json(fake.issue()).into_response()
        }
        "refresh_token" => {
            if fake.refresh_rejects.load(Ordering::SeqCst) {
                return invalid_grant();
            }
            Json(fake.issue()).into_response()
        }
        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "unsupported_grant_type"})),
        )
            .into_response(),
    }
}

/// 无令牌（或令牌无效）回 401，并在 `WWW-Authenticate` 里指路受保护资源元数据。
async fn guard(
    State(fake): State<Arc<Fake>>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    fake.mcp_hits.fetch_add(1, Ordering::SeqCst);
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if bearer.is_some_and(|t| fake.tokens.lock().unwrap().contains(t)) {
        return next.run(request).await;
    }
    (
        StatusCode::UNAUTHORIZED,
        [(
            header::WWW_AUTHENTICATE,
            format!(
                "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\"",
                fake.base()
            ),
        )],
    )
        .into_response()
}

/// 对 `server/discover` 回 -32601，让客户端回退 legacy initialize（同现有 HTTP 下游夹具）。
async fn legacy_discover_gate(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let (parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    if let Ok(message) = serde_json::from_slice::<Value>(&bytes)
        && message["method"] == "server/discover"
    {
        return Json(json!({
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

/// 起假服务；返回（假服务状态，MCP 端点 URL）。
pub async fn spawn_fake() -> (Arc<Fake>, String) {
    let fake = Arc::new(Fake::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    *fake.base.lock().unwrap() = base.clone();

    let service = StreamableHttpService::new(
        || Ok(Protected),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default().with_legacy_session_mode(true),
    );
    let mcp = Router::new()
        .nest_service("/mcp", axum::routing::any_service(service))
        .layer(axum::middleware::from_fn(legacy_discover_gate))
        .layer(axum::middleware::from_fn_with_state(fake.clone(), guard));
    let app = Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_resource),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(protected_resource),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_server),
        )
        .route("/__debug", get(debug_state))
        .route("/register", post(register))
        .route("/authorize", get(authorize))
        .route("/token", post(token))
        .with_state(fake.clone())
        .merge(mcp);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (fake, format!("{base}/mcp"))
}

/// 「浏览器」：打开授权链接，不跟随重定向，从 `Location` 取回 `code` 与 `state`。
pub async fn approve_in_browser(auth_url: &str) -> (String, String) {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let response = client.get(auth_url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::FOUND, "{auth_url}");
    let location = response.headers()[reqwest::header::LOCATION]
        .to_str()
        .unwrap();
    let url = url::Url::parse(location).unwrap();
    let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
    (query["code"].clone(), query["state"].clone())
}
