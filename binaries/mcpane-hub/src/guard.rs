//! Host / Origin 校验与请求日志（脱敏）中间件。
//!
//! - `Host` 必须命中 loopback 允许清单（防 DNS rebinding，见 SECURITY.md）；
//! - `Origin` 存在时必须同样命中允许清单；
//! - 请求日志只记录方法、路径、状态、耗时、request_id 与客户端连接 ID，
//!   永不记录任何 Header（Authorization 因此天然不落日志）。

use std::time::Instant;

use axum::{
    extract::Request,
    http::{StatusCode, header},
    middleware::Next,
    response::Response,
};

use crate::auth::ClientIdentity;

/// 每个 HTTP 请求分配的 ID，注入请求扩展供 `mcpane__connection_info` 读取。
#[derive(Debug, Clone)]
pub struct RequestId(pub String);

/// Host 允许的主机名（数据面仅监听 loopback，见 ADR-002）。
/// IPv6 只收带括号形态：HTTP 规范要求 Host 里的 IPv6 必须括号包裹，且下方
/// 端口剥离逻辑会把裸 `::1` 切成 `:`——不带括号的条目永远匹配不到，是死数据。
const ALLOWED_HOSTS: &[&str] = &["127.0.0.1", "localhost", "[::1]"];

fn host_allowed(value: &str) -> bool {
    // 去掉端口：`127.0.0.1:24860` → `127.0.0.1`；`[::1]:24860` → `[::1]`
    let host = if let Some(rest) = value.strip_prefix('[') {
        match rest.split_once(']') {
            Some((v6, _)) => format!("[{v6}]"),
            None => return false,
        }
    } else {
        value.rsplit_once(':').map_or(value, |(h, _)| h).to_string()
    };
    ALLOWED_HOSTS.contains(&host.as_str())
}

fn origin_allowed(value: &str) -> bool {
    let rest = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"));
    match rest {
        Some(host_port) => host_allowed(host_port.trim_end_matches('/')),
        None => false,
    }
}

/// Host / Origin 校验中间件。
pub async fn host_origin_guard(request: Request, next: Next) -> Result<Response, Response> {
    let host_ok = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .is_some_and(host_allowed);
    if !host_ok {
        return Err(forbidden("invalid Host header"));
    }

    if let Some(origin) = request.headers().get(header::ORIGIN) {
        let origin_ok = origin.to_str().ok().is_some_and(origin_allowed);
        if !origin_ok {
            return Err(forbidden("invalid Origin header"));
        }
    }

    Ok(next.run(request).await)
}

fn forbidden(reason: &str) -> Response {
    let mut response = Response::new(
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": { "code": -32002, "message": format!("forbidden: {reason}") }
        })
        .to_string()
        .into(),
    );
    *response.status_mut() = StatusCode::FORBIDDEN;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    response
}

/// 脱敏请求日志中间件：分配 request_id 并记录请求概要。
pub async fn request_log_middleware(mut request: Request, next: Next) -> Response {
    let request_id = format!("req_{}", uuid::Uuid::new_v4().simple());
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));

    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let client_connection_id = request
        .extensions()
        .get::<ClientIdentity>()
        .map(|i| i.client_connection_id.clone());

    let started = Instant::now();
    let response = next.run(request).await;

    tracing::info!(
        request_id = %request_id,
        method = %method,
        path = %path,
        status = response.status().as_u16(),
        duration_ms = started.elapsed().as_millis() as u64,
        client_connection_id = client_connection_id.as_deref().unwrap_or("-"),
        "mcp request"
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_allowlist_covers_loopback_variants() {
        assert!(host_allowed("127.0.0.1:24860"));
        assert!(host_allowed("127.0.0.1"));
        assert!(host_allowed("localhost:24860"));
        assert!(host_allowed("[::1]:24860"));
        assert!(host_allowed("[::1]"));
    }

    #[test]
    fn host_allowlist_rejects_external_hosts() {
        assert!(!host_allowed("evil.com"));
        assert!(!host_allowed("evil.com:24860"));
        assert!(!host_allowed("127.0.0.1.evil.com"));
        assert!(!host_allowed("192.168.1.10:24860"));
        assert!(!host_allowed(""));
    }

    #[test]
    fn origin_allowlist_behaviour() {
        assert!(origin_allowed("http://127.0.0.1:24860"));
        assert!(origin_allowed("http://localhost:1420"));
        assert!(!origin_allowed("http://evil.com"));
        assert!(!origin_allowed("https://127.0.0.1.evil.com"));
        assert!(!origin_allowed("file://etc/passwd"));
        assert!(!origin_allowed("null"));
    }
}
