//! 数据面 Router 组装：/mcp（POST/GET/DELETE）+ 中间件栈。

use std::sync::Arc;
use std::time::Duration;

use axum::{Router, middleware};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};

use crate::auth::{AuthRateLimiter, AuthState, TokenRegistry, bearer_auth_middleware};
use crate::guard::{host_origin_guard, request_log_middleware};
use crate::hub::HubService;
use crate::state::AggregationState;

/// Hub 数据面运行选项。
#[derive(Debug, Clone)]
pub struct HubOptions {
    /// 有状态模式：启用 `MCP-Session-Id`（TASK-104）。
    /// 无状态模式仅作为 TASK-100 基线保留。
    pub stateful: bool,
    /// Session 空闲超时（仅有状态模式生效；`None` 表示不超时，不推荐）。
    pub session_keep_alive: Option<Duration>,
}

impl Default for HubOptions {
    fn default() -> Self {
        Self {
            stateful: true,
            session_keep_alive: Some(Duration::from_secs(300)),
        }
    }
}

/// 组装数据面 Router。
///
/// 中间件从外到内：Host/Origin 校验 → Bearer 认证 → 脱敏请求日志 → MCP 服务。
pub fn build_router(
    registry: Arc<TokenRegistry>,
    state: Arc<AggregationState>,
    options: HubOptions,
) -> Router {
    let mut session_manager = LocalSessionManager::default();
    session_manager.session_config.keep_alive = options.session_keep_alive;

    // json_response 仅在无状态模式生效：短请求直接返回 application/json，
    // 免去 SSE 包装（TASK-105 的"普通请求优先 JSON"要求）。
    //
    // legacy_session_mode 只作用于 <2026-07-28 的请求：新协议本身无会话，
    // rmcp 一律按无状态服务，不受此开关影响（ADR-019）。
    let mcp_service = StreamableHttpService::new(
        move || Ok(HubService::new(state.clone())),
        session_manager.into(),
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(options.stateful)
            .with_json_response(true),
    );

    // 中间件挂在外层 Router：保留原始请求路径供日志使用
    // （nest_service 会向内层服务剥掉 /mcp 前缀）。
    Router::new()
        .nest_service("/mcp", axum::routing::any_service(mcp_service))
        .layer(middleware::from_fn(request_log_middleware))
        .layer(middleware::from_fn_with_state(
            AuthState {
                registry: registry.clone(),
                limiter: Arc::new(AuthRateLimiter::default()),
            },
            bearer_auth_middleware,
        ))
        .layer(middleware::from_fn(host_origin_guard))
}
