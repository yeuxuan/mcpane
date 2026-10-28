//! 官方 MCP Registry 的只读客户端。
//!
//! 出网走 `mcpane_net::client_for`（代理 + 内网豁免，ADR-017）；桌面端前端的 CSP 是
//! `default-src 'self'`，本来也不能直接请求外部地址。
//!
//! 实测官方 Registry 的响应时间在 5–27 秒之间波动（即使 `limit=5`），所以超时放宽到 30 秒，
//! 并带一个短时内存缓存：用户微调地址再查一次不必重新等。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::model::{Entry, SearchResponse};

const REGISTRY_URL: &str = "https://registry.modelcontextprotocol.io/v0/servers";
const TIMEOUT: Duration = Duration::from_secs(30);
const PAGE_SIZE: usize = 20;
const CACHE_TTL: Duration = Duration::from_secs(10 * 60);
const CACHE_MAX_ENTRIES: usize = 64;
/// 响应体上限：20 条条目远小于 1 MiB，4 MiB 挡得住异常膨胀的第三方响应。
const MAX_BODY_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, thiserror::Error)]
pub enum RegistryError {
    #[error("无法访问官方 MCP Registry（{0}），请检查网络或代理设置")]
    Network(String),
    #[error("官方 MCP Registry 返回了错误（HTTP {0}）")]
    Status(u16),
    #[error("无法解析官方 MCP Registry 的响应：{0}")]
    Parse(String),
    #[error("官方 MCP Registry 的响应过大，已放弃")]
    TooLarge,
}

type Cache = Mutex<HashMap<String, (Instant, Vec<Entry>)>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_get(term: &str) -> Option<Vec<Entry>> {
    let map = cache().lock().ok()?;
    map.get(term)
        .filter(|(at, _)| at.elapsed() < CACHE_TTL)
        .map(|(_, entries)| entries.clone())
}

fn cache_put(term: &str, entries: &[Entry]) {
    let Ok(mut map) = cache().lock() else { return };
    map.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
    if map.len() >= CACHE_MAX_ENTRIES
        && let Some(oldest) = map
            .iter()
            .min_by_key(|(_, (at, _))| *at)
            .map(|(k, _)| k.clone())
    {
        map.remove(&oldest);
    }
    map.insert(term.to_owned(), (Instant::now(), entries.to_vec()));
}

/// 按名称检索（只取每个服务器的最新版本）。
pub async fn search(term: &str) -> Result<Vec<Entry>, RegistryError> {
    let key = term.to_lowercase();
    if let Some(hit) = cache_get(&key) {
        return Ok(hit);
    }
    let url = url::Url::parse_with_params(
        REGISTRY_URL,
        [
            ("search", term),
            ("limit", &PAGE_SIZE.to_string()),
            ("version", "latest"),
        ],
    )
    .map_err(|e| RegistryError::Network(e.to_string()))?;
    let response = mcpane_net::client_for(REGISTRY_URL)
        .get(url)
        .header(
            reqwest::header::USER_AGENT,
            concat!("MCPane/", env!("CARGO_PKG_VERSION")),
        )
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| RegistryError::Network(e.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(RegistryError::Status(status.as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_BODY_BYTES)
    {
        return Err(RegistryError::TooLarge);
    }
    let body = response
        .bytes()
        .await
        .map_err(|e| RegistryError::Network(e.to_string()))?;
    if body.len() as u64 > MAX_BODY_BYTES {
        return Err(RegistryError::TooLarge);
    }
    let parsed: SearchResponse =
        serde_json::from_slice(&body).map_err(|e| RegistryError::Parse(e.to_string()))?;
    cache_put(&key, &parsed.servers);
    Ok(parsed.servers)
}
