//! OpenClaw Adapter。
//!
//! 与 OpenCode 同为 **additive**（多供应商并存、无当前生效项、无官方订阅），
//! 但配置文件是 **JSON5**：`~/.openclaw/openclaw.json` 虽然叫 .json，内容却是
//! 无引号键 + 单引号 + 尾逗号（OpenClaw 自带的初始配置就长这样）。用严格 JSON
//! 解析器读它会直接失败，所以走引擎的 `ConfigFormat::Json5`（保注释保风格）。
//!
//! 供应商住在 `models.providers.<id>`（按 OpenClaw 自带初始配置与实测，
//! 核对 2026-07-25）：
//!
//! ```json5
//! {
//!   models: {
//!     mode: 'merge',
//!     providers: {
//!       kimi: {
//!         baseUrl: 'https://api.moonshot.cn/v1',
//!         apiKey: 'sk-...',
//!         models: [{ id: 'kimi-k2.7-code' }],
//!       },
//!     },
//!   },
//! }
//! ```
pub mod detect;
pub mod discovery;
pub mod patch;

pub use detect::{OpenClawInstallation, detect_openclaw, locate_openclaw};
pub use discovery::{OpenClawConfigPaths, discover_config_paths};
pub use patch::{
    ANTHROPIC_API, DEFAULT_API, DEFAULT_MODEL_PRIMARY_PATH, MODEL_CATALOG_PATH, PROVIDERS_PATH,
    ProviderEntry, default_model_patch, model_ref, provider_entry_patch, provider_ids,
    remove_provider_patch,
};
