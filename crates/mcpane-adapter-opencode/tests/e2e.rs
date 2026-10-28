//! OpenCode Adapter × 配置安全引擎端到端测试。
//!
//! 覆盖 AGENTS.md 规则 10：配置写入必须带失败恢复测试。
//! OpenCode 是 **additive** 的——`provider` 表里同时住着多家，写一家绝不能碰
//! 其他家，也不能碰 `mcp` 等顶层键。配置按 JSONC 读（`adapter.rs` 绑定
//! `ConfigFormat::Json5`），所以用户写的 `//` 注释必须活着落盘。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mcpane_adapter_opencode::{
    ANTHROPIC_NPM, DEFAULT_NPM, PROVIDER_TABLE_KEY, ProviderEntry, provider_entry_patch,
    provider_ids, remove_provider_patch,
};
use mcpane_config::history::JsonlHistoryStore;
use mcpane_config::snapshot::FsSnapshotStore;
use mcpane_config::{
    ConfigDocument, ConfigEngine, ConfigError, ConfigFormat, FsOps, PatchPlan, RealFs,
};

type Engine = ConfigEngine<FsSnapshotStore, JsonlHistoryStore, RealFs>;

/// 生产上 opencode.json 按 JSONC 读，所以夹具带注释。
const FORMAT: ConfigFormat = ConfigFormat::Json5;

const USER_CONFIG: &str = r#"{
  // 我自己接的供应商，MCPane 不许动
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "mine": {
      "npm": "@ai-sdk/anthropic",
      "options": { "apiKey": "sk-mine" }
    }
  },
  "mcp": {
    "filesystem": { "type": "local", "command": ["npx", "-y", "server-filesystem"] }
  }
}
"#;

struct Bed {
    _dir: tempfile::TempDir,
    home: PathBuf,
    config: PathBuf,
}

fn bed() -> Bed {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join(".config/opencode/opencode.json");
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

fn entry<'a>(
    id: &'a str,
    models: &'a [String],
    headers: &'a BTreeMap<String, String>,
) -> ProviderEntry<'a> {
    ProviderEntry {
        id,
        name: "Kimi",
        npm: DEFAULT_NPM,
        base_url: "https://api.moonshot.cn/v1",
        api_key: "sk-kimi-1234",
        models,
        headers,
    }
}

/// additive 接入：新增一家，其他家与其他顶层键全须全尾，注释也在。
#[test]
fn adding_a_provider_leaves_everything_else_untouched() {
    let bed = bed();
    let eng = engine(&bed.home);
    let models = vec!["kimi-k2.7-code".to_owned()];
    let headers = BTreeMap::new();

    apply(
        &eng,
        &bed.config,
        &provider_entry_patch(&entry("kimi", &models, &headers)),
        "接入 Kimi",
    );

    let on_disk = std::fs::read_to_string(&bed.config).unwrap();
    assert!(
        on_disk.contains("// 我自己接的供应商，MCPane 不许动"),
        "JSONC 注释必须保留：{on_disk}"
    );

    let doc = live(&bed.config);
    assert_eq!(
        doc.get(&["provider", "kimi", "options", "apiKey"]).unwrap(),
        serde_json::json!("sk-kimi-1234")
    );
    assert_eq!(
        doc.get(&["provider", "kimi", "options", "setCacheKey"])
            .unwrap(),
        serde_json::json!(true)
    );
    assert_eq!(
        doc.get(&["provider", "kimi", "models", "kimi-k2.7-code", "name"])
            .unwrap(),
        serde_json::json!("kimi-k2.7-code")
    );
    // 用户自己那家与 mcp 表纹丝不动。
    assert_eq!(
        doc.get(&["provider", "mine", "options", "apiKey"]).unwrap(),
        serde_json::json!("sk-mine")
    );
    assert_eq!(
        doc.get(&["provider", "mine", "npm"]).unwrap(),
        serde_json::json!("@ai-sdk/anthropic")
    );
    assert!(doc.get(&["mcp", "filesystem"]).is_some(), "mcp 表不得被碰");
    assert_eq!(
        doc.get(&["$schema"]).unwrap(),
        serde_json::json!("https://opencode.ai/config.json")
    );
}

/// 从配置移除：只清我们那家，落盘后仍是合法严格 JSON（jq、用户脚本都在读它）。
#[test]
fn removing_our_provider_keeps_the_file_valid_and_others_intact() {
    let bed = bed();
    let eng = engine(&bed.home);
    let models = vec!["m".to_owned()];
    let headers = BTreeMap::new();

    apply(
        &eng,
        &bed.config,
        &provider_entry_patch(&entry("kimi", &models, &headers)),
        "接入",
    );
    apply(
        &eng,
        &bed.config,
        &remove_provider_patch("kimi"),
        "从配置移除",
    );

    let doc = live(&bed.config);
    assert!(doc.get(&["provider", "kimi"]).is_none());
    assert_eq!(
        doc.get(&["provider", "mine", "options", "apiKey"]).unwrap(),
        serde_json::json!("sk-mine")
    );
    assert!(doc.get(&["mcp", "filesystem"]).is_some());

    // 删掉末位键不得留下尾逗号：OpenCode 官方声明 JSON/JSONC 双支持，
    // 但用户的 jq / 脚本按严格 JSON 读。去掉注释后必须仍能被严格解析。
    let on_disk = std::fs::read_to_string(&bed.config).unwrap();
    let without_comments: String = on_disk
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::from_str::<serde_json::Value>(&without_comments)
        .unwrap_or_else(|e| panic!("移除后必须仍是合法严格 JSON：{e}\n{on_disk}"));
}

/// 模型表整体重写：编辑时删掉的模型必须真的消失，不能留旧条目。
#[test]
fn editing_models_drops_the_ones_no_longer_declared() {
    let bed = bed();
    let eng = engine(&bed.home);
    let headers = BTreeMap::new();

    let first = vec!["m-old".to_owned(), "m-keep".to_owned()];
    apply(
        &eng,
        &bed.config,
        &provider_entry_patch(&entry("kimi", &first, &headers)),
        "接入两个模型",
    );
    let second = vec!["m-keep".to_owned()];
    apply(
        &eng,
        &bed.config,
        &provider_entry_patch(&entry("kimi", &second, &headers)),
        "编辑成一个模型",
    );

    let doc = live(&bed.config);
    assert!(
        doc.get(&["provider", "kimi", "models", "m-old"]).is_none(),
        "取消声明的模型必须消失，否则用户在 TUI 里还能选到一个已删的模型"
    );
    assert!(doc.get(&["provider", "kimi", "models", "m-keep"]).is_some());
}

/// 自定义请求头从「有」改成「无」必须真的清掉。
#[test]
fn clearing_headers_removes_the_key() {
    let bed = bed();
    let eng = engine(&bed.home);
    let models = vec!["m".to_owned()];

    let mut headers = BTreeMap::new();
    headers.insert("X-Team".to_owned(), "a".to_owned());
    apply(
        &eng,
        &bed.config,
        &provider_entry_patch(&entry("kimi", &models, &headers)),
        "带头接入",
    );
    assert_eq!(
        live(&bed.config)
            .get(&["provider", "kimi", "options", "headers", "X-Team"])
            .unwrap(),
        serde_json::json!("a")
    );

    let empty = BTreeMap::new();
    apply(
        &eng,
        &bed.config,
        &provider_entry_patch(&entry("kimi", &models, &empty)),
        "清空头",
    );
    assert!(
        live(&bed.config)
            .get(&["provider", "kimi", "options", "headers"])
            .is_none(),
        "清空后不得残留旧头"
    );
}

/// SDK 包名切换（openai 兼容 ↔ anthropic）必须真的改写。
#[test]
fn npm_package_switch_overwrites_previous_value() {
    let bed = bed();
    let eng = engine(&bed.home);
    let models = vec!["m".to_owned()];
    let headers = BTreeMap::new();

    let mut anthropic = entry("relay", &models, &headers);
    anthropic.npm = ANTHROPIC_NPM;
    apply(
        &eng,
        &bed.config,
        &provider_entry_patch(&anthropic),
        "anthropic 形态",
    );
    apply(
        &eng,
        &bed.config,
        &provider_entry_patch(&entry("relay", &models, &headers)),
        "改回 openai 兼容",
    );
    assert_eq!(
        live(&bed.config)
            .get(&["provider", "relay", "npm"])
            .unwrap(),
        serde_json::json!(DEFAULT_NPM),
        "旧的 anthropic 包名不得残留"
    );
}

/// 写入失败后配置一字未变。
#[test]
fn write_failure_leaves_config_byte_identical() {
    let bed = bed();
    let failing = ConfigEngine::new(
        FsSnapshotStore::new(&bed.home),
        JsonlHistoryStore::new(&bed.home),
        FailingWriteFs { inner: RealFs },
    );
    let models = vec!["m".to_owned()];
    let headers = BTreeMap::new();

    let loaded = failing.load(&bed.config, FORMAT).unwrap();
    let result = failing.apply(
        &loaded,
        &provider_entry_patch(&entry("kimi", &models, &headers)),
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
    let headers = BTreeMap::new();

    let loaded = eng.load(&bed.config, FORMAT).unwrap();
    let edited = USER_CONFIG.replace("sk-mine", "sk-mine-rotated");
    std::fs::write(&bed.config, &edited).unwrap();

    let result = eng.apply(
        &loaded,
        &provider_entry_patch(&entry("kimi", &models, &headers)),
        "接入",
        None,
        None,
    );
    assert!(matches!(result, Err(ConfigError::Conflict { .. })));
    assert_eq!(
        std::fs::read_to_string(&bed.config).unwrap(),
        edited,
        "用户刚轮转的 Key 不得被覆盖回旧值"
    );
}

/// 回滚恢复接入前原文。
#[test]
fn rollback_restores_original_text() {
    let bed = bed();
    let eng = engine(&bed.home);
    let models = vec!["m".to_owned()];
    let headers = BTreeMap::new();

    let loaded = eng.load(&bed.config, FORMAT).unwrap();
    let outcome = eng
        .apply(
            &loaded,
            &provider_entry_patch(&entry("kimi", &models, &headers)),
            "接入",
            None,
            None,
        )
        .unwrap();
    eng.rollback(outcome.record.snapshot.as_ref().unwrap(), FORMAT)
        .unwrap();
    assert_eq!(std::fs::read_to_string(&bed.config).unwrap(), USER_CONFIG);
}

/// 「已安装但从未运行」：配置文件与父目录都不存在时首次写入。
#[test]
fn first_write_creates_config_for_a_never_launched_cli() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join(".config/opencode/opencode.json");
    let eng = engine(&dir.path().join("mcpane-home"));
    let models = vec!["m".to_owned()];
    let headers = BTreeMap::new();

    assert!(eng.load(&config, FORMAT).unwrap().missing);
    apply(
        &eng,
        &config,
        &provider_entry_patch(&entry("kimi", &models, &headers)),
        "首次写入",
    );
    assert_eq!(
        live(&config)
            .get(&["provider", "kimi", "options", "apiKey"])
            .unwrap(),
        serde_json::json!("sk-kimi-1234")
    );
}

/// `provider_ids` 反映 live 配置（判断"这条启用了没有"）。带注释的配置上
/// 尤其要对：用严格 JSON 解析器读它会静默返回空表，把已启用的全显示成未启用。
#[test]
fn provider_ids_are_correct_on_a_commented_config() {
    let bed = bed();
    let eng = engine(&bed.home);
    let models = vec!["m".to_owned()];
    let headers = BTreeMap::new();

    assert_eq!(
        provider_ids(live(&bed.config).get(&[PROVIDER_TABLE_KEY]).as_ref()),
        vec!["mine".to_owned()],
        "带 // 注释的配置也必须读得出已有供应商"
    );
    apply(
        &eng,
        &bed.config,
        &provider_entry_patch(&entry("kimi", &models, &headers)),
        "接入",
    );
    let mut ids = provider_ids(live(&bed.config).get(&[PROVIDER_TABLE_KEY]).as_ref());
    ids.sort();
    assert_eq!(ids, vec!["kimi".to_owned(), "mine".to_owned()]);
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
