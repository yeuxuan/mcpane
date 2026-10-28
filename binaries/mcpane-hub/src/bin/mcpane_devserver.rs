//! mcpane-devserver：GATE-3 验证与本地开发用的下游 MCP Server。
//!
//! - 默认：stdio transport；
//! - 工具：`search`（返回含 server 标识的结果）、`crash`（进程立即退出，
//!   用于崩溃隔离测试）；
//! - `MCPANE_DEVSERVER_NAME` 控制 server 标识（默认 `devserver`）。

use rmcp::{
    ErrorData, ServerHandler, ServiceExt,
    handler::server::wrapper::{Json, Parameters},
    model::{Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};

fn server_name() -> String {
    std::env::var("MCPANE_DEVSERVER_NAME").unwrap_or_else(|_| "devserver".to_owned())
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct SearchParams {
    pub query: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct SleepParams {
    pub ms: u64,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SleepOutput {
    pub slept_ms: u64,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SearchOutput {
    pub server: String,
    pub query: String,
    pub results: Vec<String>,
}

#[derive(Clone, Default)]
struct DevServer;

#[tool_router]
impl DevServer {
    #[tool(
        name = "search",
        description = "开发用检索工具（返回带 server 标识的固定结果）",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn search(
        &self,
        Parameters(SearchParams { query }): Parameters<SearchParams>,
    ) -> Json<SearchOutput> {
        let server = server_name();
        Json(SearchOutput {
            results: vec![format!("{server}:result-for:{query}")],
            server,
            query,
        })
    }

    #[tool(
        name = "sleep",
        description = "等待指定毫秒后返回（超时与并发测试专用）",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn sleep(
        &self,
        Parameters(SleepParams { ms }): Parameters<SleepParams>,
    ) -> Json<SleepOutput> {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        Json(SleepOutput { slept_ms: ms })
    }

    #[tool(
        name = "crash",
        description = "使当前下游进程立即退出（崩溃隔离测试专用）",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    fn crash(&self) -> Result<rmcp::model::CallToolResult, ErrorData> {
        eprintln!("[mcpane-devserver] crash tool invoked, exiting");
        std::process::exit(42);
    }

    /// MRTR 两轮工具（TASK-805）：首轮返回 `input_required` 要求确认，
    /// 客户端带着 `requestState` 重试才真正执行。
    #[tool(
        name = "confirm",
        description = "需要一次确认才执行的工具（MRTR 往返测试专用）",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    fn confirm(
        &self,
        Parameters(ConfirmParams { action }): Parameters<ConfirmParams>,
        rmcp::handler::server::tool::RequestState(state): rmcp::handler::server::tool::RequestState,
    ) -> Result<rmcp::model::CallToolResponse, ErrorData> {
        // 首轮：还没拿到确认，要求客户端补充输入。
        let Some(state) = state else {
            return Ok(
                rmcp::model::InputRequiredResult::from_request_state(format!("pending:{action}"))
                    .into(),
            );
        };
        // 次轮：state 是本 server 自己发的那份，原样回来了才算数。
        Ok(
            rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text(format!(
                "confirmed via {state}"
            ))])
            .into(),
        )
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct ConfirmParams {
    pub action: String,
}

/// `MCPANE_DEVSERVER_MODERN_ONLY=1`：只讲 2026-07-28，legacy initialize 一律
/// 拒绝。用来验证 Hub 能聚合新协议独占的上游（TASK-804）——市面上已有这类
/// 服务端（如 googleapis/mcp-toolbox 的 Go SDK 默认即新版）。
fn modern_only() -> bool {
    std::env::var("MCPANE_DEVSERVER_MODERN_ONLY").is_ok_and(|v| v == "1")
}

#[tool_handler]
impl ServerHandler for DevServer {
    fn get_info(&self) -> ServerInfo {
        let mut implementation = Implementation::from_build_env();
        implementation.name = format!("mcpane-devserver/{}", server_name());
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());
        info.server_info = implementation;
        if modern_only() {
            info.protocol_version = rmcp::model::ProtocolVersion::V_2026_07_28;
        }
        info
    }

    fn supported_protocol_versions(
        &self,
    ) -> std::borrow::Cow<'static, [rmcp::model::ProtocolVersion]> {
        if modern_only() {
            std::borrow::Cow::Owned(vec![rmcp::model::ProtocolVersion::V_2026_07_28])
        } else {
            std::borrow::Cow::Borrowed(rmcp::model::ProtocolVersion::KNOWN_VERSIONS)
        }
    }

    async fn initialize(
        &self,
        request: rmcp::model::InitializeRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::InitializeResult, ErrorData> {
        if modern_only() {
            return Err(ErrorData::invalid_request(
                "this server only speaks 2026-07-28; use server/discover",
                None,
            ));
        }
        context.peer.set_peer_info(request.clone());
        let mut info = self.get_info();
        if self
            .supported_protocol_versions()
            .contains(&request.protocol_version)
        {
            info.protocol_version = request.protocol_version;
        }
        Ok(info)
    }
}

/// `MCPANE_DEVSERVER_LEGACY_RUDE=1`：手写 stdio 循环模拟「粗鲁的存量服务器」——
/// 对 server/discover 等未知方法回 -32600（不是 rmcp Auto 认得的 -32601），
/// legacy initialize 正常。复现真实世界里探测无法自动回退的形态
/// （官方 TS SDK、learn.microsoft.com 均属此类），供 connect 层回退的
/// stdio 分支做回归。走 rmcp 实现不了：它对未知方法总是标准 -32601。
async fn run_legacy_rude() -> Result<(), Box<dyn std::error::Error>> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut lines = stdin.lines();
    while let Some(line) = lines.next_line().await? {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let id = message["id"].clone();
        if id.is_null() {
            continue; // 通知不回。
        }
        let response = match message["method"].as_str().unwrap_or_default() {
            "initialize" => serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": {
                "protocolVersion": "2025-11-25",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "legacy-rude", "version": "0" } } }),
            "tools/list" => serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": {
                "tools": [{ "name": "search", "description": "rude legacy search",
                    "inputSchema": { "type": "object" } }] } }),
            _ => serde_json::json!({ "jsonrpc": "2.0", "id": id,
                "error": { "code": -32600, "message": "Bad Request" } }),
        };
        stdout.write_all(format!("{response}\n").as_bytes()).await?;
        stdout.flush().await?;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("MCPANE_DEVSERVER_LEGACY_RUDE").is_ok_and(|v| v == "1") {
        return run_legacy_rude().await;
    }
    let service = DevServer.serve(rmcp::transport::io::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
