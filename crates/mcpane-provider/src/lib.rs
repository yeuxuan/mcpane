//! Provider 管理（Phase 3，v2 模型）。
//!
//! Provider 归属单一客户端 app（见 `ClientApp::ALL`），
//! 只有官方订阅（OAuth）与第三方中转站（base_url + API Key）两类。
//!
//! - `domain`：ProviderProfile / ProviderState（v2 schema）；
//! - `service`：中转站 CRUD、连通性测试、按 app 切换（官方 ↔ 中转站）；
//! - `store`：状态持久化（当前 JSON 文件，Phase 4 迁 SQLite 时接口不变）。
//!
//! 不做 API 协议转换。

pub mod adapter;
pub mod balance;
pub mod claude_desktop;
pub mod domain;
pub mod import_link;
pub mod install;
pub mod models_list;
pub mod quota;
pub mod reverse;
pub mod service;
pub mod store;

pub use adapter::{
    AuthHandling, ClaudeAdapter, ClaudeDesktopAdapter, ClientAdapter, ClientUserPaths,
    CodexAdapter, GeminiAdapter, adapter_for,
};
pub use balance::{BalanceProvider, detect_balance_provider};
pub use domain::{
    CODEX_AUTO_COMPACT_LIMIT_1M, CODEX_CONTEXT_WINDOW_1M, CODEX_SLUG_PREFIX, ClaudeApiFormat,
    ClaudeAuthField, ClientApp, CodexWireApi, DESKTOP_PROXY_DEFAULT_PORT, DesktopGateway,
    DesktopMode, DesktopRoute, OFFICIAL_CLAUDE_DESKTOP_ID, OFFICIAL_CLAUDE_ID, OFFICIAL_CODEX_ID,
    OFFICIAL_GEMINI_ID, PROFILE_SCHEMA_VERSION, ProviderCategory, ProviderProfile, ProviderState,
    RoutingConfig, RoutingStrategy, RoutingTuning, UNIFIED_ID_PREFIX, UnifiedApps, UnifiedProvider,
    UpstreamApi, codex_base_url_for_gateway, generate_codex_slug, is_router_url, router_base_url,
    router_namespace, unified_shadow_id,
};
pub use import_link::{ImportDraft, ImportLinkError, parse_import_link};
pub use install::{
    ClientEnv, client_env, client_env_with_dir, client_envs, client_envs_with_dirs, installed_apps,
    is_installed, is_installed_with_dir,
};
pub use models_list::fetch_models;
pub use quota::{
    Balance, CodingPlanProvider, ExhaustedUntil, ProviderQuota, QuotaTier, QuotaWindow,
    detect_coding_plan,
};
pub use reverse::{
    CLAUDE_MANAGED_ENV_KEYS, apply_codex_context_window, fill_from_claude, fill_from_codex_config,
    fill_from_gemini_env, upstream_api_from_str,
};
pub use service::{
    ClientPaths, EndpointLatency, ProviderError, ProviderService, RelayImportOutcome,
    SwitchOutcome, test_endpoints,
};
pub use store::{JsonProviderStore, ProviderStore};

/// 取出适用于该目标的共享 HTTP 客户端（连接池/TLS 复用）。超时按请求设置，
/// 不设全局默认。
///
/// 必须按目标取而不能全进程共用一个：全局代理配置下，本机与内网目标要强制
/// 直连，公网目标才走代理（ADR-017）。`reqwest::Client` 内部是 `Arc`，
/// 这里 clone 出来极廉价，且与代理配置变更天然解耦——在途请求握着旧 client
/// 用旧代理跑完，之后取到的就是新的。
pub(crate) fn http_client(target: &str) -> reqwest::Client {
    mcpane_net::client_for(target)
}
