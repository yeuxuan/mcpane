//! OpenClaw Adapter × 配置安全引擎端到端测试。
//!
//! 覆盖 AGENTS.md 规则 10：配置写入必须带失败恢复测试。
//! OpenClaw 的特殊之处是 `~/.openclaw/openclaw.json` **名为 .json 实为 JSON5**
//! （无引号键 + 单引号 + 尾逗号 + 注释）。这条路径最容易在「落盘再读回」时
//! 把用户手写的风格整份规整掉——单测只在内存文档上验证，看不到这层。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mcpane_adapter_openclaw::{
    ANTHROPIC_API, DEFAULT_API, PROVIDERS_PATH, ProviderEntry, default_model_patch, model_ref,
    provider_entry_patch, provider_ids, remove_provider_patch,
};
use mcpane_config::history::JsonlHistoryStore;
use mcpane_config::snapshot::FsSnapshotStore;
use mcpane_config::{
    ConfigDocument, ConfigEngine, ConfigError, ConfigFormat, FsOps, PatchPlan, RealFs,
};

type Engine = ConfigEngine<FsSnapshotStore, JsonlHistoryStore, RealFs>;

/// OpenClaw 自带的初始配置形态：注释、无引号键、单引号、尾逗号一应俱全。
const USER_CONFIG: &str = r#"{
  // 我自己配的，别动
  models: {
    mode: 'merge',
    providers: {
      mine: {
        baseUrl: 'https://mine.example.com/v1',
        apiKey: 'sk-mine',
        models: [{ id: 'my-model' }],
      },
    },
  },
  agents: {
    defaults: {
      model: { primary: 'mine/my-model' },
      models: { 'mine/my-model': { alias: 'my-model' } },
    },
  },
  telemetry: false,
}
"#;

struct Bed {
    _dir: tempfile::TempDir,
    home: PathBuf,
    config: PathBuf,
}

fn bed() -> Bed {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join(".openclaw/openclaw.json");
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
    ConfigDocument::parse(&std::fs::read_to_string(path).unwrap(), ConfigFormat::Json5).unwrap()
}

fn apply(eng: &Engine, path: &Path, plan: &PatchPlan, why: &str) {
    let loaded = eng.load(path, ConfigFormat::Json5).unwrap();
    eng.apply(&loaded, plan, why, None, None).unwrap();
}

fn entry<'a>(
    id: &'a str,
    models: &'a [String],
    headers: &'a BTreeMap<String, String>,
) -> ProviderEntry<'a> {
    ProviderEntry {
        id,
        base_url: "https://api.moonshot.cn/v1",
        api_key: "sk-kimi-1234",
        api: DEFAULT_API,
        models,
        context_window: Some(262_144),
        headers,
    }
}

/// additive 接入：新增一家 provider，用户手写的 JSON5 风格与其他供应商全须全尾。
#[test]
fn adding_a_provider_preserves_json5_style_and_other_providers() {
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
    // JSON5 风格逐项还在——这是「落盘再读回」才能验的，内存文档看不出来。
    assert!(on_disk.contains("// 我自己配的，别动"), "行注释必须保留");
    assert!(
        on_disk.contains("mode: 'merge'"),
        "无引号键 + 单引号必须保留：{on_disk}"
    );

    let doc = live(&bed.config);
    // 我们写的那一条落对了。
    assert_eq!(
        doc.get(&["models", "providers", "kimi", "apiKey"]).unwrap(),
        serde_json::json!("sk-kimi-1234")
    );
    assert_eq!(
        doc.get(&["models", "providers", "kimi", "models"]).unwrap()[0]["contextWindow"],
        262_144
    );
    // 用户自己那一家纹丝不动。
    assert_eq!(
        doc.get(&["models", "providers", "mine", "apiKey"]).unwrap(),
        serde_json::json!("sk-mine")
    );
    // models.mode 是用户的全局取舍，不得被碰。
    assert_eq!(
        doc.get(&["models", "mode"]).unwrap(),
        serde_json::json!("merge")
    );
    assert_eq!(doc.get(&["telemetry"]).unwrap(), serde_json::json!(false));
}

/// additive 移除：只清我们那一家，用户那家与他选的默认模型都不动。
#[test]
fn removing_our_provider_leaves_the_users_provider_and_pointer_alone() {
    let bed = bed();
    let eng = engine(&bed.home);
    let models = vec!["kimi-k2.7-code".to_owned()];
    let headers = BTreeMap::new();

    apply(
        &eng,
        &bed.config,
        &provider_entry_patch(&entry("kimi", &models, &headers)),
        "接入",
    );
    let doc = live(&bed.config);
    let catalog: Vec<String> = doc
        .get(&["agents", "defaults", "models"])
        .and_then(|v| v.as_object().map(|o| o.keys().cloned().collect()))
        .unwrap_or_default();
    let primary = doc
        .get(&["agents", "defaults", "model", "primary"])
        .and_then(|v| v.as_str().map(str::to_owned));

    apply(
        &eng,
        &bed.config,
        &remove_provider_patch("kimi", &models, primary.as_deref(), &catalog),
        "移除",
    );

    let after = live(&bed.config);
    assert!(
        after.get(&["models", "providers", "kimi"]).is_none(),
        "我们那家必须被清干净"
    );
    assert_eq!(
        after
            .get(&["models", "providers", "mine", "apiKey"])
            .unwrap(),
        serde_json::json!("sk-mine"),
        "用户自建供应商不得被牵连"
    );
    assert_eq!(
        after
            .get(&["agents", "defaults", "model", "primary"])
            .unwrap(),
        serde_json::json!("mine/my-model"),
        "用户自己选的默认模型不是我们写的，不得被清"
    );
    assert!(
        std::fs::read_to_string(&bed.config)
            .unwrap()
            .contains("// 我自己配的，别动"),
        "移除后注释仍要在"
    );
}

/// 默认模型指针的三条规则里最容易漏的一条：指向我们这家、但那个模型在
/// 编辑后已不再声明 → 是一条死引用，必须重指到当前首个模型。
#[test]
fn stale_primary_pointing_at_a_dropped_model_is_repointed() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("openclaw.json");
    // 一份干净配置：没有用户自己的 provider，指针未设。
    std::fs::write(&config, "{}\n").unwrap();
    let eng = engine(&dir.path().join("mcpane-home"));
    let headers = BTreeMap::new();

    // 首次接入声明两个模型，指针未设 → 认领首个。
    let first = vec!["m-old".to_owned(), "m-new".to_owned()];
    apply(
        &eng,
        &config,
        &provider_entry_patch(&entry("kimi", &first, &headers)),
        "接入",
    );
    apply(
        &eng,
        &config,
        &default_model_patch("kimi", &first, None, &[]),
        "认领默认模型",
    );
    assert_eq!(
        live(&config)
            .get(&["agents", "defaults", "model", "primary"])
            .unwrap(),
        serde_json::json!(model_ref("kimi", "m-old"))
    );

    // 编辑：把 m-old 删了，只留 m-new。指针成了死引用，必须重指。
    let second = vec!["m-new".to_owned()];
    let doc = live(&config);
    let catalog: Vec<String> = doc
        .get(&["agents", "defaults", "models"])
        .and_then(|v| v.as_object().map(|o| o.keys().cloned().collect()))
        .unwrap_or_default();
    let primary = doc
        .get(&["agents", "defaults", "model", "primary"])
        .and_then(|v| v.as_str().map(str::to_owned));
    apply(
        &eng,
        &config,
        &provider_entry_patch(&entry("kimi", &second, &headers)),
        "编辑",
    );
    apply(
        &eng,
        &config,
        &default_model_patch("kimi", &second, primary.as_deref(), &catalog),
        "重指默认模型",
    );

    let after = live(&config);
    assert_eq!(
        after
            .get(&["agents", "defaults", "model", "primary"])
            .unwrap(),
        serde_json::json!(model_ref("kimi", "m-new")),
        "指向已取消声明的模型是死引用，必须重指到当前首个"
    );
    assert!(
        after
            .get(&["agents", "defaults", "models", &model_ref("kimi", "m-old")])
            .is_none(),
        "取消声明的模型不得在目录里留成幽灵"
    );
}

/// 自定义请求头：从「有头」改成「无头」时必须真的删掉，不能残留上一次的。
#[test]
fn clearing_custom_headers_removes_them_from_disk() {
    let bed = bed();
    let eng = engine(&bed.home);
    let models = vec!["m".to_owned()];

    let mut headers = BTreeMap::new();
    headers.insert("X-Relay-Tag".to_owned(), "team-a".to_owned());
    apply(
        &eng,
        &bed.config,
        &provider_entry_patch(&entry("kimi", &models, &headers)),
        "带头接入",
    );
    assert_eq!(
        live(&bed.config)
            .get(&["models", "providers", "kimi", "headers", "X-Relay-Tag"])
            .unwrap(),
        serde_json::json!("team-a")
    );

    let empty = BTreeMap::new();
    apply(
        &eng,
        &bed.config,
        &provider_entry_patch(&entry("kimi", &models, &empty)),
        "清空自定义头",
    );
    assert!(
        live(&bed.config)
            .get(&["models", "providers", "kimi", "headers"])
            .is_none(),
        "清空后旧的自定义头不得残留"
    );
}

/// 上游协议形态切换：anthropic ↔ openai 必须真的改写，不能留旧值。
#[test]
fn api_shape_switch_overwrites_the_previous_value() {
    let bed = bed();
    let eng = engine(&bed.home);
    let models = vec!["m".to_owned()];
    let headers = BTreeMap::new();

    let mut anthropic = entry("relay", &models, &headers);
    anthropic.api = ANTHROPIC_API;
    apply(
        &eng,
        &bed.config,
        &provider_entry_patch(&anthropic),
        "anthropic 形态",
    );
    assert_eq!(
        live(&bed.config)
            .get(&["models", "providers", "relay", "api"])
            .unwrap(),
        serde_json::json!(ANTHROPIC_API)
    );

    apply(
        &eng,
        &bed.config,
        &provider_entry_patch(&entry("relay", &models, &headers)),
        "改回 openai 形态",
    );
    assert_eq!(
        live(&bed.config)
            .get(&["models", "providers", "relay", "api"])
            .unwrap(),
        serde_json::json!(DEFAULT_API),
        "旧的 anthropic 形态不得残留，否则报文格式与上游不符"
    );
}

/// 写入失败（磁盘满）后原配置一字未变——JSON5 的注释与风格同样不能被动过。
#[test]
fn write_failure_leaves_the_json5_config_byte_identical() {
    let bed = bed();
    let failing = ConfigEngine::new(
        FsSnapshotStore::new(&bed.home),
        JsonlHistoryStore::new(&bed.home),
        FailingWriteFs { inner: RealFs },
    );
    let models = vec!["m".to_owned()];
    let headers = BTreeMap::new();

    let loaded = failing.load(&bed.config, ConfigFormat::Json5).unwrap();
    let result = failing.apply(
        &loaded,
        &provider_entry_patch(&entry("kimi", &models, &headers)),
        "注入失败",
        None,
        None,
    );
    assert!(result.is_err());
    assert_eq!(
        std::fs::read_to_string(&bed.config).unwrap(),
        USER_CONFIG,
        "写入失败后配置必须逐字完好"
    );
}

/// 外部改动必须被拒，不得静默覆盖。
#[test]
fn external_edit_is_rejected() {
    let bed = bed();
    let eng = engine(&bed.home);
    let models = vec!["m".to_owned()];
    let headers = BTreeMap::new();

    let loaded = eng.load(&bed.config, ConfigFormat::Json5).unwrap();
    let edited = USER_CONFIG.replace("telemetry: false", "telemetry: true");
    std::fs::write(&bed.config, &edited).unwrap();

    let result = eng.apply(
        &loaded,
        &provider_entry_patch(&entry("kimi", &models, &headers)),
        "接入",
        None,
        None,
    );
    assert!(matches!(result, Err(ConfigError::Conflict { .. })));
    assert_eq!(std::fs::read_to_string(&bed.config).unwrap(), edited);
}

/// 坏掉的配置（手改出语法错误）必须报解析错误且绝不碰磁盘。
#[test]
fn corrupt_config_fails_to_parse_and_never_touches_disk() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("openclaw.json");
    let broken = "{ models: { providers: { kimi: { baseUrl: 'x' \n";
    std::fs::write(&config, broken).unwrap();
    let eng = engine(&dir.path().join("mcpane-home"));
    let models = vec!["m".to_owned()];
    let headers = BTreeMap::new();

    let loaded = eng.load(&config, ConfigFormat::Json5).unwrap();
    let result = eng.apply(
        &loaded,
        &provider_entry_patch(&entry("kimi", &models, &headers)),
        "接入",
        None,
        None,
    );
    assert!(matches!(result, Err(ConfigError::Parse { .. })));
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        broken,
        "解析失败不得改动磁盘上的坏文件"
    );
}

/// 回归（数据丢失事故）：配置里带块注释时，「移除供应商」必须报错并原样保留
/// 文件，绝不能把整份配置写成 `null`。
///
/// 事故链条有四环：json-five 0.3.1 的词法器把块注释的闭合 `/` 切在 token 之外
/// → 序列化产物无法重解析 → `ConfigDocument` 的克隆静默退化成空 JSON 文档
/// → 纯 `Remove` 计划在空文档上条条幂等成功，序列化出字面量 `null` 并被当作
/// 正常结果原子写入。用户点一下「移除供应商」，整份 `openclaw.json` 就没了，
/// 界面上还报成功。`remove_provider_patch` 恰好就是纯 Remove 计划。
#[test]
fn block_comment_config_is_refused_instead_of_being_destroyed() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("openclaw.json");
    let original = "{\n  /* 我的注释 */\n  models: { providers: { kimi: { apiKey: 'sk' } } },\n}\n";
    std::fs::write(&config, original).unwrap();
    let eng = engine(&dir.path().join("mcpane-home"));
    let models = vec!["m".to_owned()];
    let headers = BTreeMap::new();

    // 纯 Remove 计划（移除供应商）——事故的原始触发路径。
    let loaded = eng.load(&config, ConfigFormat::Json5).unwrap();
    let result = eng.apply(
        &loaded,
        &remove_provider_patch("kimi", &models, None, &[]),
        "移除供应商",
        None,
        None,
    );
    assert!(
        matches!(result, Err(ConfigError::LossySerialize { .. })),
        "往返有损必须报错，实际：{result:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        original,
        "报错路径下配置必须一字未动（此前会被整份写成 null）"
    );

    // 带 Set 的计划同样被拦下，同样不动磁盘。
    let loaded = eng.load(&config, ConfigFormat::Json5).unwrap();
    let result = eng.apply(
        &loaded,
        &provider_entry_patch(&entry("other", &models, &headers)),
        "接入另一家",
        None,
        None,
    );
    assert!(matches!(result, Err(ConfigError::LossySerialize { .. })));
    assert_eq!(std::fs::read_to_string(&config).unwrap(), original);
}

/// 回滚到接入前状态（含 JSON5 风格）。
#[test]
fn rollback_restores_the_original_json5_text() {
    let bed = bed();
    let eng = engine(&bed.home);
    let models = vec!["m".to_owned()];
    let headers = BTreeMap::new();

    let loaded = eng.load(&bed.config, ConfigFormat::Json5).unwrap();
    let outcome = eng
        .apply(
            &loaded,
            &provider_entry_patch(&entry("kimi", &models, &headers)),
            "接入",
            None,
            None,
        )
        .unwrap();
    eng.rollback(
        outcome.record.snapshot.as_ref().unwrap(),
        ConfigFormat::Json5,
    )
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(&bed.config).unwrap(),
        USER_CONFIG,
        "回滚必须恢复接入前的原文"
    );
}

/// `provider_ids` 读的是 live 配置里的全部供应商（含用户自建），供 UI 判重。
#[test]
fn provider_ids_reflect_live_config() {
    let bed = bed();
    let eng = engine(&bed.home);
    let models = vec!["m".to_owned()];
    let headers = BTreeMap::new();

    assert_eq!(
        provider_ids(live(&bed.config).get(&PROVIDERS_PATH).as_ref()),
        vec!["mine".to_owned()]
    );
    apply(
        &eng,
        &bed.config,
        &provider_entry_patch(&entry("kimi", &models, &headers)),
        "接入",
    );
    let mut ids = provider_ids(live(&bed.config).get(&PROVIDERS_PATH).as_ref());
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
