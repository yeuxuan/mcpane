//! Hermes Adapter × 配置安全引擎端到端测试。
//!
//! 覆盖 AGENTS.md 规则 10：配置写入必须带失败恢复测试。
//! Hermes 有两处与其他家不同，都只有落盘才验得出来：
//! - **YAML 不保注释**（生态无成熟 round-trip 库，参考实现同样整份读写）。
//!   这是既定取舍，本文件把它锁成显式契约：注释会掉，但**数据一条不能少**，
//!   且写前快照留着原文可回滚——测试要证明这个兜底真的成立。
//! - **供应商容器是数组**，走「整块数组改写」，容易连带抹掉我们不管的字段。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use mcpane_adapter_hermes::{
    ANTHROPIC_API_MODE, CUSTOM_PROVIDERS_KEY, DEFAULT_API_MODE, PROVIDERS_DICT_KEY, ProviderEntry,
    dict_provider_entries, provider_names, remove_patch, upsert_patch,
};
use mcpane_config::history::JsonlHistoryStore;
use mcpane_config::snapshot::FsSnapshotStore;
use mcpane_config::{
    ConfigDocument, ConfigEngine, ConfigError, ConfigFormat, FsOps, PatchPlan, RealFs,
};

type Engine = ConfigEngine<FsSnapshotStore, JsonlHistoryStore, RealFs>;
const FORMAT: ConfigFormat = ConfigFormat::Yaml;

/// 用户手写的 config.yaml：自建供应商带一批我们不管的字段，外加其他顶层配置。
const USER_CONFIG: &str = r#"agent:
  max_turns: 50
  verbose: true
custom_providers:
  - name: acme
    base_url: https://acme.example.com/v1
    api_key: sk-acme
    request_timeout_seconds: 300
    key_env: ACME_API_KEY
    rate_limit_delay: 2
"#;

struct Bed {
    _dir: tempfile::TempDir,
    home: PathBuf,
    config: PathBuf,
}

fn bed() -> Bed {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join(".hermes/config.yaml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, USER_CONFIG).unwrap();
    Bed {
        home: dir.path().join("mcpane-home"),
        config,
        _dir: dir,
    }
}

fn engine(home: &Path) -> Engine {
    ConfigEngine::new(
        FsSnapshotStore::new(home),
        JsonlHistoryStore::new(home),
        RealFs,
    )
}

fn live(path: &Path) -> ConfigDocument {
    ConfigDocument::parse(&std::fs::read_to_string(path).unwrap(), FORMAT).unwrap()
}

fn apply(eng: &Engine, path: &Path, plan: &PatchPlan, why: &str) {
    let loaded = eng.load(path, FORMAT).unwrap();
    eng.apply(&loaded, plan, why, None, None).unwrap();
}

fn entry<'a>(name: &'a str, models: &'a [String], api_mode: &'a str) -> ProviderEntry<'a> {
    ProviderEntry {
        name,
        base_url: "https://relay.example.com/v1",
        api_key: "sk-relay",
        api_mode,
        models,
        context_length: Some(200_000),
    }
}

/// 追加一家：用户自建那条原位保留（含我们不管的字段），其他顶层配置不动。
#[test]
fn appending_preserves_the_users_entry_and_its_unmanaged_fields() {
    let bed = bed();
    let eng = engine(&bed.home);
    let models = vec!["claude-opus-4-8".to_owned()];

    let existing = live(&bed.config).get(&[CUSTOM_PROVIDERS_KEY]);
    apply(
        &eng,
        &bed.config,
        &upsert_patch(
            existing.as_ref(),
            &entry("relay", &models, DEFAULT_API_MODE),
        ),
        "接入 relay",
    );

    let doc = live(&bed.config);
    let list = doc.get(&[CUSTOM_PROVIDERS_KEY]).unwrap();
    assert_eq!(list[0]["name"], "acme", "用户自建条目原位保留");
    assert_eq!(
        list[0]["request_timeout_seconds"], 300,
        "我们不管的字段不得被整块改写抹掉"
    );
    assert_eq!(list[0]["key_env"], "ACME_API_KEY");
    assert_eq!(list[0]["rate_limit_delay"], 2);
    assert_eq!(list[1]["name"], "relay");
    assert_eq!(list[1]["model"], "claude-opus-4-8", "默认模型取列表首项");
    assert_eq!(
        list[1]["models"]["claude-opus-4-8"]["context_length"],
        200_000
    );
    assert_eq!(doc.get(&["agent", "max_turns"]).unwrap(), 50);
    assert_eq!(doc.get(&["agent", "verbose"]).unwrap(), true);
}

/// 就地更新自建条目：我们的键更新，用户在 Hermes Web UI 里设过的键全留着。
#[test]
fn upserting_the_users_own_entry_keeps_fields_we_do_not_manage() {
    let bed = bed();
    let eng = engine(&bed.home);
    let models = vec!["m".to_owned()];

    let existing = live(&bed.config).get(&[CUSTOM_PROVIDERS_KEY]);
    apply(
        &eng,
        &bed.config,
        &upsert_patch(existing.as_ref(), &entry("acme", &models, DEFAULT_API_MODE)),
        "更新 acme",
    );

    let doc = live(&bed.config);
    let list = doc.get(&[CUSTOM_PROVIDERS_KEY]).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1, "同名原位更新，不新增");
    assert_eq!(list[0]["base_url"], "https://relay.example.com/v1");
    assert_eq!(list[0]["api_key"], "sk-relay");
    assert_eq!(list[0]["request_timeout_seconds"], 300, "必须保留");
    assert_eq!(list[0]["key_env"], "ACME_API_KEY");
}

/// 移除最后一条时把整个键删掉，不留 `custom_providers: []` 空壳。
#[test]
fn removing_the_last_entry_drops_the_container_key() {
    let bed = bed();
    let eng = engine(&bed.home);

    let existing = live(&bed.config).get(&[CUSTOM_PROVIDERS_KEY]);
    apply(
        &eng,
        &bed.config,
        &remove_patch(existing.as_ref(), "acme"),
        "移除 acme",
    );

    let doc = live(&bed.config);
    assert!(
        doc.get(&[CUSTOM_PROVIDERS_KEY]).is_none(),
        "空数组不该留成 custom_providers: []"
    );
    assert_eq!(
        doc.get(&["agent", "max_turns"]).unwrap(),
        50,
        "其他配置还在"
    );
}

/// `api_mode` 恒显式写出并可来回切：不写就留旧值，报文格式与上游对不上。
#[test]
fn api_mode_switches_both_ways_on_disk() {
    let bed = bed();
    let eng = engine(&bed.home);
    let models = vec!["m".to_owned()];

    let existing = live(&bed.config).get(&[CUSTOM_PROVIDERS_KEY]);
    apply(
        &eng,
        &bed.config,
        &upsert_patch(
            existing.as_ref(),
            &entry("relay", &models, ANTHROPIC_API_MODE),
        ),
        "anthropic 形态",
    );
    let existing = live(&bed.config).get(&[CUSTOM_PROVIDERS_KEY]);
    apply(
        &eng,
        &bed.config,
        &upsert_patch(
            existing.as_ref(),
            &entry("relay", &models, DEFAULT_API_MODE),
        ),
        "改回 chat 形态",
    );

    let list = live(&bed.config).get(&[CUSTOM_PROVIDERS_KEY]).unwrap();
    let relay = list
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "relay")
        .unwrap();
    assert_eq!(
        relay["api_mode"], "chat_completions",
        "旧的 anthropic_messages 不得残留"
    );
}

/// YAML 这一路**不保注释**是既定取舍（见模块文档）。这条把取舍锁成显式契约：
/// 注释确实会掉，但（1）数据一条不少、（2）写前快照留着原文，回滚拿得回来。
/// 没有第二条，这个取舍就不成立。
#[test]
fn comments_are_lost_by_design_but_data_survives_and_rollback_restores_the_original() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    let commented = "# 我的 Hermes 配置\nagent:\n  max_turns: 50  # 别调大\ncustom_providers:\n  - name: acme\n    api_key: sk-acme\n";
    std::fs::write(&config, commented).unwrap();
    let eng = engine(&dir.path().join("mcpane-home"));
    let models = vec!["m".to_owned()];

    let loaded = eng.load(&config, FORMAT).unwrap();
    let existing = live(&config).get(&[CUSTOM_PROVIDERS_KEY]);
    let outcome = eng
        .apply(
            &loaded,
            &upsert_patch(
                existing.as_ref(),
                &entry("relay", &models, DEFAULT_API_MODE),
            ),
            "接入",
            None,
            None,
        )
        .unwrap();

    let on_disk = std::fs::read_to_string(&config).unwrap();
    assert!(
        !on_disk.contains("# 我的 Hermes 配置"),
        "YAML 这一路不保注释（既定取舍）——若哪天保住了，说明换了实现，本测试应随之更新"
    );
    // 数据一条不少。
    let doc = live(&config);
    assert_eq!(doc.get(&["agent", "max_turns"]).unwrap(), 50);
    assert_eq!(
        doc.get(&[CUSTOM_PROVIDERS_KEY]).unwrap()[0]["api_key"],
        "sk-acme"
    );
    // 兜底成立：快照回滚能逐字找回带注释的原文。
    eng.rollback(outcome.record.snapshot.as_ref().unwrap(), FORMAT)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        commented,
        "注释被规整掉之后，用户必须能从历史里逐字找回原文"
    );
}

/// v12+ 的 `providers:` 字典由 Hermes 自己管理，我们只读不写——落盘后
/// 那个容器必须一个字节都没动过。
#[test]
fn the_hermes_managed_dict_container_is_never_written() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    let v12 = "custom_providers:\n  - name: acme\n    api_key: sk-acme\nproviders:\n  webui-one:\n    base_url: https://from-webui\n";
    std::fs::write(&config, v12).unwrap();
    let eng = engine(&dir.path().join("mcpane-home"));
    let models = vec!["m".to_owned()];

    let existing = live(&config).get(&[CUSTOM_PROVIDERS_KEY]);
    apply(
        &eng,
        &config,
        &upsert_patch(
            existing.as_ref(),
            &entry("relay", &models, DEFAULT_API_MODE),
        ),
        "接入",
    );
    apply(
        &eng,
        &config,
        &remove_patch(live(&config).get(&[CUSTOM_PROVIDERS_KEY]).as_ref(), "relay"),
        "移除",
    );

    let doc = live(&config);
    assert_eq!(
        doc.get(&[PROVIDERS_DICT_KEY, "webui-one", "base_url"])
            .unwrap(),
        "https://from-webui",
        "Hermes 托管的字典容器不得被我们改动"
    );
    // 数组同名遮蔽字典的读取语义在落盘后依然成立。
    let entries = dict_provider_entries(
        doc.get(&[PROVIDERS_DICT_KEY]).as_ref(),
        doc.get(&[CUSTOM_PROVIDERS_KEY]).as_ref(),
    );
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "webui-one");
    assert_eq!(
        provider_names(doc.get(&[CUSTOM_PROVIDERS_KEY]).as_ref()),
        vec!["acme".to_owned()]
    );
}

/// 写入失败后配置一字未变（含注释——根本没走到序列化那一步）。
#[test]
fn write_failure_leaves_config_byte_identical() {
    let bed = bed();
    let failing = ConfigEngine::new(
        FsSnapshotStore::new(&bed.home),
        JsonlHistoryStore::new(&bed.home),
        FailingWriteFs { inner: RealFs },
    );
    let models = vec!["m".to_owned()];

    let loaded = failing.load(&bed.config, FORMAT).unwrap();
    let result = failing.apply(
        &loaded,
        &upsert_patch(None, &entry("relay", &models, DEFAULT_API_MODE)),
        "注入失败",
        None,
        None,
    );
    assert!(result.is_err());
    assert_eq!(std::fs::read_to_string(&bed.config).unwrap(), USER_CONFIG);
}

/// 外部改动必须被拒。
#[test]
fn external_edit_is_rejected() {
    let bed = bed();
    let eng = engine(&bed.home);
    let models = vec!["m".to_owned()];

    let loaded = eng.load(&bed.config, FORMAT).unwrap();
    let edited = USER_CONFIG.replace("sk-acme", "sk-acme-rotated");
    std::fs::write(&bed.config, &edited).unwrap();

    let result = eng.apply(
        &loaded,
        &upsert_patch(None, &entry("relay", &models, DEFAULT_API_MODE)),
        "接入",
        None,
        None,
    );
    assert!(matches!(result, Err(ConfigError::Conflict { .. })));
    assert_eq!(std::fs::read_to_string(&bed.config).unwrap(), edited);
}

/// 手改坏的 YAML 必须报解析错误且不碰磁盘。
#[test]
fn corrupt_yaml_fails_to_parse_and_never_touches_disk() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    let broken = "custom_providers:\n  - name: acme\n   bad_indent: x\n";
    std::fs::write(&config, broken).unwrap();
    let eng = engine(&dir.path().join("mcpane-home"));
    let models = vec!["m".to_owned()];

    let loaded = eng.load(&config, FORMAT).unwrap();
    let result = eng.apply(
        &loaded,
        &upsert_patch(None, &entry("relay", &models, DEFAULT_API_MODE)),
        "接入",
        None,
        None,
    );
    assert!(matches!(result, Err(ConfigError::Parse { .. })));
    assert_eq!(std::fs::read_to_string(&config).unwrap(), broken);
}

/// 磁盘上那条不是映射（配置被手改坏）时，upsert 用新值兜底让切换仍能成功，
/// 且不得把整个数组搞丢。
#[test]
fn a_malformed_array_element_is_replaced_instead_of_blocking_the_switch() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    std::fs::write(
        &config,
        "custom_providers:\n  - just-a-string\n  - name: keep\n",
    )
    .unwrap();
    let eng = engine(&dir.path().join("mcpane-home"));
    let models = vec!["m".to_owned()];

    let existing = live(&config).get(&[CUSTOM_PROVIDERS_KEY]);
    apply(
        &eng,
        &config,
        &upsert_patch(
            existing.as_ref(),
            &entry("relay", &models, DEFAULT_API_MODE),
        ),
        "接入",
    );

    let list = live(&config).get(&[CUSTOM_PROVIDERS_KEY]).unwrap();
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 3, "坏元素与其他条目都得留着，只追加我们这条");
    assert_eq!(arr[0], "just-a-string", "认不出的元素原样保留");
    assert_eq!(arr[1]["name"], "keep");
    assert_eq!(arr[2]["name"], "relay");
}

/// 磁盘写入失败注入。
struct FailingWriteFs {
    inner: RealFs,
}

impl FsOps for FailingWriteFs {
    fn read_to_string(&self, path: &Path) -> std::io::Result<String> {
        self.inner.read_to_string(path)
    }
    fn write(&self, _path: &Path, _content: &str) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::StorageFull,
            "injected disk full",
        ))
    }
    fn fsync(&self, path: &Path) -> std::io::Result<()> {
        self.inner.fsync(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        self.inner.rename(from, to)
    }
    fn remove_file(&self, path: &Path) -> std::io::Result<()> {
        self.inner.remove_file(path)
    }
    fn mtime_unix_nanos(&self, path: &Path) -> Option<u128> {
        self.inner.mtime_unix_nanos(path)
    }
    fn exists(&self, path: &Path) -> bool {
        self.inner.exists(path)
    }
}
