//! 例外表的端到端实证：本机目标必须**真的**不经过代理。
//!
//! 单测里的路由断言（`ptr::eq` 比对返回了哪个客户端）只证明我们挑对了客户端，
//! 不证明那个客户端的行为符合预期。这里立一个只数连接的假代理，从 reqwest
//! 的实际行为反推——这才是「本地 MCP server / 本机模型端点不被劫走」的证据。
//!
//! 全程不出网：假代理与目标服务都在 127.0.0.1 上。

#![allow(clippy::expect_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use mcpane_net::{OutboundClients, ProxyConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// 假代理探到的东西：被连过几次，以及对方开口说的头几个字节。
#[derive(Default)]
struct Spy {
    hits: AtomicUsize,
    /// 首次连接收到的前若干字节。协议握手的第一句话足以认出对方在说什么。
    first_bytes: Mutex<Vec<u8>>,
}

/// 假代理：统计连接并录下对方的开场白，不做真转发。
/// 「代理被联系过」这一个事实就足以判定流量的去向。
async fn spy_proxy() -> (u16, Arc<Spy>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定假代理");
    let port = listener.local_addr().expect("取端口").port();
    let spy = Arc::new(Spy::default());
    let recorder = spy.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            recorder.hits.fetch_add(1, Ordering::SeqCst);
            let mut head = [0_u8; 8];
            if let Ok(n) = stream.read(&mut head).await
                && n > 0
                && let Ok(mut slot) = recorder.first_bytes.lock()
                && slot.is_empty()
            {
                slot.extend_from_slice(&head[..n]);
            }
            drop(stream);
        }
    });
    (port, spy)
}

/// 本机目标服务：像个本地 MCP server，收到请求就回 204。
async fn local_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定本地服务");
    let port = listener.local_addr().expect("取端口").port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            // 必须先把请求读掉、回应后再优雅关闭。Windows 上若接收缓冲区里还
            // 压着没读的请求字节就关闭 socket，TCP 走的是强制关闭（RST），
            // 客户端拿到 ConnectionAborted(10053) 而不是那条 204；Unix 上同样
            // 的写法通常侥幸能过，所以这个坑只在 Windows 现形。
            let mut request = Vec::new();
            let mut buf = [0_u8; 1024];
            while let Ok(n) = stream.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                // GET 无体，读到头部终止符就算收完
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let _ = stream
                .write_all(b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\n\r\n")
                .await;
            let _ = stream.flush().await;
            // 先发 FIN，让客户端有机会把响应读完再见到连接关闭
            let _ = stream.shutdown().await;
        }
    });
    port
}

/// 本功能最要紧的一条：配了代理之后，本机目标依然直连。
///
/// 反例的代价很具体——`hyper-util` 不读系统例外表，不自建这层的话，
/// 用户一开代理，本地 MCP server 与本机模型端点（Ollama 等）就全被塞进代理。
#[tokio::test]
async fn local_target_never_touches_the_proxy() {
    let (proxy_port, spy) = spy_proxy().await;
    let target_port = local_server().await;

    let clients = OutboundClients::build(&ProxyConfig::Manual {
        url: format!("http://127.0.0.1:{proxy_port}"),
    })
    .expect("构造客户端");

    let url = format!("http://127.0.0.1:{target_port}/");
    let response = clients
        .for_target(&url)
        .get(&url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await;

    assert!(response.is_ok(), "本地目标应直连可达：{response:?}");
    assert_eq!(
        spy.hits.load(Ordering::SeqCst),
        0,
        "本地目标的流量被送进了代理——例外表失效了"
    );
}

/// 反向对照：公网目标必须**真的**经过代理，否则上面那条测试只要
/// 「谁都不走代理」就能作弊通过。
#[tokio::test]
async fn public_target_does_go_through_the_proxy() {
    let (proxy_port, spy) = spy_proxy().await;

    let clients = OutboundClients::build(&ProxyConfig::Manual {
        url: format!("http://127.0.0.1:{proxy_port}"),
    })
    .expect("构造客户端");

    // 假代理不会真转发，请求注定失败——我们要的证据只是「它被连过」。
    // `.invalid` 是保留 TLD，永远解析不到真实主机，不会误触外网。
    let url = "http://nowhere.invalid/";
    let _ = clients
        .for_target(url)
        .get(url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await;

    assert!(
        spy.hits.load(Ordering::SeqCst) > 0,
        "公网目标没走代理——代理配置根本没生效"
    );
}

/// 强制直连模式下，连公网目标也不碰代理。
#[tokio::test]
async fn direct_mode_ignores_everything() {
    let (proxy_port, spy) = spy_proxy().await;
    let _ = proxy_port; // Direct 模式不该用到它

    let clients = OutboundClients::build(&ProxyConfig::Direct).expect("构造客户端");
    let url = "http://nowhere.invalid/";
    let _ = clients
        .for_target(url)
        .get(url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await;

    assert_eq!(
        spy.hits.load(Ordering::SeqCst),
        0,
        "Direct 模式不该碰任何代理"
    );
}

/// reqwest 的 `socks` feature 哨兵——**必须验行为，不能验构造**。
///
/// 曾经写成「`OutboundClients::build(socks5 地址).is_ok()`」，那是假测试：
/// reqwest 0.13.4 里带 `#[cfg(feature = "socks")]` 门控、会报 unknown scheme 的
/// `ProxyScheme::parse` **整段被注释掉了**（src/proxy.rs 97–757 行是一个注释块）；
/// 实际生效的 `IntoProxy::into_proxy` 对 socks 既不做 feature 门控也不做 scheme
/// 校验，只解析 URL 并补默认端口 1080。于是无论 feature 开没开，构造恒 Ok，
/// 那条断言实际只测了「URL 能解析」。
///
/// 唯一靠得住的判据是握手本身：SOCKS5 的第一个字节是版本号 `0x05`。
/// feature 被裁掉时 reqwest 根本不会讲 socks，这里立刻红。
///
/// **踩过第二次坑：scheme 必须是 `socks5h`，不能是 `socks5`。**
/// 换成握手判据后这条测试曾经又恒红——不是 feature 裁了，是 scheme 选错了。
/// reqwest 对目标域名的 DNS 解析方式由 scheme 决定（`reqwest-0.13.4/src/
/// connect.rs:540-541`）：`socks4`/`socks5` 走 `DnsResolve::Local`（本机先解析
/// 目标域名，再把解析出的 IP 交给代理），只有 `socks4a`/`socks5h` 才走
/// `DnsResolve::Proxy`（域名原样交给代理去解析）。这里的目标域名
/// `nowhere.invalid` 用的是 RFC 2606 保留 TLD，本机解析必然 NXDOMAIN——配成
/// `socks5` 的话，请求在本机 DNS 阶段就失败，连接根本到不了下面的假代理，
/// 永远等不到握手，于是这条哨兵恒红，且红的原因跟它想防的 feature 裁剪
/// 毫无关系。必须用 `socks5h` 让 reqwest 把域名原样转交代理，握手才会发生。
#[tokio::test]
async fn socks5_feature_is_actually_compiled_in() {
    let (proxy_port, spy) = spy_proxy().await;

    let clients = OutboundClients::build(&ProxyConfig::Manual {
        url: format!("socks5h://127.0.0.1:{proxy_port}"),
    })
    .expect("构造客户端");

    let url = "http://nowhere.invalid/";
    let _ = clients
        .for_target(url)
        .get(url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await;

    let opening = spy.first_bytes.lock().expect("读开场白").clone();
    assert_eq!(
        opening.first(),
        Some(&0x05),
        "没听到 SOCKS5 握手（收到 {opening:02x?}）——reqwest 的 socks feature 是不是被裁了？\
         裁掉后代理会静默失效，用户只看到「保存成功但连不上」"
    );
}

/// 进程级入口（`set_proxy` + `client_for`）的接线检查：换了配置之后，
/// 后续请求要用上新代理。
///
/// 本文件只有这一条动全局槽——它是进程单例，多条并发改它会互相顶掉。
#[tokio::test]
async fn global_slot_applies_new_proxy_to_later_requests() {
    let (proxy_port, spy) = spy_proxy().await;

    mcpane_net::set_proxy(&ProxyConfig::Manual {
        url: format!("http://127.0.0.1:{proxy_port}"),
    })
    .expect("换上代理");

    let url = "http://nowhere.invalid/";
    let _ = mcpane_net::client_for(url)
        .get(url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await;

    assert!(
        spy.hits.load(Ordering::SeqCst) > 0,
        "set_proxy 换的代理没被 client_for 用上"
    );
}
