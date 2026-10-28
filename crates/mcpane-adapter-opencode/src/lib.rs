//! OpenCode Adapter。
//!
//! 与切换式客户端（Claude 系 / Codex / Gemini / Grok）的根本差异：
//! **OpenCode 是 additive 的**。
//! `~/.config/opencode/opencode.json` 里的 `provider` 表可以同时住着任意多个
//! 供应商，OpenCode 自己在 TUI 里选用哪个模型——没有"当前生效项"这回事。
//! 所以这里的动作是"写进配置 / 从配置移除"，而不是"切过去 / 切回来"。
//! 同理，OpenCode 自己不提供模型，也就**没有官方订阅**（cc-switch 的
//! `OFFICIAL_SEEDS` 里同样没有它）。
//!
//! 一条 provider 的形状（对齐 cc-switch `OpenCodeProviderConfig`，核对 2026-07-25）：
//!
//! ```json
//! {
//!   "provider": {
//!     "<id>": {
//!       "npm": "@ai-sdk/openai-compatible",
//!       "name": "Kimi",
//!       "options": { "baseURL": "https://api.moonshot.cn/v1", "apiKey": "sk-..." },
//!       "models": { "kimi-k2.7-code": { "name": "Kimi K2.7 Code" } }
//!     }
//!   }
//! }
//! ```

pub mod detect;
pub mod discovery;
pub mod patch;

pub use detect::{OpenCodeInstallation, detect_opencode, locate_opencode};
pub use discovery::{OpenCodeConfigPaths, discover_config_paths};
pub use patch::{
    ANTHROPIC_NPM, DEFAULT_NPM, PROVIDER_TABLE_KEY, ProviderEntry, provider_entry_patch,
    provider_ids, remove_provider_patch,
};
