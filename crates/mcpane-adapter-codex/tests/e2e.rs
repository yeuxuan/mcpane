//! Codex Adapter × 配置安全引擎端到端测试（GATE-2）。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use mcpane_adapter_codex::{hub_entry_patch, remove_hub_entry_patch, validate_hub_entry};
use mcpane_config::history::JsonlHistoryStore;
use mcpane_config::snapshot::FsSnapshotStore;
use mcpane_config::{ConfigEngine, ConfigFormat, CredentialDelivery, CredentialMode, RealFs};

fn fixture() -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/codex-configs/user-config.toml"),
    )
    .unwrap()
}

#[test]
fn one_click_connect_preserves_comments_and_unknown_tables() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    std::fs::write(&target, fixture()).unwrap();
    let engine = ConfigEngine::new(
        FsSnapshotStore::new(dir.path().join("home")),
        JsonlHistoryStore::new(dir.path().join("home")),
        RealFs,
    );

    let loaded = engine.load(&target, ConfigFormat::Toml).unwrap();
    let outcome = engine
        .apply(
            &loaded,
            &hub_entry_patch(None, CredentialDelivery::Inline("tok-e2e-1234")),
            "一键接入 MCP Hub（Codex）",
            Some(&|doc| validate_hub_entry(doc, CredentialMode::Inline)),
            None,
        )
        .unwrap();
    assert!(outcome.written);

    let after = std::fs::read_to_string(&target).unwrap();
    // GATE-2：注释与未知表逐字保留。
    for must_keep in [
        "# Codex 用户配置（真实风格 fixture）",
        "model = \"gpt-5.2-codex\"  # 用户手选",
        "[mcp_servers.context7]",
        "startup_timeout_sec = 20",
        "[mcp_servers.playwright]",
        "# 这个注释也要保留",
        "[model_providers.my-proxy]",
        "[future_unknown_table.sub]",
    ] {
        assert!(
            after.contains(must_keep),
            "必须保留: {must_keep}\n---\n{after}"
        );
    }
    assert!(after.contains("[mcp_servers.mcpane]"));
    assert!(after.contains("Authorization = \"Bearer tok-e2e-1234\""));
    assert!(!after.contains("bearer_token_env_var"));

    // 解绑后与原文件逐字一致（原有内容位置未被扰动）。
    let loaded = engine.load(&target, ConfigFormat::Toml).unwrap();
    engine
        .apply(&loaded, &remove_hub_entry_patch(), "解绑", None, None)
        .unwrap();
    let restored = std::fs::read_to_string(&target).unwrap();
    assert_eq!(restored.trim(), fixture().trim(), "解绑后应回到原始内容");
}

#[test]
fn external_edit_between_preview_and_apply_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    std::fs::write(&target, fixture()).unwrap();
    let engine = ConfigEngine::new(
        FsSnapshotStore::new(dir.path().join("home")),
        JsonlHistoryStore::new(dir.path().join("home")),
        RealFs,
    );

    let loaded = engine.load(&target, ConfigFormat::Toml).unwrap();
    // 用户在 Diff 预览期间手工编辑了文件。
    std::fs::write(&target, format!("{}\nuser_edit = true\n", fixture())).unwrap();

    let result = engine.apply(
        &loaded,
        &hub_entry_patch(None, CredentialDelivery::Inline("tok")),
        "接入",
        None,
        None,
    );
    assert!(matches!(
        result,
        Err(mcpane_config::ConfigError::Conflict { .. })
    ));
    let content = std::fs::read_to_string(&target).unwrap();
    assert!(content.contains("user_edit = true"), "用户编辑必须原样保留");
    assert!(!content.contains("mcpane"), "冲突时不得写入任何内容");
}
