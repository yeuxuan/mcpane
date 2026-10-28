//! Gemini CLI Adapter。
//!
//! 职责：检测安装、发现配置、生成最小 Patch（Provider env / auth 类型）。
//! 所有写入统一交给 `mcpane-config` 引擎执行（Diff/Snapshot/原子写/回滚）。
//!
//! Gemini 的供应商配置分两个文件（按官方文档与实测，核对日期 2026-07-20）：
//! - `~/.gemini/.env`：`GOOGLE_GEMINI_BASE_URL` / `GEMINI_API_KEY` /
//!   `GEMINI_MODEL`（dotenv 格式，经 mcpane-config 的 Env 文档模型写入）；
//! - `~/.gemini/settings.json`：`security.auth.selectedType` 决定 CLI 走
//!   OAuth（`oauth-personal`）还是 API Key（`gemini-api-key`）。
//!
//! 官方 OAuth 凭据（`oauth_creds.json`）由 Gemini CLI 自己管理：切中转站
//! 不需要备份，切回官方只需把 selectedType 设回 oauth-personal。

pub mod detect;
pub mod discovery;
pub mod patch;

pub use detect::{GeminiInstallation, detect_gemini, locate_gemini};
pub use discovery::{GeminiConfigPaths, discover_config_paths};
pub use patch::{
    AUTH_TYPE_API_KEY, AUTH_TYPE_OAUTH, AUTH_TYPE_PATH, PROVIDER_ENV_KEYS, auth_type_patch,
    provider_env_patch, remove_provider_env_patch,
};
