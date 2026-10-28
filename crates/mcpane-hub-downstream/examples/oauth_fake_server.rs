//! 独立运行的假 OAuth 授权服务器 + 受令牌保护的 MCP 服务器（ADR-024 的手动端到端用）。
//!
//! ```text
//! cargo run -p mcpane-hub-downstream --example oauth_fake_server
//! FAKE_EXPIRES_IN=5 cargo run -p mcpane-hub-downstream --example oauth_fake_server   # 令牌 5 秒过期
//! ```
//!
//! 启动后打印一行 `MCP_URL=http://127.0.0.1:<端口>/mcp`；`GET /__debug` 看授权服务器收到过什么。
#![allow(clippy::unwrap_used, clippy::print_stdout)]

#[path = "../tests/common/oauth_fake.rs"]
mod fake;

#[tokio::main]
async fn main() {
    let (state, url) = fake::spawn_fake().await;
    if let Some(secs) = std::env::var("FAKE_EXPIRES_IN")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        state
            .expires_in
            .store(secs, std::sync::atomic::Ordering::SeqCst);
    }
    println!("MCP_URL={url}");
    std::future::pending::<()>().await;
}
