//! MCPane 配置安全引擎（TASK-200，ADR-005）。
//!
//! 所有客户端配置写入统一走：
//!
//! ```text
//! 重新读取当前文件 → 解析 → 计算内容哈希 → 生成最小 Patch → 展示 Diff
//! → 创建 Snapshot → 写入同目录临时文件 → fsync → 语法与业务验证
//! → 原子替换 → 重新读取验证 → 写入操作历史
//! ```
//!
//! 强制约束：
//! - 禁止字符串搜索替换；JSON 未知字段必须保留；TOML 注释/未知表尽量保留；
//! - 写入前文件哈希变化 → [`ConfigError::Conflict`]，绝不静默覆盖；
//! - 写入失败自动恢复；回滚前再次备份当前文件。

pub mod atomic_io;
pub mod credential;
pub mod diff;
pub mod document;
pub mod engine;
pub mod error;
pub mod guidance;
pub mod history;
pub(crate) mod json5;
pub mod patch;
pub mod permissions;
pub mod process_output;
pub mod snapshot;
pub mod watch;

pub use credential::{CredentialDelivery, CredentialMode, is_usable_inline_bearer};
pub use diff::ConfigDiff;
pub use document::{
    ConfigDocument, ConfigFormat, EnvDocument, FileFormatting, LoadedConfig, NewlineStyle,
    content_hash,
};
pub use engine::{
    ApplyOutcome, ConfigEngine, FsOps, RealFs, is_recent_own_write, register_own_write,
};
pub use error::ConfigError;
pub use history::{HistoryStore, JsonlHistoryStore, OperationRecord, OperationStatus};
pub use patch::{PatchOp, PatchPlan};
pub use snapshot::{FsSnapshotStore, SnapshotMeta, SnapshotStore};
pub use watch::{ConfigWatcher, watch_config};
