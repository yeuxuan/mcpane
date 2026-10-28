//! MCP Hub：统一 MCP 聚合入口（TASK-100 最小实现）。
//!
//! - 数据面：`http://127.0.0.1:24860/mcp`（POST/GET/DELETE，Bearer 认证）
//! - 控制面：仅本机用户可见的状态端点（macOS UDS；Windows 在 TASK-400 补齐）
//! - 协议：**双栈**（ADR-019）——主协议 `2026-07-28`（无状态、`server/discover`），
//!   `2025-11-25` 及更早经 `initialize` 握手长期并行（默认有状态 Session）

pub mod app;
pub mod auth;
pub mod bridge;
pub mod control;
pub mod guard;
pub mod hub;
pub mod state;

/// Hub 版本（与 crate 版本一致，写入 `connection_info` 与控制面状态）。
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Hub 的**主** MCP 协议版本（ADR-019）。仅用于对外展示（控制面状态 → 桌面端）
/// 与无法判定时的兜底；实际协商按请求逐个进行，历史版本同样受支持，
/// 见 [`MCP_LEGACY_PROTOCOL_VERSION`]。
pub const MCP_PROTOCOL_VERSION: &str = "2026-07-28";

/// 兼容层的协议版本：`initialize` 握手的协商回退值，也是当前两个锚定客户端
/// （Claude Code / Codex）实际走的那一档（ADR-019）。
pub const MCP_LEGACY_PROTOCOL_VERSION: &str = "2025-11-25";

/// 默认数据面监听地址（冻结标识，唯一事实源在 mcpane-hub-protocol）。
pub const DEFAULT_BIND_ADDR: &str = mcpane_hub_protocol::DEFAULT_BIND_ADDR;
