//! MCP 服务器发现（ADR-025）：用户粘贴 GitHub 仓库 / npm 包 / PyPI 包 / 远程 URL，
//! 查官方 MCP Registry，产出可预填进「添加服务器」表单的候选。
//!
//! 全程**只读且不执行任何东西**：发现阶段不调 npx / uvx，候选交给界面预填表单，
//! 由用户核对完整命令后自己保存。

pub mod address;
pub mod candidate;
pub mod client;
pub mod model;
pub mod resolve;

pub use address::{Address, parse_address};
pub use candidate::{Candidate, Field, Install, RemoteTransport, Runtime, Source};
pub use client::RegistryError;
pub use resolve::{Notice, Resolution, resolve, search_terms};

/// 生产路径：解析地址并查询官方 Registry。输入为空返回 `None`。
pub async fn discover(input: &str) -> Option<Result<Resolution, RegistryError>> {
    let address = parse_address(input)?;
    Some(resolve(address, |term| async move { client::search(&term).await }).await)
}
