//! Claude Code Adapter（TASK-201）。
//!
//! 职责：检测安装、发现配置、生成最小 Patch（Hub MCP Entry / Provider env）。
//! 所有写入统一交给 `mcpane-config` 引擎执行（Diff/Snapshot/原子写/回滚）。
//!
//! 凭据策略（ADR-015）：默认 EnvVar——`Bearer ${MCPANE_CLAUDE_TOKEN}` 环境变量
//! 展开，Token 不落盘；Inline（Authorization 明文直写，零手动步骤）为可选。

pub mod detect;
pub mod discovery;
pub mod patch;
pub mod takeover;

pub use detect::{ClaudeInstallation, detect_claude, locate_claude};
pub use discovery::{
    ClaudeConfigPaths, McpScope, claude_config_dir, config_dir_override, discover_config_paths,
    discover_config_paths_with,
};
pub use patch::{
    HUB_ENTRY_KEY, LEGACY_ENV_KEYS, PROVIDER_ENV_KEYS, hub_entry_patch, provider_env_patch,
    read_mcp_servers, remove_hub_entry_patch, remove_provider_env_patch, validate_hub_entry,
};

/// Claude Code 的 MCP Entry 名称（冻结标识）。
pub const MCP_ENTRY_NAME: &str = "mcpane";

/// 默认上游地址（冻结标识）：直接取自 hub-protocol 唯一事实源，避免与 Hub
/// 实际监听地址漂移（端口若变，客户端配置写的 URL 必须同步变）。
pub const DEFAULT_HUB_URL: &str = mcpane_hub_protocol::HUB_MCP_URL;

/// 凭据环境变量（ADR-015 EnvVar 默认模式用）。
pub const TOKEN_ENV_VAR: &str = "MCPANE_CLAUDE_TOKEN";
