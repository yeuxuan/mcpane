//! Claude Adapter × 配置安全引擎端到端测试（GATE-2）。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use mcpane_adapter_claude::{hub_entry_patch, remove_hub_entry_patch, validate_hub_entry};
use mcpane_config::history::JsonlHistoryStore;
use mcpane_config::snapshot::FsSnapshotStore;
use mcpane_config::{ConfigEngine, ConfigFormat, CredentialDelivery, CredentialMode, RealFs};

fn fixture() -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/claude-configs/user-claude.json"),
    )
    .unwrap()
}

#[test]
fn one_click_connect_and_disconnect_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join(".claude.json");
    std::fs::write(&target, fixture()).unwrap();
    let engine = ConfigEngine::new(
        FsSnapshotStore::new(dir.path().join("home")),
        JsonlHistoryStore::new(dir.path().join("home")),
        RealFs,
    );
    let prefix = vec!["mcpServers".to_owned()];

    // 一键接入：Diff → Snapshot → 原子写 → 验证。
    let loaded = engine.load(&target, ConfigFormat::Json).unwrap();
    let plan = hub_entry_patch(&prefix, None, CredentialDelivery::Inline("tok-e2e-5678"));
    let validate_prefix = prefix.clone();
    let outcome = engine
        .apply(
            &loaded,
            &plan,
            "一键接入 MCP Hub（Claude Code）",
            Some(&move |doc| validate_hub_entry(doc, &validate_prefix, CredentialMode::Inline)),
            None,
        )
        .unwrap();
    assert!(outcome.written);

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&target).unwrap()).unwrap();
    let original: serde_json::Value = serde_json::from_str(&fixture()).unwrap();

    // GATE-2：未知字段 100% 保留（除新增的 mcpane 条目外全等）。
    let mut after_without_entry = after.clone();
    after_without_entry["mcpServers"]
        .as_object_mut()
        .unwrap()
        .remove("mcpane");
    assert_eq!(
        after_without_entry, original,
        "除新增 Entry 外必须与原文件语义全等"
    );
    assert_eq!(after["mcpServers"]["mcpane"]["type"], "http");
    assert_eq!(
        after["mcpServers"]["mcpane"]["headers"]["Authorization"],
        "Bearer tok-e2e-5678"
    );

    // 解绑：删除 Entry，其他一切保留。
    let loaded = engine.load(&target, ConfigFormat::Json).unwrap();
    engine
        .apply(
            &loaded,
            &remove_hub_entry_patch(&prefix),
            "解绑 MCP Hub（Claude Code）",
            None,
            None,
        )
        .unwrap();
    let restored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(restored, original, "解绑后必须回到原始语义");
}

#[test]
fn rollback_restores_pre_connect_state() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join(".claude.json");
    std::fs::write(&target, fixture()).unwrap();
    let engine = ConfigEngine::new(
        FsSnapshotStore::new(dir.path().join("home")),
        JsonlHistoryStore::new(dir.path().join("home")),
        RealFs,
    );
    let prefix = vec!["mcpServers".to_owned()];

    let loaded = engine.load(&target, ConfigFormat::Json).unwrap();
    let outcome = engine
        .apply(
            &loaded,
            &hub_entry_patch(&prefix, None, CredentialDelivery::Inline("tok")),
            "接入",
            None,
            None,
        )
        .unwrap();

    engine
        .rollback(
            outcome.record.snapshot.as_ref().unwrap(),
            ConfigFormat::Json,
        )
        .unwrap();
    let restored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&target).unwrap()).unwrap();
    let original: serde_json::Value = serde_json::from_str(&fixture()).unwrap();
    assert_eq!(restored, original, "回滚必须恢复接入前状态");
}
