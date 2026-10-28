//! Grok CLI Adapter。
//!
//! 职责：检测安装、发现配置、生成最小 Patch（自定义模型表 + 默认模型指针）。
//! 所有写入统一交给 `mcpane-config` 引擎执行（Diff/Snapshot/原子写/回滚）。
//!
//! Grok CLI 的供应商配置全在 `~/.grok/config.toml`（按 Grok CLI 实测，
//! 核对日期 2026-07-25）：
//!
//! ```toml
//! [models]
//! default = "mcpane-relay"      # 指向下面某张模型表
//!
//! [model."mcpane-relay"]
//! model = "grok-4.5"            # 上游真实模型名
//! base_url = "https://api.example.com/v1"
//! name = "显示名"
//! api_key = "sk-..."            # 或 env_key = "XAI_API_KEY"
//! api_backend = "chat"          # 走 OpenAI 兼容 chat completions
//! context_window = 500000       # grok-4.5 的实际窗口
//! ```
//!
//! **官方形态 = 配置里没有我们写的模型表**：Grok CLI 回落到内置模型 +
//! 自带的 xAI OAuth 登录，凭据由 CLI 自管，MCPane 不备份也不触碰。
//! 因此切回官方只需移除受管表与指针，不需要像 Codex 那样备份 auth.json。

pub mod detect;
pub mod discovery;
pub mod patch;

pub use detect::{GrokInstallation, detect_grok, locate_grok};
pub use discovery::{GrokConfigPaths, discover_config_paths};
pub use patch::{
    DEFAULT_API_BACKEND, DEFAULT_CONTEXT_WINDOW, DefaultPointerDisposition, GrokModelTable,
    MODEL_TABLE_KEY, MODELS_DEFAULT_PATH, SLUG_PREFIX, model_table_patch, remove_model_table_patch,
    table_slug,
};
