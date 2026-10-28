//! Bearer Token 认证（ADR-003）。
//!
//! - 内存中只保存 Token 的 SHA-256 哈希与可识别前缀，不保存明文；
//! - 哈希比较使用常量时间算法，防时序侧信道；
//! - 认证通过后向请求注入 [`ClientIdentity`]，供 Tool 层读取。

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::Response,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// 客户端类型（覆盖全部可发放 Token 的客户端；未知归 custom）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "lowercase")]
pub enum ClientType {
    Claude,
    /// 变体名 lowercase 会拼成 `claudedesktop`，但客户端 id 带连字符，显式改回。
    #[serde(rename = "claude-desktop")]
    ClaudeDesktop,
    Codex,
    #[serde(rename = "gemini-cli")]
    GeminiCli,
    Antigravity,
    Cursor,
    Vscode,
    Cline,
    Devin,
    Trae,
    Qoder,
    #[serde(rename = "qwen-code")]
    QwenCode,
    Codebuddy,
    Zcode,
    Custom,
}

impl ClientType {
    /// 全部内置客户端（不含 Custom）。新接一个客户端就加进这里——
    /// 测试遍历它做 parse/serde 往返，漏配 parse 分支会被逮住。
    ///
    /// 事实源在桌面端的 `mcp_clients::CLIENTS`（加上 claude / codex 两个专用
    /// adapter），但那是另一个 crate，钉不进类型系统：只能靠下面那条数量断言
    /// 把人拦一次。漏了不会报错，只会让该客户端的连接在界面上显示成 "custom"。
    const BUILTIN: &'static [Self] = &[
        Self::Claude,
        Self::ClaudeDesktop,
        Self::Codex,
        Self::GeminiCli,
        Self::Antigravity,
        Self::Cursor,
        Self::Vscode,
        Self::Cline,
        Self::Devin,
        Self::Trae,
        Self::Qoder,
        Self::QwenCode,
        Self::Codebuddy,
        Self::Zcode,
    ];

    /// 存储层 `client_type` 的取值。写成穷尽 match 而不是给 `parse` 加一条
    /// `_ => Custom` 兜底：新增变体时漏在这里会**编译失败**，而漏在带兜底的
    /// parse 里只会静默降级成 custom，要等用户在客户端里看到自己被标成
    /// "custom" 才发现。
    const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::ClaudeDesktop => "claude-desktop",
            Self::Codex => "codex",
            Self::GeminiCli => "gemini-cli",
            Self::Antigravity => "antigravity",
            Self::Cursor => "cursor",
            Self::Vscode => "vscode",
            Self::Cline => "cline",
            Self::Devin => "devin",
            Self::Trae => "trae",
            Self::Qoder => "qoder",
            Self::QwenCode => "qwen-code",
            Self::Codebuddy => "codebuddy",
            Self::Zcode => "zcode",
            Self::Custom => "custom",
        }
    }

    /// 从存储层的 client_type 字符串解析（与 serde 表示一致）；认不出归 Custom。
    pub fn parse(value: &str) -> Self {
        Self::BUILTIN
            .iter()
            .copied()
            .find(|variant| variant.as_str() == value)
            .unwrap_or(Self::Custom)
    }
}

/// 认证通过后注入请求扩展的客户端身份。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientIdentity {
    pub client_connection_id: String,
    pub client_type: ClientType,
    /// 库里存的原样 `client_type` 字符串。
    ///
    /// 枚举把认不出的一律折成 `Custom`，但用户自建客户端的名字恰恰只在这个
    /// 字符串里——不留着它，项目页与访问控制里几个自定义客户端就全叫 "custom"，
    /// 谁也分不清哪条连接是谁的。
    pub client_type_raw: String,
}

impl ClientIdentity {
    /// 从库里的原样字符串构造（枚举与原文同时留下）。
    pub fn from_raw(client_connection_id: String, raw: &str) -> Self {
        Self {
            client_connection_id,
            client_type: ClientType::parse(raw),
            client_type_raw: raw.to_owned(),
        }
    }
}

/// 单个已注册 Token（只存哈希与前缀）。
struct TokenEntry {
    token_hash: [u8; 32],
    token_prefix: String,
    identity: ClientIdentity,
    enabled: bool,
}

/// Token 注册表（内部可变，支持运行时撤销/恢复）。
/// TASK-401 会替换为 SQLite 持久化实现。
pub struct TokenRegistry {
    entries: std::sync::RwLock<Vec<TokenEntry>>,
}

impl TokenRegistry {
    pub fn new() -> Self {
        Self {
            entries: std::sync::RwLock::new(Vec::new()),
        }
    }

    fn entries_read(&self) -> std::sync::RwLockReadGuard<'_, Vec<TokenEntry>> {
        self.entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn entries_write(&self) -> std::sync::RwLockWriteGuard<'_, Vec<TokenEntry>> {
        self.entries
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 注册一个 Token，只保留哈希与前 8 位前缀。
    pub fn register(&self, token: &str, identity: ClientIdentity) {
        self.entries_write().push(TokenEntry {
            token_hash: hash_token(token),
            token_prefix: token.chars().take(8).collect(),
            identity,
            enabled: true,
        });
    }

    /// 以哈希（hex）直接注册——SQLite 持久层水合用（明文永不出库）。
    /// 返回 false 表示哈希格式非法。
    pub fn register_hash(
        &self,
        token_hash_hex: &str,
        token_prefix: &str,
        identity: ClientIdentity,
        enabled: bool,
    ) -> bool {
        let Some(token_hash) = hex_to_hash(token_hash_hex) else {
            return false;
        };
        self.entries_write().push(TokenEntry {
            token_hash,
            token_prefix: token_prefix.to_owned(),
            identity,
            enabled,
        });
        true
    }

    /// 清空后重新水合（SQLite 变更后调用；dev 环境变量 Token 由调用方重注册）。
    pub fn clear(&self) {
        self.entries_write().clear();
    }

    /// 常量时间验证：对每个条目都执行完整比较，不提前短路；
    /// 已撤销条目同样参与比较但不产生匹配结果。
    pub fn verify(&self, token: &str) -> Option<ClientIdentity> {
        let candidate = hash_token(token);
        let mut matched: Option<ClientIdentity> = None;
        for entry in self.entries_read().iter() {
            let hash_eq: bool = entry.token_hash.ct_eq(&candidate).into();
            if hash_eq && entry.enabled {
                matched = Some(entry.identity.clone());
            }
        }
        matched
    }

    /// 按连接 ID 撤销/恢复 Token。返回是否命中条目。
    pub fn set_enabled(&self, client_connection_id: &str, enabled: bool) -> bool {
        let mut hit = false;
        for entry in self.entries_write().iter_mut() {
            if entry.identity.client_connection_id == client_connection_id {
                entry.enabled = enabled;
                hit = true;
            }
        }
        if hit {
            tracing::warn!(client_connection_id, enabled, "客户端连接 Token 状态已变更");
        }
        hit
    }

    /// 控制面状态展示用：连接 ID / 类型 / Token 前缀 / 启用态（永不返回完整 Token）。
    ///
    /// 类型给的是**原样字符串**而非折过的枚举——UI 要靠它区分几个自定义客户端。
    pub fn connection_summaries(&self) -> Vec<(String, String, String, bool)> {
        self.entries_read()
            .iter()
            .map(|e| {
                (
                    e.identity.client_connection_id.clone(),
                    e.identity.client_type_raw.clone(),
                    e.token_prefix.clone(),
                    e.enabled,
                )
            })
            .collect()
    }
}

impl Default for TokenRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn hash_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn hex_to_hash(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let text = std::str::from_utf8(chunk).ok()?;
        out[i] = u8::from_str_radix(text, 16).ok()?;
    }
    Some(out)
}

/// 认证失败限速（TASK-401）：连续失败达到阈值后短暂锁定，
/// 防 Token 暴力猜测（数据面仅 loopback，全局计数已足够）。
pub struct AuthRateLimiter {
    threshold: u32,
    lockout: std::time::Duration,
    failures: std::sync::atomic::AtomicU32,
    locked_until: std::sync::Mutex<Option<std::time::Instant>>,
}

impl AuthRateLimiter {
    pub fn new(threshold: u32, lockout: std::time::Duration) -> Self {
        Self {
            threshold,
            lockout,
            failures: std::sync::atomic::AtomicU32::new(0),
            locked_until: std::sync::Mutex::new(None),
        }
    }

    /// 请求前检查：处于锁定期则拒绝。
    pub fn check(&self) -> bool {
        let mut guard = self
            .locked_until
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match *guard {
            Some(until) if std::time::Instant::now() < until => false,
            Some(_) => {
                *guard = None;
                self.failures.store(0, std::sync::atomic::Ordering::SeqCst);
                true
            }
            None => true,
        }
    }

    pub fn record_failure(&self) {
        let failures = self
            .failures
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        if failures >= self.threshold {
            let mut guard = self
                .locked_until
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *guard = Some(std::time::Instant::now() + self.lockout);
            tracing::warn!(
                failures,
                lockout_secs = self.lockout.as_secs(),
                "认证失败达到阈值，进入锁定"
            );
        }
    }

    pub fn record_success(&self) {
        self.failures.store(0, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Default for AuthRateLimiter {
    fn default() -> Self {
        Self::new(10, std::time::Duration::from_secs(30))
    }
}

/// 使用系统安全随机源生成 256-bit Token（唯一实现在 mcpane-storage）。
pub use mcpane_storage::generate_token;

/// 认证中间件共享状态。
#[derive(Clone)]
pub struct AuthState {
    pub registry: Arc<TokenRegistry>,
    pub limiter: Arc<AuthRateLimiter>,
}

/// Bearer 认证中间件：POST/GET/DELETE 全部生效（挂在 /mcp 整个子服务上）。
/// 连续认证失败触发限速（429）。
pub async fn bearer_auth_middleware(
    State(auth): State<AuthState>,
    mut request: Request,
    next: Next,
) -> Result<Response, Response> {
    if !auth.limiter.check() {
        return Err(too_many_requests());
    }

    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| !t.is_empty());

    let Some(token) = token else {
        auth.limiter.record_failure();
        return Err(unauthorized("missing bearer token"));
    };

    match auth.registry.verify(token) {
        Some(identity) => {
            auth.limiter.record_success();
            tracing::debug!(
                client_connection_id = %identity.client_connection_id,
                client_type = ?identity.client_type,
                "bearer auth ok"
            );
            request.extensions_mut().insert(identity);
            Ok(next.run(request).await)
        }
        None => {
            auth.limiter.record_failure();
            // 前缀粒度与注册表一致（8 位），便于对照凭证页定位；永不记完整 Token。
            let attempted_prefix: String = token.chars().take(8).collect();
            tracing::warn!(%attempted_prefix, "bearer 认证失败：Token 未注册");
            Err(unauthorized(invalid_token_reason(token)))
        }
    }
}

/// 401 原因文案：识别两类"根本不是 Token"的字面量并给出定向修复提示——
/// `$` 开头是未展开的环境变量引用（shell 没加载 MCPANE_*），`<` 开头是
/// 配置模板占位符没替换（Cline 等不支持变量展开的客户端）。
/// 合法 Token 是纯 hex，两个前缀都零误报。
fn invalid_token_reason(token: &str) -> &'static str {
    if token.starts_with('$') {
        "bearer token is an unexpanded ${...} placeholder — the shell that launched the client \
         never loaded the MCPANE_* environment variable; start the client from a new terminal and retry"
    } else if token.starts_with('<') {
        "bearer token is still the config template placeholder — issue a token from the client \
         card's credentials page in MCPane and paste it into the config"
    } else {
        "invalid bearer token"
    }
}

fn too_many_requests() -> Response {
    let mut response = Response::new(
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": { "code": -32003, "message": "too many failed authentication attempts" }
        })
        .to_string()
        .into(),
    );
    *response.status_mut() = StatusCode::TOO_MANY_REQUESTS;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("30"));
    response
}

fn unauthorized(reason: &str) -> Response {
    // 不回显任何 Token 内容，只给出静态原因。
    let mut response = Response::new(
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": { "code": -32001, "message": format!("unauthorized: {reason}") }
        })
        .to_string()
        .into(),
    );
    *response.status_mut() = StatusCode::UNAUTHORIZED;
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer realm=\"mcpane-hub\""),
    );
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// parse 与 serde 表示必须互为逆运算——漏一个就静默落到 Custom，
    /// `connection_info` 把内置客户端报成 "custom"，Hub 日志里也分不出是谁。
    ///
    /// 遍历 BUILTIN 而不是抄一份 id 清单：抄漏的那一条正是会静默降级的那一条。
    #[test]
    fn client_type_parse_and_serde_round_trip_for_every_builtin() {
        for &variant in ClientType::BUILTIN {
            let id = variant.as_str();
            assert_eq!(ClientType::parse(id), variant, "{id} 往返不一致");
            // serde 表示是对外契约（connection_info 的返回值），rename 写错
            // 这里就会不一致——as_str 是唯一事实源。
            assert_eq!(
                serde_json::to_value(variant).unwrap(),
                serde_json::json!(id),
                "{id} 的 serde 表示与 as_str 不一致"
            );
        }
        // 客户端页当前能发 Token 的内置客户端数量（含 claude / codex 两个专用
        // adapter）。新接客户端时同步这个数字，顺带提醒把变体加进 BUILTIN。
        assert_eq!(ClientType::BUILTIN.len(), 14);
        assert_eq!(ClientType::parse("zed"), ClientType::Custom);
    }

    fn registry() -> TokenRegistry {
        let r = TokenRegistry::new();
        r.register(
            "token-claude-abcdef",
            ClientIdentity::from_raw("client_claude".into(), "claude"),
        );
        r.register(
            "token-codex-123456",
            ClientIdentity::from_raw("client_codex".into(), "codex"),
        );
        r
    }

    #[test]
    fn verify_accepts_registered_tokens() {
        let r = registry();
        assert_eq!(
            r.verify("token-claude-abcdef").unwrap().client_type,
            ClientType::Claude
        );
        assert_eq!(
            r.verify("token-codex-123456").unwrap().client_type,
            ClientType::Codex
        );
    }

    #[test]
    fn invalid_token_reason_flags_placeholder_literals() {
        // Claude Code / Gemini CLI 的 ${VAR}、编辑器系的 ${env:VAR}、裸 $VAR 手改形态
        assert!(invalid_token_reason("${MCPANE_CLAUDE_TOKEN}").contains("unexpanded"));
        assert!(invalid_token_reason("${env:MCPANE_CURSOR_TOKEN}").contains("unexpanded"));
        assert!(invalid_token_reason("$MCPANE_GEMINI_TOKEN").contains("unexpanded"));
        // Cline 等客户端的配置模板占位符
        assert!(invalid_token_reason("<在卡片「凭证」里发放的 Token>").contains("placeholder"));
        // 合法 Token 是纯 hex，不可能以 $ 或 < 开头，误报面为零。
        assert_eq!(
            invalid_token_reason("e01163b75ee40849"),
            "invalid bearer token"
        );
        assert_eq!(invalid_token_reason("deadbeef01"), "invalid bearer token");
    }

    #[test]
    fn verify_rejects_unknown_and_near_miss_tokens() {
        let r = registry();
        assert!(r.verify("").is_none());
        assert!(r.verify("token-claude-abcdeF").is_none());
        assert!(r.verify("token-claude-abcde").is_none());
        assert!(r.verify("totally-unknown").is_none());
    }

    #[test]
    fn revoke_disables_only_target_connection() {
        let r = registry();
        assert!(r.set_enabled("client_claude", false));
        assert!(r.verify("token-claude-abcdef").is_none(), "撤销后必须拒绝");
        assert!(
            r.verify("token-codex-123456").is_some(),
            "撤销 Claude 不得影响 Codex"
        );
        assert!(r.set_enabled("client_claude", true));
        assert!(r.verify("token-claude-abcdef").is_some(), "恢复后放行");
        assert!(
            !r.set_enabled("client_unknown", false),
            "未知连接返回 false"
        );
    }

    #[test]
    fn registry_never_stores_plaintext() {
        let r = registry();
        for (_, _, prefix, _) in r.connection_summaries() {
            assert_eq!(prefix.len(), 8);
        }
        // TokenEntry 无明文字段（编译期保证），这里断言摘要不含完整 Token。
        let summaries = format!("{:?}", r.connection_summaries());
        assert!(!summaries.contains("token-claude-abcdef"));
        assert!(!summaries.contains("token-codex-123456"));
    }

    #[test]
    fn hash_hydration_matches_plaintext_verification() {
        let plaintext = TokenRegistry::new();
        plaintext.register(
            "hydrate-me-token",
            ClientIdentity::from_raw("c1".into(), "claude"),
        );
        // 模拟 SQLite 存的 hex 哈希。
        let hash_hex: String = sha2::Sha256::digest("hydrate-me-token".as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();

        let hydrated = TokenRegistry::new();
        assert!(hydrated.register_hash(
            &hash_hex,
            "hydrate-",
            ClientIdentity::from_raw("c1".into(), "claude"),
            true,
        ));
        assert!(hydrated.verify("hydrate-me-token").is_some());
        assert!(hydrated.verify("wrong").is_none());
        assert!(!hydrated.register_hash(
            "not-hex",
            "x",
            ClientIdentity::from_raw("c2".into(), "codex"),
            true
        ));
    }

    #[test]
    fn rate_limiter_locks_after_threshold_and_recovers() {
        let limiter = AuthRateLimiter::new(3, std::time::Duration::from_millis(80));
        assert!(limiter.check());
        limiter.record_failure();
        limiter.record_failure();
        assert!(limiter.check(), "未达阈值不锁");
        limiter.record_failure();
        assert!(!limiter.check(), "达到阈值必须锁定");
        std::thread::sleep(std::time::Duration::from_millis(120));
        assert!(limiter.check(), "锁定期过后恢复");
        // 成功重置计数。
        limiter.record_failure();
        limiter.record_success();
        limiter.record_failure();
        limiter.record_failure();
        assert!(limiter.check(), "成功后计数应清零");
    }

    #[test]
    fn generated_tokens_are_256_bit_hex_and_unique() {
        let a = generate_token();
        let b = generate_token();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
