//! Tauri 命令层：桥接前端与 mcpane-* 服务（Phase 5）。
//!
//! - Hub 相关一律走控制面 UDS（ADR-007），不经数据面 `/mcp`；
//! - Provider CRUD 走 `mcpane-provider`（Keychain Secret，JSON 存储）；
//! - Secret 明文只在 set_provider_api_key 的入参瞬时存在，绝不回传前端。
//!
//! 按业务域分模块，全部经此处 re-export——`crate::commands::X` 路径保持稳定。

/// 数据目录与 socket 路径的唯一事实源（与 hub 共享，含 SUN_LEN 回退）。
pub(crate) use mcpane_hub_protocol::mcpane_home;

/// 进程级共享 SQLite 池：首次调用建池 + 跑迁移，之后所有命令复用。
/// 初始化失败不缓存，下次调用重试。
static DB_POOL: tokio::sync::OnceCell<mcpane_storage::SqlitePool> =
    tokio::sync::OnceCell::const_new();

pub(crate) async fn db_pool() -> Result<mcpane_storage::SqlitePool, String> {
    DB_POOL
        .get_or_try_init(|| async {
            mcpane_storage::open_database(&mcpane_home().join("mcpane.db"))
                .await
                .map_err(|e| e.to_string())
        })
        .await
        .cloned()
}

mod autostart;
mod ccswitch;
mod connect;
mod data;
mod detect;
mod guidance;
mod history;
mod hub;
mod import;
mod import_link;
mod init;
mod logs;
mod pricing;
mod provider;
mod server_discover;
mod server_export;
mod server_oauth;
mod servers;
mod settings;
mod sync;
pub mod updater;
mod usage;
mod workspace;

pub use autostart::*;
pub use ccswitch::*;
pub use connect::*;
pub use data::*;
pub use detect::*;
pub use guidance::*;
pub use history::*;
pub use hub::*;
pub use import::*;
pub use import_link::*;
pub use init::*;
pub use logs::*;
pub use pricing::*;
pub use provider::*;
pub use server_discover::*;
pub use server_export::*;
pub use server_oauth::*;
pub use servers::*;
pub use settings::*;
pub use sync::*;
pub use updater::*;
pub use usage::*;
pub use workspace::*;
