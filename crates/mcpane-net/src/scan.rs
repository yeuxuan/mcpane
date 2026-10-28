//! 本地代理探测：找出本机正在跑、且**实测可用**的代理。
//!
//! ## 与「只探端口通不通」的差别
//!
//! TCP 连得上只能说明有人在监听，不能说明它讲代理协议。开发机上 8080 十有八九
//! 是本地 dev server，3128 / 8888 同理——把它们列成候选，用户点了连不上，白白
//! 浪费一轮排查。所以每个候选都要真的当代理用一次，通了才列。
//!
//! 验证目标用 HTTPS 是刻意的：reqwest 对 HTTPS 目标会向代理发 `CONNECT`，
//! 只有真代理答得上来；dev server 收到 `CONNECT` 只会 400/405，直接判死。
//! 换成 HTTP 目标就退化成「对面回了个 HTTP 响应」——dev server 也回，区分不开。
//!
//! ## 为什么不按端口猜协议
//!
//! cc-switch 维护了一张「端口 → 协议」硬编码表。这表两头漏：mihomo / Clash Verge
//! 的默认混合端口 7897 不在表里（这类用户扫不出自己的代理），而表里认定 8080 是
//! http 代理又常常是错的。既然每个候选都要实测，就不必猜——每个开着的端口
//! http 与 socks5 各试一次，谁通留谁。端口表退化成「值得一探的端口」，猜错无代价。

use std::time::{Duration, Instant};

use serde::Serialize;

/// 值得一探的本地端口。因为每个候选都会实测，收录得宽一些也不会产生误报。
const CANDIDATE_PORTS: &[(u16, &str)] = &[
    (7890, "Clash / ClashX / Clash for Windows"),
    (7891, "Clash SOCKS"),
    (7892, "Clash HTTP"),
    (7897, "Clash Verge / mihomo"),
    (1080, "通用 SOCKS5"),
    (1086, "ShadowsocksX-NG SOCKS"),
    (1087, "ShadowsocksX-NG HTTP"),
    (2080, "sing-box / NekoRay"),
    (3128, "Squid"),
    (8080, "通用 HTTP 代理"),
    (8888, "Charles / Fiddler"),
    (10808, "v2rayN SOCKS"),
    (10809, "v2rayN HTTP"),
];

/// 每个开着的端口都按这两种协议各试一次，不靠端口号猜。
const CANDIDATE_SCHEMES: &[&str] = &["http", "socks5"];

/// 探测目标：本功能存在的理由就是够到这个地址。用 HTTPS 以强制走 `CONNECT`。
const VERIFY_TARGET: &str = "https://api.anthropic.com";

/// TCP 探活超时。本机连接，通不通是微秒级的事，给足余量即可。
const PROBE_TIMEOUT: Duration = Duration::from_millis(200);

/// 实测超时：要真的经代理连出去，得容忍跨境往返。
const VERIFY_TIMEOUT: Duration = Duration::from_secs(6);

/// 一个实测可用的本地代理。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DetectedProxy {
    /// 可直接填进配置的完整地址。
    pub url: String,
    pub port: u16,
    /// 该端口常见的归属，仅供 UI 提示，不参与判定。
    pub hint: &'static str,
    /// 经该代理够到验证目标的耗时。
    pub latency_ms: u64,
}

/// 扫描本机可用代理，按实测延迟升序返回。
///
/// 只返回实测通过的条目：宁可空手而归，也不给用户一个「看着像、实际不通」的选项。
pub async fn scan_local_proxies() -> Vec<DetectedProxy> {
    let open_ports = probe_open_ports().await;
    if open_ports.is_empty() {
        return Vec::new();
    }
    tracing::debug!(?open_ports, "本地代理扫描：发现监听端口，开始实测");

    let mut checks = Vec::new();
    for (port, hint) in open_ports {
        for scheme in CANDIDATE_SCHEMES {
            checks.push(tokio::spawn(verify(port, hint, scheme)));
        }
    }

    let mut found = Vec::new();
    for check in checks {
        if let Ok(Some(detected)) = check.await {
            found.push(detected);
        }
    }
    found.sort_by_key(|d| (d.latency_ms, d.port));
    found
}

/// 先用 TCP 探活筛一轮：多数端口没人监听，直接省掉一次跨境实测的等待。
async fn probe_open_ports() -> Vec<(u16, &'static str)> {
    let probes: Vec<_> = CANDIDATE_PORTS
        .iter()
        .map(|&(port, hint)| {
            tokio::spawn(async move {
                let target = format!("127.0.0.1:{port}");
                match tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect(target))
                    .await
                {
                    Ok(Ok(_)) => Some((port, hint)),
                    _ => None,
                }
            })
        })
        .collect();

    let mut open = Vec::new();
    for probe in probes {
        if let Ok(Some(hit)) = probe.await {
            open.push(hit);
        }
    }
    open
}

/// 把候选当代理真用一次；通了才算数。
async fn verify(port: u16, hint: &'static str, scheme: &str) -> Option<DetectedProxy> {
    let url = format!("{scheme}://127.0.0.1:{port}");
    let latency_ms = measure_through_proxy(&url).await.ok()?;
    tracing::debug!(%url, latency_ms, "本地代理扫描：实测通过");
    Some(DetectedProxy {
        url,
        port,
        hint,
        latency_ms,
    })
}

/// 经指定代理够到验证目标，返回耗时。
///
/// 任何 HTTP 状态码都算通——我们要证的是隧道能建起来，不是目标返回 200。
pub async fn measure_through_proxy(proxy_url: &str) -> Result<u64, String> {
    let validated = crate::validate_proxy_url(proxy_url).map_err(|e| e.to_string())?;
    let proxy = reqwest::Proxy::all(validated).map_err(|e| e.to_string())?;
    let client = reqwest::Client::builder()
        .proxy(proxy)
        .timeout(VERIFY_TIMEOUT)
        .connect_timeout(VERIFY_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;

    let started = Instant::now();
    client
        .head(VERIFY_TARGET)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    Ok(started.elapsed().as_millis().try_into().unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    /// 端口表可以随手加，但不能重复——重复会让同一个代理在 UI 里列两遍。
    #[test]
    fn candidate_ports_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for &(port, _) in CANDIDATE_PORTS {
            assert!(seen.insert(port), "端口 {port} 在候选表里重复了");
        }
    }

    /// 7897 是 Clash Verge / mihomo 的默认混合端口，也是 cc-switch 那张表漏掉的那个。
    /// 掉了它，这类用户扫描一无所获，功能对他们等于不存在。
    #[test]
    fn covers_modern_clash_default_port() {
        assert!(CANDIDATE_PORTS.iter().any(|&(port, _)| port == 7897));
    }

    /// 扫描结果要能直接喂回配置层，两边的协议白名单不能各说各话。
    #[test]
    fn every_scanned_url_would_pass_validation() {
        for &(port, _) in CANDIDATE_PORTS {
            for scheme in CANDIDATE_SCHEMES {
                let url = format!("{scheme}://127.0.0.1:{port}");
                assert!(
                    crate::validate_proxy_url(&url).is_ok(),
                    "扫描会产出 {url}，但配置层不认"
                );
            }
        }
    }

    #[tokio::test]
    async fn rejects_invalid_proxy_url_without_touching_network() {
        assert!(measure_through_proxy("ftp://127.0.0.1:2121").await.is_err());
    }

    /// 没人监听的端口必须落空——这条守住「实测才算数」的底线。
    #[tokio::test]
    async fn dead_port_never_verifies() {
        // 绑定后立刻释放，拿到一个几乎可以确定没人在听的端口。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑定临时端口");
        let port = listener.local_addr().expect("取端口").port();
        drop(listener);

        assert!(
            verify(port, "测试", "http").await.is_none(),
            "空端口不该被判为可用代理"
        );
    }

    /// 监听着、但不讲代理协议的端口（例如本地 dev server）不能被列为候选——
    /// 这正是「只探 TCP 通不通」会踩的坑。
    #[tokio::test]
    async fn listening_but_not_a_proxy_is_rejected() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑定临时端口");
        let port = listener.local_addr().expect("取端口").port();
        // 接受连接后立刻断开：像个不懂代理协议的普通服务。
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                drop(stream);
            }
        });

        assert!(
            verify(port, "测试", "http").await.is_none(),
            "不讲代理协议的端口被误判为代理"
        );
        assert!(
            verify(port, "测试", "socks5").await.is_none(),
            "不讲 socks5 的端口被误判为代理"
        );
    }
}
