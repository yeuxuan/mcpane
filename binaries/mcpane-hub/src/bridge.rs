//! `mcpane-hub bridge`：stdio ↔ Streamable HTTP 透明桥。
//!
//! 只支持 stdio 的 MCP 客户端（Claude Desktop 等）把本进程当作一个普通
//! stdio MCP Server 拉起；本进程把 stdio 上的每条 JSON-RPC 消息原样转发到
//! Hub 数据面（默认 `http://127.0.0.1:24860/mcp`），反向亦然。initialize
//! 握手、会话头（Mcp-Session-Id）全部由 rmcp 的 Streamable HTTP 客户端
//! 传输在内部处理——桥本身不理解也不改写任何消息，协议演进零维护。
//!
//! 对齐 mcp-router 的 `@mcp_router/cli connect`，但不发 npm 包：桥就是
//! 随应用分发的 hub 二进制本体，客户端配置写它的绝对路径即可，用户机器
//! 上无需 node/npx。
//!
//! 约束：
//! - stdout 是协议通道，日志一律走 stderr；
//! - 零数据目录副作用（不装 crash.log panic hook、不碰 MCPANE_HOME）；
//! - 目标恒为本机 loopback，reqwest 显式 `no_proxy()`——系统代理
//!   （Clash 等）截走 localhost 流量会让桥莫名其妙连不上。

use anyhow::Context;
use rmcp::service::{RoleClient, RoleServer};
use rmcp::transport::Transport;
use rmcp::transport::async_rw::AsyncRwTransport;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};

/// Token 环境变量名（唯一事实源在 protocol crate，桌面端写配置时共用）。
pub const TOKEN_ENV: &str = mcpane_hub_protocol::bridge_env::TOKEN;
/// Hub 数据面端点环境变量名；缺省用 [`mcpane_hub_protocol::HUB_MCP_URL`]。
pub const URL_ENV: &str = mcpane_hub_protocol::bridge_env::URL;

/// 桥主循环：双向泵到任一侧断开为止。
///
/// - stdin EOF（客户端退出/关停）→ 正常结束；
/// - Hub 侧流断（hub 重启/不可达）→ 报错退出，宿主客户端会按
///   「server 崩溃」呈现并允许用户重连——比静默挂着装活着诚实。
pub async fn run(url: String, token: String) -> anyhow::Result<()> {
    let mut headers = reqwest::header::HeaderMap::new();
    let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
        .context("MCPANE_TOKEN 含非法字符，无法构造 Authorization 头")?;
    value.set_sensitive(true);
    headers.insert(reqwest::header::AUTHORIZATION, value);
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .no_proxy()
        .build()
        .context("构造 HTTP 客户端失败")?;
    let mut upstream = StreamableHttpClientTransport::with_client(
        client,
        StreamableHttpClientTransportConfig::with_uri(url.clone()),
    );

    let (stdin, stdout) = rmcp::transport::io::stdio();
    let mut downstream = AsyncRwTransport::<RoleServer, _, _>::new_server(stdin, stdout);

    tracing::info!(%url, "bridge 已启动（stdio ↔ streamable http）");
    loop {
        tokio::select! {
            incoming = downstream.receive() => {
                let Some(message) = incoming else {
                    tracing::info!("stdin 关闭，bridge 退出");
                    break;
                };
                Transport::<RoleClient>::send(&mut upstream, message)
                    .await
                    .map_err(|e| anyhow::anyhow!("转发到 Hub 失败（{url}）：{e}"))?;
            }
            outgoing = upstream.receive() => {
                let Some(message) = outgoing else {
                    anyhow::bail!("Hub 侧连接中断（{url}）");
                };
                Transport::<RoleServer>::send(&mut downstream, message)
                    .await
                    .context("写回 stdout 失败")?;
            }
        }
    }
    let _ = Transport::<RoleClient>::close(&mut upstream).await;
    Ok(())
}
