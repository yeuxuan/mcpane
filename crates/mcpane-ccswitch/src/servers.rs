//! cc-switch 的 MCP 服务记录 → 标准 MCP entry JSON。
//!
//! 只做取出与清洗：`server_config` 存的就是标准 entry（`command`/`args`/`env`
//! 或 `url`），交给桌面端现成的导入通道解析成 ServerDef，不在这里另解一遍。

use serde_json::Value;
use sqlx::Row;

pub(crate) fn from_rows(rows: &[sqlx::sqlite::SqliteRow]) -> Vec<(String, Value)> {
    rows.iter()
        .filter_map(|row| {
            let name: String = row.try_get("name").ok()?;
            let config: String = row.try_get("server_config").ok()?;
            let value: Value = serde_json::from_str(&config).ok()?;
            value.is_object().then_some((name, value))
        })
        .collect()
}

/// 旧版 `config.json` 的 `mcp.servers`：值是宽松对象，除 entry 本身还混着
/// `enabled` / `source` 这类 UI 辅助字段——真正的 entry 在 `server` 下（新结构）
/// 或就是它自己（v3.6.x 及更早）。
pub(crate) fn from_legacy(root: &Value) -> Vec<(String, Value)> {
    let Some(servers) = root
        .pointer("/mcp/servers")
        .and_then(Value::as_object)
        .or_else(|| {
            root.pointer("/mcp/config/servers")
                .and_then(Value::as_object)
        })
    else {
        return Vec::new();
    };
    servers
        .iter()
        .filter_map(|(id, entry)| {
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(id)
                .to_owned();
            let config = entry.get("server").unwrap_or(entry);
            config.is_object().then(|| (name, config.clone()))
        })
        .collect()
}
