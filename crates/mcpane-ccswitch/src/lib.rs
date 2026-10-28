//! cc-switch 迁移：把用户已装的 cc-switch 里的供应商与 MCP 服务读出来，
//! 交给 MCPane 入库。
//!
//! 只读，且**不碰用户正在用的那个文件**：cc-switch 可能正开着，其数据库是 WAL
//! 模式——只读打开仍需要对 `-wal` 有写权限。所以先把三件套复制到临时目录再读
//! （见 [`open_readonly_copy`]）。
//!
//! 数据形态两种，都要认：v3.7.0 起是 `~/.cc-switch/cc-switch.db`（SQLite），
//! 之前是 `config.json`（升级后被改名成 `config.json.migrated`）。

mod providers;
mod servers;

use std::path::{Path, PathBuf};

pub use providers::ImportedProvider;

#[derive(Debug, thiserror::Error)]
pub enum CcSwitchError {
    #[error("cc-switch 的数据打不开：{0}")]
    Db(#[from] sqlx::Error),
    #[error("IO 错误：{0}")]
    Io(#[from] std::io::Error),
    #[error("cc-switch 的配置读不出内容：{0}")]
    Parse(String),
}

/// cc-switch 的数据落点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CcSwitchSource {
    /// v3.7.0 起的主存储。
    Db(PathBuf),
    /// v3.7.0 之前的 `config.json`（或迁移后归档的 `.migrated`）。
    LegacyJson(PathBuf),
}

impl CcSwitchSource {
    pub fn path(&self) -> &Path {
        match self {
            Self::Db(p) | Self::LegacyJson(p) => p,
        }
    }
}

/// 读出来的全部内容。`warnings` 是"读到了但没法搬"的逐条说明——静默丢弃会让
/// 用户对着少掉的那条供应商无从归因。
#[derive(Debug, Default)]
pub struct CcSwitchData {
    pub providers: Vec<ImportedProvider>,
    /// MCP 服务：名称 → 标准 MCP entry JSON（`command`/`args`/`url` 等）。
    pub servers: Vec<(String, serde_json::Value)>,
    pub warnings: Vec<String>,
    /// 用户在 cc-switch 里调过（与默认值不同）的路由超时 / 熔断参数，已截到 MCPane 允许范围。
    pub routing_tuning: Vec<(mcpane_provider::ClientApp, mcpane_provider::RoutingTuning)>,
}

/// cc-switch 的数据目录。默认 `~/.cc-switch`，但它支持挪走（设置里改数据目录，
/// 多见于云盘同步）——覆盖值存在自己的 Tauri Store `app_paths.json` 里。不读这份
/// 覆盖的话，这类用户会被告知"没装 cc-switch"，而他们恰恰是配置最多的那批。
pub fn config_dir(home: &Path) -> PathBuf {
    override_dir(home)
        .filter(|dir| dir.is_dir())
        .unwrap_or_else(|| home.join(".cc-switch"))
}

/// cc-switch 的 Tauri Store 落点（identifier 为 `com.ccswitch.desktop`）。
fn store_path(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Application Support/com.ccswitch.desktop/app_paths.json")
    }
    #[cfg(target_os = "windows")]
    {
        // Store 落在 %APPDATA%（Roaming）下，而 home 是 %USERPROFILE%。
        std::env::var("APPDATA").map_or_else(
            |_| home.join("AppData/Roaming/com.ccswitch.desktop/app_paths.json"),
            |appdata| PathBuf::from(appdata).join("com.ccswitch.desktop/app_paths.json"),
        )
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        home.join(".config/com.ccswitch.desktop/app_paths.json")
    }
}

fn override_dir(home: &Path) -> Option<PathBuf> {
    let raw = std::fs::read_to_string(store_path(home)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let dir = value.get("app_config_dir_override")?.as_str()?.trim();
    if dir.is_empty() {
        return None;
    }
    // cc-switch 自己也做 `~` 展开，存进去的可能就是带 `~` 的原样字符串。
    Some(
        match dir.strip_prefix("~/").or_else(|| dir.strip_prefix("~\\")) {
            Some(rest) => home.join(rest),
            None if dir == "~" => home.to_path_buf(),
            None => PathBuf::from(dir),
        },
    )
}

/// 找 cc-switch 的数据。db 优先——它存在时 `config.json` 必然是历史遗留。
pub fn discover(home: &Path) -> Option<CcSwitchSource> {
    let dir = config_dir(home);
    let db = dir.join("cc-switch.db");
    if db.is_file() {
        return Some(CcSwitchSource::Db(db));
    }
    for name in ["config.json", "config.json.migrated"] {
        let path = dir.join(name);
        if path.is_file() {
            return Some(CcSwitchSource::LegacyJson(path));
        }
    }
    None
}

/// 手选文件时按扩展名判断形态（用户可能指向备份目录里的任意一份）。
pub fn source_from_path(path: PathBuf) -> CcSwitchSource {
    if path.extension().is_some_and(|ext| ext == "db") {
        CcSwitchSource::Db(path)
    } else {
        CcSwitchSource::LegacyJson(path)
    }
}

/// 读出全部可迁移内容。
pub async fn read_all(source: &CcSwitchSource) -> Result<CcSwitchData, CcSwitchError> {
    match source {
        CcSwitchSource::Db(path) => read_db(path).await,
        CcSwitchSource::LegacyJson(path) => read_legacy_json(path),
    }
}

async fn read_db(path: &Path) -> Result<CcSwitchData, CcSwitchError> {
    let (_guard, copy) = open_readonly_copy(path)?;
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&copy)
        .read_only(true)
        .create_if_missing(false);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;

    let mut data = CcSwitchData::default();
    let rows = sqlx::query("SELECT * FROM providers")
        .fetch_all(&pool)
        .await?;
    let raws: Vec<providers::RawProvider> =
        rows.iter().filter_map(providers::raw_from_row).collect();
    providers::convert_all(raws, &mut data);

    // 代理配置表只在较新的 schema 里有；没有就不搬。
    if let Ok(rows) = sqlx::query("SELECT * FROM proxy_config")
        .fetch_all(&pool)
        .await
    {
        data.routing_tuning = rows.iter().filter_map(tuning_from_row).collect();
    }

    // MCP 表在极早的 schema 里可能不存在；那不是错误，只是没东西可搬。
    if let Ok(rows) = sqlx::query("SELECT name, server_config FROM mcp_servers")
        .fetch_all(&pool)
        .await
    {
        data.servers = servers::from_rows(&rows);
    }
    pool.close().await;
    Ok(data)
}

/// 一行 `proxy_config` → 路由调参；app 不支持路由、或各项都是默认值时不搬。
fn tuning_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Option<(mcpane_provider::ClientApp, mcpane_provider::RoutingTuning)> {
    use sqlx::Row;
    let app = match row.try_get::<String, _>("app_type").ok()?.as_str() {
        "claude" => mcpane_provider::ClientApp::Claude,
        "codex" => mcpane_provider::ClientApp::Codex,
        "gemini" => mcpane_provider::ClientApp::Gemini,
        "grokbuild" => mcpane_provider::ClientApp::Grok,
        _ => return None,
    };
    let default = mcpane_provider::RoutingTuning::default();
    let ranges = mcpane_provider::RoutingTuning::RANGES;
    let read = |column: &str, fallback: u32, (_, min, max): (&str, u32, u32)| {
        row.try_get::<i64, _>(column)
            .ok()
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(fallback)
            .clamp(min, max)
    };
    let tuning = mcpane_provider::RoutingTuning {
        first_byte_timeout_secs: read(
            "streaming_first_byte_timeout",
            default.first_byte_timeout_secs,
            ranges[0],
        ),
        idle_timeout_secs: read(
            "streaming_idle_timeout",
            default.idle_timeout_secs,
            ranges[1],
        ),
        failure_threshold: read(
            "circuit_failure_threshold",
            default.failure_threshold,
            ranges[2],
        ),
        cooldown_secs: read("circuit_timeout_seconds", default.cooldown_secs, ranges[3]),
    };
    (tuning != default).then_some((app, tuning))
}

fn read_legacy_json(path: &Path) -> Result<CcSwitchData, CcSwitchError> {
    let content = std::fs::read_to_string(path)?;
    let root: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| CcSwitchError::Parse(e.to_string()))?;
    let mut data = CcSwitchData::default();
    providers::convert_all(providers::raw_from_legacy(&root), &mut data);
    data.servers = servers::from_legacy(&root);
    Ok(data)
}

/// 把 db 连同 `-wal` / `-shm` 复制到临时目录后再打开。
/// 返回的 `TempDir` 必须与连接同生命周期——drop 掉目录就没了。
fn open_readonly_copy(path: &Path) -> Result<(tempfile::TempDir, PathBuf), CcSwitchError> {
    let dir = tempfile::tempdir()?;
    let dest = dir.path().join("cc-switch.db");
    std::fs::copy(path, &dest)?;
    for suffix in ["-wal", "-shm"] {
        let mut side = path.as_os_str().to_owned();
        side.push(suffix);
        let side = PathBuf::from(side);
        if side.is_file() {
            let mut target = dest.as_os_str().to_owned();
            target.push(suffix);
            std::fs::copy(&side, PathBuf::from(target))?;
        }
    }
    Ok((dir, dest))
}
