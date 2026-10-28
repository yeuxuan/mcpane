//! Codex Adapter（TASK-202）。
//!
//! - `toml_edit` 语法树级最小修改：注释、未知表、格式全部保留；
//! - 禁止整体重建 `config.toml`；
//! - 凭据策略（ADR-015）：默认 EnvVar——`bearer_token_env_var` 环境变量引用，
//!   Token 不落盘；Inline（`http_headers.Authorization` 明文直写，零手动步骤）为可选。

pub mod auth;
pub mod detect;
pub mod discovery;
pub mod patch;

pub use auth::{AuthKind, classify_auth, relay_auth_content};
pub use detect::{CodexInstallation, detect_codex, locate_codex};
pub use discovery::{CodexConfigPaths, discover_config_paths};
pub use patch::{hub_entry_patch, read_mcp_servers, remove_hub_entry_patch, validate_hub_entry};

/// Codex 的 MCP Entry 名称（冻结标识）。
pub const MCP_ENTRY_NAME: &str = "mcpane";

/// 默认上游地址（冻结标识）：取自 hub-protocol 唯一事实源，避免与 Hub 实际
/// 监听地址漂移。
pub const DEFAULT_HUB_URL: &str = mcpane_hub_protocol::HUB_MCP_URL;

/// 凭据环境变量（ADR-015 EnvVar 默认模式用）。
pub const TOKEN_ENV_VAR: &str = "MCPANE_CODEX_TOKEN";
