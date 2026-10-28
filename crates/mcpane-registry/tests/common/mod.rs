//! 真实抓取的官方 Registry 条目（按服务器名索引）。
use std::collections::BTreeMap;

use mcpane_registry::model::Entry;

#[allow(clippy::expect_used)]
pub fn entries() -> BTreeMap<String, Entry> {
    let raw = include_str!("../fixtures/registry-entries.json");
    serde_json::from_str(raw).expect("夹具应能解析")
}

#[allow(clippy::expect_used)]
pub fn entry(name: &str) -> Entry {
    entries()
        .remove(name)
        .unwrap_or_else(|| panic!("夹具里没有 {name}"))
}
