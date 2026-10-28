//! Grok Adapter × 配置安全引擎端到端测试。
//!
//! 覆盖 AGENTS.md 规则 10：配置写入必须带失败恢复测试。
//! Grok 是**切换式**客户端（不是 additive）：`~/.grok/config.toml` 里
//! `[models].default` 是唯一生效指针，切走时既要清我们那张表，又不能弄丢
//! 用户接管前自己选的默认模型。Key 明文落在同一个文件里，快照必须脱敏。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use mcpane_adapter_grok::{
    DEFAULT_API_BACKEND, DEFAULT_CONTEXT_WINDOW, DefaultPointerDisposition, GrokModelTable,
    MODEL_TABLE_KEY, model_table_patch, remove_model_table_patch, table_slug,
};
use mcpane_config::history::JsonlHistoryStore;
use mcpane_config::snapshot::FsSnapshotStore;
use mcpane_config::{
    ConfigDocument, ConfigEngine, ConfigError, ConfigFormat, FsOps, PatchPlan, RealFs,
};

type Engine = ConfigEngine<FsSnapshotStore, JsonlHistoryStore, RealFs>;
const FORMAT: ConfigFormat = ConfigFormat::Toml;

/// 用户手写的 config.toml：带注释、自建模型表、以及我们不该碰的顶层设置。
const USER_CONFIG: &str = r#"# 我的 Grok 配置，别乱改
[settings]
theme = "dark"   # 行内注释

[models]
default = "my-own"

[model.my-own]
model = "grok-4"
base_url = "https://api.x.ai/v1"
api_key = "sk-my-own"
"#;

struct Bed {
    _dir: tempfile::TempDir,
    home: PathBuf,
    config: PathBuf,
}

fn bed() -> Bed {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join(".grok/config.toml");
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

fn table<'a>(
    slug: &'a str,
    api_key: Option<&'a str>,
    env_key: Option<&'a str>,
) -> GrokModelTable<'a> {
    GrokModelTable {
        slug,
        name: "My Relay",
        model: "grok-4.5",
        base_url: "https://relay.example.com/v1",
        api_key,
        env_key,
        api_backend: DEFAULT_API_BACKEND,
        context_window: DEFAULT_CONTEXT_WINDOW,
    }
}

/// 切中转站 → 切回官方的完整往返：指针还原成用户接管前的原值，
/// 用户自建的表与注释一路都在。
#[test]
fn relay_switch_and_revert_restores_the_users_default_pointer() {
    let bed = bed();
    let eng = engine(&bed.home);
    let slug = table_slug("My Relay");

    apply(
        &eng,
        &bed.config,
        &model_table_patch(&table(&slug, Some("sk-relay"), None)),
        "切中转站",
    );

    let doc = live(&bed.config);
    assert_eq!(doc.get(&["models", "default"]).unwrap(), slug.as_str());
    assert_eq!(
        doc.get(&[MODEL_TABLE_KEY, &slug, "base_url"]).unwrap(),
        "https://relay.example.com/v1"
    );
    // Grok CLI 要求 context_window 是正整数，序列化成字符串它会判整份配置非法。
    assert!(
        doc.get(&[MODEL_TABLE_KEY, &slug, "context_window"])
            .unwrap()
            .is_i64()
    );
    // TOML 注释与用户自建表落盘后仍在。
    let on_disk = std::fs::read_to_string(&bed.config).unwrap();
    assert!(
        on_disk.contains("# 我的 Grok 配置，别乱改"),
        "顶部注释必须保留"
    );
    assert!(on_disk.contains("# 行内注释"), "行内注释必须保留");
    assert_eq!(doc.get(&["settings", "theme"]).unwrap(), "dark");

    // 切回官方：找回了接管前的原值 "my-own"，指针必须还原而不是清空。
    apply(
        &eng,
        &bed.config,
        &remove_model_table_patch(&slug, DefaultPointerDisposition::Restore("my-own")),
        "切回官方",
    );

    let doc = live(&bed.config);
    assert!(
        doc.get(&[MODEL_TABLE_KEY, &slug]).is_none(),
        "我们的表清干净"
    );
    assert_eq!(
        doc.get(&["models", "default"]).unwrap(),
        "my-own",
        "必须还原成接管前用户自己的默认模型，清空等于弄丢用户的选择"
    );
    assert_eq!(
        doc.get(&[MODEL_TABLE_KEY, "my-own", "api_key"]).unwrap(),
        "sk-my-own",
        "用户自建表纹丝不动"
    );
    assert!(
        std::fs::read_to_string(&bed.config)
            .unwrap()
            .contains("# 我的 Grok 配置，别乱改"),
        "切回后注释仍要在"
    );
}

/// 凭据形态互斥：切成 env_key 形态后，上一次写的明文 api_key 必须消失。
/// 残留失效的旧形态会让 Grok CLI 拿错凭据。
#[test]
fn credential_shape_switch_clears_the_previous_form_on_disk() {
    let bed = bed();
    let eng = engine(&bed.home);
    let slug = table_slug("My Relay");

    apply(
        &eng,
        &bed.config,
        &model_table_patch(&table(&slug, Some("sk-inline"), None)),
        "明文 Key",
    );
    apply(
        &eng,
        &bed.config,
        &model_table_patch(&table(&slug, None, Some("XAI_API_KEY"))),
        "改走环境变量",
    );

    let doc = live(&bed.config);
    assert!(
        doc.get(&[MODEL_TABLE_KEY, &slug, "api_key"]).is_none(),
        "旧的明文 Key 必须被清掉"
    );
    assert_eq!(
        doc.get(&[MODEL_TABLE_KEY, &slug, "env_key"]).unwrap(),
        "XAI_API_KEY"
    );
    assert!(
        !std::fs::read_to_string(&bed.config)
            .unwrap()
            .contains("sk-inline"),
        "被替换掉的明文 Key 不得残留在文件里"
    );
}

/// 指针本就不是我们写的（用户在 MCPane 之外自己改过）：只删表，指针不碰。
#[test]
fn a_pointer_that_was_never_ours_is_left_untouched() {
    let bed = bed();
    let eng = engine(&bed.home);
    let slug = table_slug("My Relay");

    // 先写我们的表（指针被指过来），再模拟用户手工把指针改回自己的表。
    apply(
        &eng,
        &bed.config,
        &model_table_patch(&table(&slug, Some("sk"), None)),
        "切中转站",
    );
    let loaded = eng.load(&bed.config, FORMAT).unwrap();
    let restored = loaded
        .content
        .replace(&format!("default = \"{slug}\""), "default = \"my-own\"");
    std::fs::write(&bed.config, &restored).unwrap();

    apply(
        &eng,
        &bed.config,
        &remove_model_table_patch(&slug, DefaultPointerDisposition::Untouched),
        "只删表",
    );

    let doc = live(&bed.config);
    assert!(doc.get(&[MODEL_TABLE_KEY, &slug]).is_none());
    assert_eq!(
        doc.get(&["models", "default"]).unwrap(),
        "my-own",
        "指针不是我们写的就不该被碰"
    );
}

/// 找不到接管前原值时的兜底：清空指针，但用户自建的表本身不得被删。
#[test]
fn clearing_the_pointer_as_a_fallback_keeps_the_users_tables() {
    let bed = bed();
    let eng = engine(&bed.home);
    let slug = table_slug("My Relay");

    apply(
        &eng,
        &bed.config,
        &model_table_patch(&table(&slug, Some("sk"), None)),
        "切中转站",
    );
    apply(
        &eng,
        &bed.config,
        &remove_model_table_patch(&slug, DefaultPointerDisposition::Clear),
        "切回官方（原值已丢）",
    );

    let doc = live(&bed.config);
    assert!(doc.get(&["models", "default"]).is_none(), "兜底是清空指针");
    assert_eq!(
        doc.get(&[MODEL_TABLE_KEY, "my-own", "model"]).unwrap(),
        "grok-4",
        "指针清空不等于删表——用户自建的表必须留着"
    );
}

/// 表名由供应商名派生（`mcpane-<sanitized>`），且只清带前缀的那张。
///
/// 派生是**多对一**的：`My Relay` / `my-relay` / `My/Relay` 都归一到
/// `mcpane-my-relay`。这条测试把该事实钉住，免得日后有人以为表名与供应商
/// 一一对应。
///
/// 撞名**不会**造成凭据串号，这一点经实测确认，不要凭直觉推翻：Grok 是切换式
/// 客户端，`[model.<slug>]` 是「当前生效中转站」的单一槽位，切过去就该由新的
/// 那家覆写；而 `backfill` 只在 `[models].default` 正指向自己算出的 slug 时才
/// 读表，读到的必然是它自己上一次写进去的值。服务层的回归锁见
/// `mcpane-provider` 的 `grok_colliding_display_names_do_not_cross_contaminate`。
#[test]
fn table_slug_is_derived_and_collisions_are_a_known_many_to_one_mapping() {
    assert_eq!(table_slug("My Relay"), "mcpane-my-relay");
    assert_eq!(table_slug("智谱 GLM"), "mcpane-glm");
    assert_eq!(table_slug("***"), "mcpane-relay");
    // 多对一：三个不同的展示名派生出同一张表。
    assert_eq!(table_slug("My Relay"), table_slug("my-relay"));
    assert_eq!(table_slug("My Relay"), table_slug("My/Relay"));

    // 前缀是清理边界：不带 mcpane- 前缀的表一律不碰。
    let bed = bed();
    let eng = engine(&bed.home);
    let slug = table_slug("My Relay");
    apply(
        &eng,
        &bed.config,
        &model_table_patch(&table(&slug, Some("sk"), None)),
        "切中转站",
    );
    apply(
        &eng,
        &bed.config,
        &remove_model_table_patch(&slug, DefaultPointerDisposition::Restore("my-own")),
        "切回官方",
    );
    let doc = live(&bed.config);
    assert!(doc.get(&[MODEL_TABLE_KEY, "my-own"]).is_some());
    assert!(doc.get(&[MODEL_TABLE_KEY, &slug]).is_none());
}

/// 快照脱敏：Key 明文落在 config.toml 里，写前快照不能把它原样留在历史中。
#[test]
fn snapshot_redaction_keeps_plaintext_keys_out_of_history() {
    let bed = bed();
    let eng = engine(&bed.home);
    let slug = table_slug("My Relay");
    let old_key = "sk-my-own";

    let loaded = eng.load(&bed.config, FORMAT).unwrap();
    let redact = |content: &str| content.replace(old_key, "***");
    let outcome = eng
        .apply(
            &loaded,
            &model_table_patch(&table(&slug, Some("sk-relay-new"), None)),
            "切中转站",
            None,
            Some(&redact),
        )
        .unwrap();

    let snapshot = outcome.record.snapshot.as_ref().unwrap();
    let stored = std::fs::read_to_string(&snapshot.stored_at).unwrap();
    assert!(
        !stored.contains(old_key),
        "写前快照里不得留下可读的明文 Key：{stored}"
    );
    assert!(stored.contains("***"), "脱敏后应有占位符");
    // 但配置本身该写的还是写了。
    assert_eq!(
        live(&bed.config)
            .get(&[MODEL_TABLE_KEY, &slug, "api_key"])
            .unwrap(),
        "sk-relay-new"
    );
}

/// 写入失败后配置一字未变（含注释与用户自建表）。
#[test]
fn write_failure_leaves_config_byte_identical() {
    let bed = bed();
    let failing = ConfigEngine::new(
        FsSnapshotStore::new(&bed.home),
        JsonlHistoryStore::new(&bed.home),
        FailingWriteFs { inner: RealFs },
    );
    let slug = table_slug("My Relay");

    let loaded = failing.load(&bed.config, FORMAT).unwrap();
    let result = failing.apply(
        &loaded,
        &model_table_patch(&table(&slug, Some("sk"), None)),
        "注入失败",
        None,
        None,
    );
    assert!(result.is_err());
    assert_eq!(std::fs::read_to_string(&bed.config).unwrap(), USER_CONFIG);
}

/// 外部改动必须被拒——尤其不能把用户刚轮转的 Key 覆盖回旧值。
#[test]
fn external_edit_is_rejected() {
    let bed = bed();
    let eng = engine(&bed.home);
    let slug = table_slug("My Relay");

    let loaded = eng.load(&bed.config, FORMAT).unwrap();
    let edited = USER_CONFIG.replace("sk-my-own", "sk-my-own-rotated");
    std::fs::write(&bed.config, &edited).unwrap();

    let result = eng.apply(
        &loaded,
        &model_table_patch(&table(&slug, Some("sk"), None)),
        "切中转站",
        None,
        None,
    );
    assert!(matches!(result, Err(ConfigError::Conflict { .. })));
    assert_eq!(std::fs::read_to_string(&bed.config).unwrap(), edited);
}

/// 手改坏的 TOML 必须报解析错误且不碰磁盘。
#[test]
fn corrupt_toml_fails_to_parse_and_never_touches_disk() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    let broken = "[models\ndefault = \"x\"\n";
    std::fs::write(&config, broken).unwrap();
    let eng = engine(&dir.path().join("mcpane-home"));

    let loaded = eng.load(&config, FORMAT).unwrap();
    let result = eng.apply(
        &loaded,
        &model_table_patch(&table("mcpane-relay", Some("sk"), None)),
        "切中转站",
        None,
        None,
    );
    assert!(matches!(result, Err(ConfigError::Parse { .. })));
    assert_eq!(std::fs::read_to_string(&config).unwrap(), broken);
}

/// 回滚恢复接入前原文。
#[test]
fn rollback_restores_original_text() {
    let bed = bed();
    let eng = engine(&bed.home);
    let slug = table_slug("My Relay");

    let loaded = eng.load(&bed.config, FORMAT).unwrap();
    let outcome = eng
        .apply(
            &loaded,
            &model_table_patch(&table(&slug, Some("sk"), None)),
            "切中转站",
            None,
            None,
        )
        .unwrap();
    eng.rollback(outcome.record.snapshot.as_ref().unwrap(), FORMAT)
        .unwrap();
    assert_eq!(std::fs::read_to_string(&bed.config).unwrap(), USER_CONFIG);
}

/// 「已安装但从未运行」：`~/.grok` 尚不存在时首次写入。
#[test]
fn first_write_creates_config_for_a_never_launched_cli() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join(".grok/config.toml");
    let eng = engine(&dir.path().join("mcpane-home"));
    let slug = table_slug("My Relay");

    assert!(eng.load(&config, FORMAT).unwrap().missing);
    apply(
        &eng,
        &config,
        &model_table_patch(&table(&slug, Some("sk-first"), None)),
        "首次写入",
    );
    let doc = live(&config);
    assert_eq!(doc.get(&["models", "default"]).unwrap(), slug.as_str());
    assert_eq!(
        doc.get(&[MODEL_TABLE_KEY, &slug, "api_key"]).unwrap(),
        "sk-first"
    );
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
