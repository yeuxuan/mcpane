//! Hermes Adapter。
//!
//! additive（多供应商并存、无当前生效项、无官方订阅），配置是
//! `~/.hermes/config.yaml`。两点与前面几家不同：
//! - **YAML**：引擎这一路不保注释（生态无成熟 round-trip 库，参考实现同样整份
//!   读写）；写前快照与历史回滚照旧，格式被规整时用户能找回原文。
//! - **供应商容器是数组**（按 `name` 定位），故写入走"整个数组改写"，详见
//!   [`patch`] 模块文档。
pub mod detect;
pub mod discovery;
pub mod patch;

pub use detect::{HermesInstallation, detect_hermes, locate_hermes};
pub use discovery::{HermesConfigPaths, discover_config_paths};
pub use patch::{
    ANTHROPIC_API_MODE, CUSTOM_PROVIDERS_KEY, DEFAULT_API_MODE, DictProvider, PROVIDERS_DICT_KEY,
    ProviderEntry, dict_provider_entries, provider_names, remove_patch, upsert_patch,
};
