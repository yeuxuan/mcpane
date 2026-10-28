//! Gemini Adapter × 配置安全引擎端到端测试。
//!
//! 覆盖 AGENTS.md 规则 10：配置写入必须带失败恢复测试。
//! Gemini 的特殊之处是**两个文件**必须一起对：`~/.gemini/.env`（凭据，dotenv）
//! 与 `~/.gemini/settings.json`（`security.auth.selectedType`）。只写其一等于
//! 没切——.env 有 key 但 selectedType 还是 oauth 时 CLI 根本不读那个 key。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use mcpane_adapter_gemini::{
    AUTH_TYPE_API_KEY, AUTH_TYPE_OAUTH, AUTH_TYPE_PATH, PROVIDER_ENV_KEYS, auth_type_patch,
    provider_env_patch, remove_provider_env_patch,
};
use mcpane_config::history::JsonlHistoryStore;
use mcpane_config::snapshot::FsSnapshotStore;
use mcpane_config::{
    ConfigDocument, ConfigEngine, ConfigError, ConfigFormat, FsOps, PatchPlan, RealFs,
};

type Engine = ConfigEngine<FsSnapshotStore, JsonlHistoryStore, RealFs>;

/// 用户手写的 `.env`：带注释、带用户自己的键、键序有意义。
const USER_ENV: &str = "# Gemini CLI 环境变量\nGOOGLE_CLOUD_PROJECT=my-proj\n\n# 代理\nHTTPS_PROXY=http://127.0.0.1:7890\n";

/// 用户手写的 `settings.json`：带 mcpServers 等我们不该碰的配置。
const USER_SETTINGS: &str = r#"{
  "theme": "GitHub",
  "mcpServers": {
    "filesystem": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem"] }
  },
  "security": { "auth": { "selectedType": "oauth-personal" } }
}
"#;

struct Bed {
    _dir: tempfile::TempDir,
    home: PathBuf,
    env_file: PathBuf,
    settings: PathBuf,
}

fn bed() -> Bed {
    let dir = tempfile::tempdir().unwrap();
    let gemini = dir.path().join(".gemini");
    std::fs::create_dir_all(&gemini).unwrap();
    let env_file = gemini.join(".env");
    let settings = gemini.join("settings.json");
    std::fs::write(&env_file, USER_ENV).unwrap();
    std::fs::write(&settings, USER_SETTINGS).unwrap();
    Bed {
        home: dir.path().join("mcpane-home"),
        env_file,
        settings,
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

fn read_env(path: &Path) -> ConfigDocument {
    ConfigDocument::parse(&std::fs::read_to_string(path).unwrap(), ConfigFormat::Env).unwrap()
}

fn apply(engine: &Engine, path: &Path, format: ConfigFormat, plan: &PatchPlan, why: &str) {
    let loaded = engine.load(path, format).unwrap();
    engine.apply(&loaded, plan, why, None, None).unwrap();
}

/// 切中转站 → 切回官方的完整往返：两个文件都要对，用户自有内容一字不动。
#[test]
fn relay_switch_and_revert_touches_only_managed_keys() {
    let bed = bed();
    let eng = engine(&bed.home);

    apply(
        &eng,
        &bed.env_file,
        ConfigFormat::Env,
        &provider_env_patch(&[
            ("GOOGLE_GEMINI_BASE_URL", "https://relay.example.com/v1"),
            ("GEMINI_API_KEY", "sk-relay-1234"),
            ("GEMINI_MODEL", "gemini-3-pro"),
        ]),
        "切中转站（.env）",
    );
    apply(
        &eng,
        &bed.settings,
        ConfigFormat::Json,
        &auth_type_patch(AUTH_TYPE_API_KEY),
        "切中转站（auth）",
    );

    let env_doc = read_env(&bed.env_file);
    assert_eq!(
        env_doc.get(&["GEMINI_API_KEY"]).unwrap(),
        serde_json::json!("sk-relay-1234")
    );
    // 用户自有键与注释一字不动。
    let on_disk = std::fs::read_to_string(&bed.env_file).unwrap();
    assert!(on_disk.contains("# Gemini CLI 环境变量"), "注释必须保留");
    assert!(on_disk.contains("# 代理"));
    assert!(on_disk.contains("GOOGLE_CLOUD_PROJECT=my-proj"));
    assert!(on_disk.contains("HTTPS_PROXY=http://127.0.0.1:7890"));

    // settings.json：只有 selectedType 变了，其余语义全等。
    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&bed.settings).unwrap()).unwrap();
    let mut original: serde_json::Value = serde_json::from_str(USER_SETTINGS).unwrap();
    assert_eq!(after["security"]["auth"]["selectedType"], AUTH_TYPE_API_KEY);
    original["security"]["auth"]["selectedType"] = serde_json::json!(AUTH_TYPE_API_KEY);
    assert_eq!(after, original, "除 selectedType 外必须与原文件语义全等");

    // 切回官方：清掉受管 env 键，auth 设回 oauth。
    apply(
        &eng,
        &bed.env_file,
        ConfigFormat::Env,
        &remove_provider_env_patch(&PROVIDER_ENV_KEYS),
        "切回官方（.env）",
    );
    apply(
        &eng,
        &bed.settings,
        ConfigFormat::Json,
        &auth_type_patch(AUTH_TYPE_OAUTH),
        "切回官方（auth）",
    );

    assert_eq!(
        std::fs::read_to_string(&bed.env_file).unwrap(),
        USER_ENV,
        "切回官方后 .env 必须逐字回到原文（含注释与键序）"
    );
    let reverted: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&bed.settings).unwrap()).unwrap();
    assert_eq!(
        reverted,
        serde_json::from_str::<serde_json::Value>(USER_SETTINGS).unwrap(),
        "切回官方后 settings.json 必须回到原始语义"
    );
}

/// 中转站 Key 里的特殊字符必须无损落盘并读回。
///
/// dotenv 的三种形态里只有单引号是无损容器：双引号形态会展开 `\n`/`\r`
/// 且不认 `\"` 转义（消费方读到的是截断值）。这条锁住的是「写进去什么，
/// Gemini CLI 读出来就是什么」。
#[test]
fn secrets_with_special_characters_survive_the_round_trip() {
    let bed = bed();
    let eng = engine(&bed.home);
    // 空格、井号、双引号、反斜杠——每一个都能让朴素的 dotenv 写法出错。
    let secret = r#"sk-a"b#c d\ne"#;

    apply(
        &eng,
        &bed.env_file,
        ConfigFormat::Env,
        &provider_env_patch(&[("GEMINI_API_KEY", secret)]),
        "写入含特殊字符的 Key",
    );

    let on_disk = std::fs::read_to_string(&bed.env_file).unwrap();
    assert_eq!(
        read_env(&bed.env_file).get(&["GEMINI_API_KEY"]).unwrap(),
        serde_json::json!(secret),
        "Key 必须无损读回。磁盘原文：{on_disk:?}"
    );
    assert!(
        on_disk.lines().count() == USER_ENV.lines().count() + 1,
        "值里的 \\n 字面量不得把一行撑成两行：{on_disk:?}"
    );
}

/// 写入失败（模拟磁盘满）后，两个配置文件都必须一字未变。
#[test]
fn write_failure_leaves_both_config_files_intact() {
    let bed = bed();
    let failing = ConfigEngine::new(
        FsSnapshotStore::new(&bed.home),
        JsonlHistoryStore::new(&bed.home),
        FailingWriteFs { inner: RealFs },
    );

    for (path, format, plan) in [
        (
            &bed.env_file,
            ConfigFormat::Env,
            provider_env_patch(&[("GEMINI_API_KEY", "sk-should-not-land")]),
        ),
        (
            &bed.settings,
            ConfigFormat::Json,
            auth_type_patch(AUTH_TYPE_API_KEY),
        ),
    ] {
        let loaded = failing.load(path, format).unwrap();
        let result = failing.apply(&loaded, &plan, "注入失败", None, None);
        assert!(result.is_err(), "写入应当失败");
    }

    assert_eq!(
        std::fs::read_to_string(&bed.env_file).unwrap(),
        USER_ENV,
        "写入失败后 .env 必须完好"
    );
    assert_eq!(
        std::fs::read_to_string(&bed.settings).unwrap(),
        USER_SETTINGS,
        "写入失败后 settings.json 必须完好"
    );
}

/// 只写了 .env、settings.json 却因外部改动写失败时，不能留下「有 key 但
/// auth 仍是 oauth」的半切状态而无人知晓——引擎必须把冲突报上来。
#[test]
fn external_edit_between_load_and_apply_is_rejected_not_overwritten() {
    let bed = bed();
    let eng = engine(&bed.home);

    let loaded = eng.load(&bed.settings, ConfigFormat::Json).unwrap();
    // 用户在此期间自己改了 settings.json。
    let edited = USER_SETTINGS.replace(r#""theme": "GitHub""#, r#""theme": "Dracula""#);
    std::fs::write(&bed.settings, &edited).unwrap();

    let result = eng.apply(
        &loaded,
        &auth_type_patch(AUTH_TYPE_API_KEY),
        "切中转站",
        None,
        None,
    );
    assert!(matches!(result, Err(ConfigError::Conflict { .. })));
    assert_eq!(
        std::fs::read_to_string(&bed.settings).unwrap(),
        edited,
        "用户的外部改动必须原样保留，不得被静默覆盖"
    );
}

/// 回滚快照回到接入前状态（两个文件各自独立回滚）。
#[test]
fn rollback_restores_pre_switch_state_for_both_files() {
    let bed = bed();
    let eng = engine(&bed.home);

    let env_loaded = eng.load(&bed.env_file, ConfigFormat::Env).unwrap();
    let env_outcome = eng
        .apply(
            &env_loaded,
            &provider_env_patch(&[("GEMINI_API_KEY", "sk-x")]),
            "切中转站",
            None,
            None,
        )
        .unwrap();
    let settings_loaded = eng.load(&bed.settings, ConfigFormat::Json).unwrap();
    let settings_outcome = eng
        .apply(
            &settings_loaded,
            &auth_type_patch(AUTH_TYPE_API_KEY),
            "切中转站",
            None,
            None,
        )
        .unwrap();

    eng.rollback(
        env_outcome.record.snapshot.as_ref().unwrap(),
        ConfigFormat::Env,
    )
    .unwrap();
    eng.rollback(
        settings_outcome.record.snapshot.as_ref().unwrap(),
        ConfigFormat::Json,
    )
    .unwrap();

    assert_eq!(std::fs::read_to_string(&bed.env_file).unwrap(), USER_ENV);
    assert_eq!(
        std::fs::read_to_string(&bed.settings).unwrap(),
        USER_SETTINGS
    );
}

/// 「已安装但从未运行」：`~/.gemini` 下两个文件都不存在，首次写入要能自建。
#[test]
fn first_write_creates_files_for_a_never_launched_cli() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("mcpane-home");
    let env_file = dir.path().join(".gemini/.env");
    let settings = dir.path().join(".gemini/settings.json");
    let eng = engine(&home);

    assert!(eng.load(&env_file, ConfigFormat::Env).unwrap().missing);
    apply(
        &eng,
        &env_file,
        ConfigFormat::Env,
        &provider_env_patch(&[("GEMINI_API_KEY", "sk-first")]),
        "首次写入",
    );
    apply(
        &eng,
        &settings,
        ConfigFormat::Json,
        &auth_type_patch(AUTH_TYPE_API_KEY),
        "首次写入",
    );

    assert_eq!(
        read_env(&env_file).get(&["GEMINI_API_KEY"]).unwrap(),
        serde_json::json!("sk-first")
    );
    let doc = ConfigDocument::parse(
        &std::fs::read_to_string(&settings).unwrap(),
        ConfigFormat::Json,
    )
    .unwrap();
    assert_eq!(
        doc.get(&AUTH_TYPE_PATH).unwrap(),
        serde_json::json!(AUTH_TYPE_API_KEY)
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
