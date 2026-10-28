//! 配置安全引擎集成测试（TASK-200 / GATE-2）：
//! 真实文件系统 + fixtures + 失败注入（AGENTS.md 规则 10：写入必须带失败恢复测试）。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use mcpane_config::history::JsonlHistoryStore;
use mcpane_config::snapshot::FsSnapshotStore;
use mcpane_config::{
    ConfigDocument, ConfigEngine, ConfigError, ConfigFormat, FsOps, PatchOp, PatchPlan, RealFs,
};
use serde_json::json;

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/config-core")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

struct TestBed {
    _dir: tempfile::TempDir,
    root: PathBuf,
    target: PathBuf,
}

fn setup(content: &str, ext: &str) -> TestBed {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("mcpane-home");
    let target = dir.path().join(format!("config.{ext}"));
    std::fs::write(&target, content).unwrap();
    TestBed {
        root,
        target,
        _dir: dir,
    }
}

fn engine(root: &Path) -> ConfigEngine<FsSnapshotStore, JsonlHistoryStore, RealFs> {
    ConfigEngine::new(
        FsSnapshotStore::new(root),
        JsonlHistoryStore::new(root),
        RealFs,
    )
}

// ---------- 凭据落盘权限（ADR-015） ----------

/// 配置里注入了 Hub / 中转 Token，落盘必须 0600；快照留存写前全文（可能含
/// 用户 secret）同样必须 0600。此前引擎裸 `std::fs::write` 会以 umask 默认
/// （0644）世界可读落盘，与 ADR-015「本机其他用户由文件权限解决」相矛盾。
#[cfg(unix)]
#[test]
fn written_config_and_snapshot_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let bed = setup(&fixture("unknown-fields.json"), "json");
    // 故意先把目标放松到 0644，验证写入后被收紧回 0600。
    std::fs::set_permissions(&bed.target, std::fs::Permissions::from_mode(0o644)).unwrap();
    let engine = engine(&bed.root);

    let loaded = engine.load(&bed.target, ConfigFormat::Json).unwrap();
    let plan = PatchPlan::new(vec![PatchOp::set(
        &["mcpServers", "mcpane"],
        json!({ "type": "http", "url": "http://127.0.0.1:24860/mcp" }),
    )]);
    engine
        .apply(&loaded, &plan, "写入 mcpane Hub Entry", None, None)
        .unwrap();

    let target_mode = std::fs::metadata(&bed.target).unwrap().permissions().mode() & 0o777;
    assert_eq!(target_mode, 0o600, "写入的配置文件必须收紧为 0600");

    // 快照目录里的写前全文备份同样必须 0600。
    let snap_dir = bed.root.join("config-snapshots");
    let mut checked = 0usize;
    for entry in std::fs::read_dir(&snap_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) == Some("json")
            && path.to_string_lossy().contains(".meta.")
        {
            continue; // 只查内容快照，元信息不含 secret
        }
        if path.is_file() {
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "快照 {} 必须 0600", path.display());
            checked += 1;
        }
    }
    assert!(checked > 0, "应至少留下一份内容快照");
}

/// 回归：目标是 dotfiles 工具（chezmoi / GNU stow 等）建的符号链接时，
/// 原子写不得把链接替换成普通文件。
///
/// `rename` 换的是目录项本身：不先解析链接，写完 `~/.claude.json` 就从
/// 「指向仓库文件的链接」变成「一份普通文件」——内容不丢，但用户仓库里的
/// 那一份从此再也收不到 MCPane 的写入，且没有任何提示，等发现时两边已分叉。
#[cfg(unix)]
#[test]
fn atomic_write_follows_symlink_instead_of_replacing_it() {
    let bed = setup(&fixture("unknown-fields.json"), "json");
    // 模拟 dotfiles 布局：真实文件在「仓库」里，家目录下只放一个链接。
    let repo_file = bed.target.with_file_name("dotfiles-repo-config.json");
    std::fs::rename(&bed.target, &repo_file).unwrap();
    std::os::unix::fs::symlink(&repo_file, &bed.target).unwrap();

    let engine = engine(&bed.root);
    let loaded = engine.load(&bed.target, ConfigFormat::Json).unwrap();
    let plan = PatchPlan::new(vec![PatchOp::set(
        &["mcpServers", "mcpane"],
        json!({ "type": "http", "url": "http://127.0.0.1:24860/mcp" }),
    )]);
    engine
        .apply(&loaded, &plan, "写入 mcpane Hub Entry", None, None)
        .unwrap();

    assert!(
        std::fs::symlink_metadata(&bed.target)
            .unwrap()
            .file_type()
            .is_symlink(),
        "写入后目标退化成了普通文件：dotfiles 仓库里的那份从此不再被同步"
    );
    // 链接指向的真实文件必须收到这次写入。
    let written = std::fs::read_to_string(&repo_file).unwrap();
    assert!(
        written.contains("24860"),
        "链接保住了，但内容没写进它指向的真实文件"
    );
}

// ---------- 正常流水线 ----------

#[test]
fn full_pipeline_preserves_unknown_json_fields() {
    let bed = setup(&fixture("unknown-fields.json"), "json");
    let engine = engine(&bed.root);

    let loaded = engine.load(&bed.target, ConfigFormat::Json).unwrap();
    let plan = PatchPlan::new(vec![PatchOp::set(
        &["mcpServers", "mcpane"],
        json!({ "type": "http", "url": "http://127.0.0.1:24860/mcp" }),
    )]);
    let outcome = engine
        .apply(&loaded, &plan, "写入 mcpane Hub Entry", None, None)
        .unwrap();
    assert!(outcome.written);
    assert!(!outcome.diff.unified.is_empty());

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&bed.target).unwrap()).unwrap();
    // 未知字段 100% 保留（GATE-2 条件 1）。
    assert_eq!(
        after["unknownTopLevel"]["nested"],
        json!([1, 2, {"deep": true}])
    );
    assert_eq!(after["$schema"], "https://example.com/schema.json");
    assert_eq!(after["numberField"], 42.5);
    assert_eq!(after["mcpServers"]["existing"]["command"], "npx");
    assert_eq!(after["mcpServers"]["mcpane"]["type"], "http");
}

#[test]
fn apply_registers_own_write_hash_for_watcher() {
    use mcpane_config::{content_hash, is_recent_own_write};
    let bed = setup(r#"{"a": 1}"#, "json");
    let eng = engine(&bed.root);
    let loaded = eng.load(&bed.target, ConfigFormat::Json).unwrap();
    let plan = PatchPlan::new(vec![PatchOp::set(&["b"], json!(2))]);
    eng.apply(&loaded, &plan, "写入", None, None).unwrap();

    // 引擎写盘前登记了最终内容 hash：文件监听据此把自写与外部改动区分开，
    // 不依赖操作历史落盘时机（消除 rename 早于历史的竞态）。
    let written = std::fs::read_to_string(&bed.target).unwrap();
    assert!(
        is_recent_own_write(&bed.target, &content_hash(&written)),
        "写入后应能立即认出自写 hash"
    );
    // 别的 hash / 别的路径不误命中。
    assert!(!is_recent_own_write(&bed.target, "deadbeef"));
    assert!(!is_recent_own_write(
        Path::new("/other/file.json"),
        &content_hash(&written)
    ));
}

#[test]
fn full_pipeline_preserves_toml_comments_and_unknown_tables() {
    let content = format!(
        "{}{}",
        fixture("commented.toml"),
        fixture("unknown-tables.toml")
    );
    let bed = setup(&content, "toml");
    let engine = engine(&bed.root);

    let loaded = engine.load(&bed.target, ConfigFormat::Toml).unwrap();
    let plan = PatchPlan::new(vec![
        PatchOp::set(
            &["mcp_servers", "mcpane", "url"],
            json!("http://127.0.0.1:24860/mcp"),
        ),
        PatchOp::set(
            &["mcp_servers", "mcpane", "bearer_token_env_var"],
            json!("MCPANE_CODEX_TOKEN"),
        ),
    ]);
    engine
        .apply(&loaded, &plan, "写入 [mcp_servers.mcpane]", None, None)
        .unwrap();

    let after = std::fs::read_to_string(&bed.target).unwrap();
    assert!(after.contains("# Codex 用户配置（顶部注释必须保留）"));
    assert!(after.contains("model = \"o3\"  # 用户选择的模型"));
    assert!(after.contains("[mcp_servers.context7]  # 已有 server"));
    assert!(after.contains("[totally_unknown_table]"));
    assert!(after.contains("[future_feature.sub]"));
    assert!(after.contains("[mcp_servers.mcpane]"));
}

#[test]
fn corrupt_files_fail_with_parse_error_and_never_touch_disk() {
    for (name, format, ext) in [
        ("corrupt.json", ConfigFormat::Json, "json"),
        ("corrupt.toml", ConfigFormat::Toml, "toml"),
    ] {
        let original = fixture(name);
        let bed = setup(&original, ext);
        let engine = engine(&bed.root);
        let loaded = engine.load(&bed.target, format).unwrap();
        let result = engine.apply(
            &loaded,
            &PatchPlan::new(vec![PatchOp::set(&["a"], json!(1))]),
            "test",
            None,
            None,
        );
        assert!(matches!(result, Err(ConfigError::Parse { .. })));
        assert_eq!(
            std::fs::read_to_string(&bed.target).unwrap(),
            original,
            "损坏文件必须原样保留"
        );
    }
}

#[test]
fn empty_fixtures_are_writable() {
    let bed = setup(&fixture("empty.json"), "json");
    let eng = engine(&bed.root);
    let loaded = eng.load(&bed.target, ConfigFormat::Json).unwrap();
    eng.apply(
        &loaded,
        &PatchPlan::new(vec![PatchOp::set(
            &["mcpServers", "mcpane"],
            json!({"type":"http"}),
        )]),
        "首次写入",
        None,
        None,
    )
    .unwrap();

    let bed = setup(&fixture("empty.toml"), "toml");
    let eng = engine(&bed.root);
    let loaded = eng.load(&bed.target, ConfigFormat::Toml).unwrap();
    eng.apply(
        &loaded,
        &PatchPlan::new(vec![PatchOp::set(
            &["mcp_servers", "mcpane", "url"],
            json!("http://x"),
        )]),
        "首次写入",
        None,
        None,
    )
    .unwrap();
}

#[test]
fn missing_parent_directory_is_created_on_first_write() {
    // 模拟「客户端 CLI 已装但从未运行」：~/.codex 这类父目录尚不存在。
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("mcpane-home");
    let target = dir.path().join("never-created/codex/config.toml");
    assert!(!target.parent().unwrap().exists());
    let eng = engine(&root);
    let loaded = eng.load(&target, ConfigFormat::Toml).unwrap();
    assert!(loaded.missing);
    eng.apply(
        &loaded,
        &PatchPlan::new(vec![PatchOp::set(
            &["model_provider"],
            json!("mcpane-relay"),
        )]),
        "首次写入（父目录不存在）",
        None,
        None,
    )
    .unwrap();
    assert!(
        std::fs::read_to_string(&target)
            .unwrap()
            .contains("mcpane-relay"),
        "父目录应被自动创建并写入成功"
    );
}

#[test]
fn failed_first_write_does_not_fabricate_empty_file() {
    // 原文件不存在 + 写入失败：恢复路径绝不能造出一个空文件
    //（对某些客户端「文件存在但为空」≠「文件不存在」）。
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("mcpane-home");
    let target = dir.path().join("codex/config.toml");
    let engine = ConfigEngine::new(
        FsSnapshotStore::new(&root),
        JsonlHistoryStore::new(&root),
        FailingWriteFs { inner: RealFs },
    );
    let loaded = engine.load(&target, ConfigFormat::Toml).unwrap();
    assert!(loaded.missing);
    let result = engine.apply(
        &loaded,
        &PatchPlan::new(vec![PatchOp::set(&["model_provider"], json!("x"))]),
        "首次写入失败",
        None,
        None,
    );
    assert!(result.is_err());
    assert!(
        !target.exists(),
        "首次写入失败后不得凭空造出空文件，实际残留：{:?}",
        std::fs::read_to_string(&target)
    );
}

/// 回归（上一条的镜像）：目标**存在但为 0 字节**时写入失败，恢复不得把它删掉。
///
/// `recover()` 曾用 `original.is_empty()` 推断「原文件本不存在」，而 0 字节的
/// 已存在文件内容同样为空，一并命中删除分支——一次失败的写入反而让用户的文件
/// 凭空消失。`.claude.json` 在 Claude Code 首次运行前正是 0 字节，这条路径是
/// 真实可达的。判据必须来自 `LoadedConfig::missing`，不能靠内容猜。
#[test]
fn failed_write_must_not_delete_an_existing_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("mcpane-home");
    let target = dir.path().join(".claude.json");
    std::fs::write(&target, "").unwrap();

    let engine = ConfigEngine::new(
        FsSnapshotStore::new(&root),
        JsonlHistoryStore::new(&root),
        FailingWriteFs { inner: RealFs },
    );
    let loaded = engine.load(&target, ConfigFormat::Json).unwrap();
    assert!(!loaded.missing, "文件确实存在，只是内容为空");

    let result = engine.apply(
        &loaded,
        &PatchPlan::new(vec![PatchOp::set(&["mcpServers", "mcpane"], json!({}))]),
        "写入失败",
        None,
        None,
    );
    assert!(result.is_err());
    assert!(
        target.exists(),
        "写入失败的恢复不得删除本来存在的空文件——恢复动作反而造成了数据丢失"
    );
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "",
        "恢复后内容仍应是原来的空"
    );
}

/// 凭据在 dotenv 里的无损往返（Gemini CLI 的 `~/.gemini/.env`）。
///
/// 走完整引擎落到真实磁盘，再按 dotenv 语义读回。旧实现无条件写双引号 +
/// `\"` 转义，含双引号的 Key 会被消费方读成截断值（`"sk-a\"b"` → `sk-a\`）。
#[test]
fn env_secret_with_quotes_round_trips_through_the_real_pipeline() {
    let bed = setup("EXISTING=1\n", "env");
    let engine = engine(&bed.root);
    let secret = r#"sk-a"b#c d"#;

    let loaded = engine.load(&bed.target, ConfigFormat::Env).unwrap();
    engine
        .apply(
            &loaded,
            &PatchPlan::new(vec![PatchOp::set(&["GEMINI_API_KEY"], json!(secret))]),
            "写入中转站 Key",
            None,
            None,
        )
        .unwrap();

    let on_disk = std::fs::read_to_string(&bed.target).unwrap();
    let doc = ConfigDocument::parse(&on_disk, ConfigFormat::Env).unwrap();
    assert_eq!(
        doc.get(&["GEMINI_API_KEY"]).unwrap(),
        json!(secret),
        "写进去什么读回来必须是什么。磁盘原文：{on_disk:?}"
    );
    assert!(on_disk.contains("EXISTING=1"), "用户既有键不受影响");
}

// ---------- 冲突检测（GATE-2 条件 4：外部修改不被静默覆盖） ----------

#[test]
fn external_modification_between_load_and_apply_is_rejected() {
    let bed = setup(r#"{"a": 1}"#, "json");
    let engine = engine(&bed.root);
    let loaded = engine.load(&bed.target, ConfigFormat::Json).unwrap();

    // 模拟外部修改（用户手工编辑）。
    std::fs::write(&bed.target, r#"{"a": 1, "user_added": true}"#).unwrap();

    let result = engine.apply(
        &loaded,
        &PatchPlan::new(vec![PatchOp::set(&["b"], json!(2))]),
        "test",
        None,
        None,
    );
    assert!(matches!(result, Err(ConfigError::Conflict { .. })));
    // 外部修改的内容原样保留。
    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&bed.target).unwrap()).unwrap();
    assert_eq!(after["user_added"], true);
    assert!(after.get("b").is_none());
}

// ---------- 业务验证 ----------

#[test]
fn business_validation_failure_prevents_write() {
    let bed = setup(r#"{"a": 1}"#, "json");
    let engine = engine(&bed.root);
    let loaded = engine.load(&bed.target, ConfigFormat::Json).unwrap();
    let result = engine.apply(
        &loaded,
        &PatchPlan::new(vec![PatchOp::set(&["b"], json!(2))]),
        "test",
        Some(&|_doc: &ConfigDocument| Err("业务规则不通过".to_owned())),
        None,
    );
    assert!(matches!(result, Err(ConfigError::ValidationFailed(_))));
    assert_eq!(std::fs::read_to_string(&bed.target).unwrap(), r#"{"a": 1}"#);
}

// ---------- 失败注入（GATE-2 条件 5：模拟失败后原配置完整） ----------

/// 包装 RealFs：第 N 次 rename 失败（模拟原子替换失败）。
struct FailingRenameFs {
    inner: RealFs,
    fail_on: usize,
    count: AtomicUsize,
}

impl FsOps for FailingRenameFs {
    fn read_to_string(&self, path: &Path) -> std::io::Result<String> {
        self.inner.read_to_string(path)
    }
    fn write(&self, path: &Path, content: &str) -> std::io::Result<()> {
        self.inner.write(path, content)
    }
    fn fsync(&self, path: &Path) -> std::io::Result<()> {
        self.inner.fsync(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        let n = self.count.fetch_add(1, Ordering::SeqCst) + 1;
        if n == self.fail_on {
            return Err(std::io::Error::other("injected rename failure"));
        }
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

/// 磁盘写入失败模拟。
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

#[test]
fn rename_failure_recovers_original_content() {
    let bed = setup(r#"{"precious": "data"}"#, "json");
    let engine = ConfigEngine::new(
        FsSnapshotStore::new(&bed.root),
        JsonlHistoryStore::new(&bed.root),
        FailingRenameFs {
            inner: RealFs,
            fail_on: 1,
            count: AtomicUsize::new(0),
        },
    );
    let loaded = engine.load(&bed.target, ConfigFormat::Json).unwrap();
    let result = engine.apply(
        &loaded,
        &PatchPlan::new(vec![PatchOp::set(&["b"], json!(2))]),
        "test",
        None,
        None,
    );
    assert!(matches!(
        result,
        Err(ConfigError::RecoveredFromFailure { .. })
    ));
    assert_eq!(
        std::fs::read_to_string(&bed.target).unwrap(),
        r#"{"precious": "data"}"#,
        "失败后原配置必须完整"
    );
    // 目录中不允许残留临时文件。
    let leftovers: Vec<_> = std::fs::read_dir(bed.target.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains("mcpane-tmp"))
        .collect();
    assert!(leftovers.is_empty(), "残留临时文件: {leftovers:?}");
}

#[test]
fn disk_write_failure_leaves_original_untouched() {
    let bed = setup(r#"{"precious": "data"}"#, "json");
    let engine = ConfigEngine::new(
        FsSnapshotStore::new(&bed.root),
        JsonlHistoryStore::new(&bed.root),
        FailingWriteFs { inner: RealFs },
    );
    let loaded = engine.load(&bed.target, ConfigFormat::Json).unwrap();
    let result = engine.apply(
        &loaded,
        &PatchPlan::new(vec![PatchOp::set(&["b"], json!(2))]),
        "test",
        None,
        None,
    );
    // 磁盘满：临时文件都写不进去，原文件从未被碰——恢复路径判定"无需恢复"。
    assert!(result.is_err());
    assert_eq!(
        std::fs::read_to_string(&bed.target).unwrap(),
        r#"{"precious": "data"}"#
    );
}

#[cfg(unix)]
#[test]
fn readonly_target_directory_fails_cleanly() {
    use std::os::unix::fs::PermissionsExt;
    let bed = setup(r#"{"a": 1}"#, "json");
    let engine = engine(&bed.root);
    let loaded = engine.load(&bed.target, ConfigFormat::Json).unwrap();

    // 只读目录：临时文件无法创建。
    let dir = bed.target.parent().unwrap();
    let original_perm = std::fs::metadata(dir).unwrap().permissions();
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o500)).unwrap();

    let result = engine.apply(
        &loaded,
        &PatchPlan::new(vec![PatchOp::set(&["b"], json!(2))]),
        "test",
        None,
        None,
    );

    std::fs::set_permissions(dir, original_perm).unwrap();
    assert!(result.is_err());
    assert_eq!(std::fs::read_to_string(&bed.target).unwrap(), r#"{"a": 1}"#);
}

// ---------- 回滚（GATE-2 条件 6：任意历史版本可回滚） ----------

#[test]
fn rollback_restores_any_previous_version_with_pre_rollback_backup() {
    let bed = setup(r#"{"v": 1}"#, "json");
    let engine = engine(&bed.root);

    // 两次演进：v1 → v2 → v3。
    let loaded = engine.load(&bed.target, ConfigFormat::Json).unwrap();
    let out1 = engine
        .apply(
            &loaded,
            &PatchPlan::new(vec![PatchOp::set(&["v"], json!(2))]),
            "v2",
            None,
            None,
        )
        .unwrap();
    let loaded = engine.load(&bed.target, ConfigFormat::Json).unwrap();
    engine
        .apply(
            &loaded,
            &PatchPlan::new(vec![PatchOp::set(&["v"], json!(3))]),
            "v3",
            None,
            None,
        )
        .unwrap();

    // 回滚到第一次操作的快照（即 v1 内容）。
    let record = engine
        .rollback(out1.record.snapshot.as_ref().unwrap(), ConfigFormat::Json)
        .unwrap();
    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&bed.target).unwrap()).unwrap();
    assert_eq!(after["v"], 1, "必须恢复到 v1");

    // 回滚前的当前内容（v3）已再次备份，可以"回滚回滚"。
    let v3_backup = engine
        .rollback(record.snapshot.as_ref().unwrap(), ConfigFormat::Json)
        .unwrap();
    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&bed.target).unwrap()).unwrap();
    assert_eq!(after["v"], 3, "回滚前备份必须可用");
    drop(v3_backup);

    // 历史记录完整：2 次 apply + 2 次 rollback。
    use mcpane_config::HistoryStore;
    let records = engine.history().list().unwrap();
    assert_eq!(records.len(), 4);
}

// ---------- no-op 不落快照、不留历史 ----------

#[test]
fn noop_apply_leaves_no_snapshot_and_no_history() {
    let bed = setup(r#"{"a": 1}"#, "json");
    let engine = engine(&bed.root);
    let plan = PatchPlan::new(vec![PatchOp::set(&["a"], json!(2))]);

    // 第一次实际写入。
    let loaded = engine.load(&bed.target, ConfigFormat::Json).unwrap();
    let first = engine.apply(&loaded, &plan, "写入", None, None).unwrap();
    assert!(first.written);

    // 同一计划再应用一次 → no-op。
    let loaded = engine.load(&bed.target, ConfigFormat::Json).unwrap();
    let outcome = engine.apply(&loaded, &plan, "重复", None, None).unwrap();
    assert!(!outcome.written);
    assert!(outcome.record.snapshot.is_none(), "no-op 不应产生快照");

    // 快照目录只有第一次写入那一对文件（内容 + meta）。
    let count = std::fs::read_dir(bed.root.join("config-snapshots"))
        .unwrap()
        .count();
    assert_eq!(count, 2, "no-op 不应落任何快照文件");

    use mcpane_config::HistoryStore;
    assert_eq!(
        engine.history().list().unwrap().len(),
        1,
        "no-op 不应留历史"
    );
}

// ---------- 快照保留策略 ----------

#[test]
fn snapshot_store_prunes_oldest_beyond_cap() {
    use mcpane_config::snapshot::SnapshotStore;
    let dir = tempfile::tempdir().unwrap();
    let store = FsSnapshotStore::new(dir.path());
    for i in 0..120 {
        store
            .save(
                Path::new("/tmp/example.json"),
                &format!("{{\"v\": {i}}}"),
                None,
            )
            .unwrap();
    }
    let count = std::fs::read_dir(dir.path().join("config-snapshots"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| !e.file_name().to_string_lossy().ends_with(".meta.json"))
        .count();
    assert_eq!(count, 100, "超上限的最旧快照应被淘汰");
}

// ---------- 每次写入都有 Diff + Snapshot（GATE-2 条件 3） ----------

#[test]
fn every_write_produces_diff_and_snapshot() {
    let bed = setup(r#"{"a": 1}"#, "json");
    let engine = engine(&bed.root);
    let loaded = engine.load(&bed.target, ConfigFormat::Json).unwrap();
    let outcome = engine
        .apply(
            &loaded,
            &PatchPlan::new(vec![PatchOp::set(&["b"], json!(2))]),
            "test",
            None,
            None,
        )
        .unwrap();
    assert!(!outcome.diff.entries.is_empty());
    assert!(!outcome.diff.unified.is_empty());
    let snapshot = outcome.record.snapshot.as_ref().unwrap();
    assert!(snapshot.stored_at.exists());
    // 快照内容即写前内容。
    assert_eq!(
        std::fs::read_to_string(&snapshot.stored_at).unwrap(),
        r#"{"a": 1}"#
    );
}

// ---------- Windows 外壳保真：行尾与 BOM（AGENTS.md 规则 7） ----------

/// 用字节建床：BOM 用 `&str` 表达不直观，且要验证的正是磁盘字节。
fn setup_bytes(content: &[u8], ext: &str) -> TestBed {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("mcpane-home");
    let target = dir.path().join(format!("config.{ext}"));
    std::fs::write(&target, content).unwrap();
    TestBed {
        root,
        target,
        _dir: dir,
    }
}

fn crlf_count(text: &str) -> (usize, usize) {
    let crlf = text.matches("\r\n").count();
    (crlf, text.matches('\n').count() - crlf)
}

/// Windows 用户的 settings.json 通常是 CRLF。改一个键之后，**未触碰的行**
/// 行尾必须还是 CRLF——否则 `git diff` 会把整份文件显示成变更。
#[test]
fn crlf_json_keeps_line_endings_after_apply() {
    let raw = "{\r\n  \"existingKey\": \"keep-me\",\r\n  \"mcpServers\": {}\r\n}\r\n";
    let bed = setup_bytes(raw.as_bytes(), "json");
    let eng = engine(&bed.root);

    let loaded = eng.load(&bed.target, ConfigFormat::Json).unwrap();
    let plan = PatchPlan::new(vec![PatchOp::set(
        &["mcpServers", "mcpane"],
        json!({ "type": "http" }),
    )]);
    let outcome = eng.apply(&loaded, &plan, "写入", None, None).unwrap();
    assert!(outcome.written);

    let after = std::fs::read_to_string(&bed.target).unwrap();
    let (crlf, lone_lf) = crlf_count(&after);
    assert_eq!(lone_lf, 0, "不得残留裸 LF：{after:?}");
    assert!(crlf >= 4, "CRLF 行尾必须保留，实际 {crlf} 处：{after:?}");
    // 内容确实写进去了（保真不等于没写）。
    let parsed: serde_json::Value = serde_json::from_str(&after).unwrap();
    assert_eq!(parsed["mcpServers"]["mcpane"]["type"], "http");
    assert_eq!(parsed["existingKey"], "keep-me");
}

/// Codex 的 config.toml 同理。
#[test]
fn crlf_toml_keeps_line_endings_after_apply() {
    let raw = "[existing]\r\nkeep = \"me\"\r\n";
    let bed = setup_bytes(raw.as_bytes(), "toml");
    let eng = engine(&bed.root);

    let loaded = eng.load(&bed.target, ConfigFormat::Toml).unwrap();
    let plan = PatchPlan::new(vec![PatchOp::set(
        &["mcp_servers", "mcpane", "url"],
        json!("http://127.0.0.1:24860/mcp"),
    )]);
    eng.apply(&loaded, &plan, "写入", None, None).unwrap();

    let after = std::fs::read_to_string(&bed.target).unwrap();
    let (crlf, lone_lf) = crlf_count(&after);
    assert_eq!(lone_lf, 0, "不得残留裸 LF：{after:?}");
    assert!(crlf >= 2, "CRLF 行尾必须保留：{after:?}");
    assert!(after.contains("keep = \"me\""));
}

/// Gemini 的 .env 走自写行模型，同样要保真。
#[test]
fn crlf_env_keeps_line_endings_after_apply() {
    let raw = "# comment\r\nEXISTING=1\r\n";
    let bed = setup_bytes(raw.as_bytes(), "env");
    let eng = engine(&bed.root);

    let loaded = eng.load(&bed.target, ConfigFormat::Env).unwrap();
    let plan = PatchPlan::new(vec![PatchOp::set(&["GEMINI_API_KEY"], json!("k"))]);
    eng.apply(&loaded, &plan, "写入", None, None).unwrap();

    let after = std::fs::read_to_string(&bed.target).unwrap();
    let (crlf, lone_lf) = crlf_count(&after);
    assert_eq!(lone_lf, 0, "不得残留裸 LF：{after:?}");
    assert!(crlf >= 3, "CRLF 行尾必须保留：{after:?}");
    assert!(after.contains("# comment"), "注释必须逐字保留：{after:?}");
}

/// LF 文件不能被"顺手"改成 CRLF——保真是双向的。
#[test]
fn lf_file_is_not_converted_to_crlf() {
    let raw = "{\n  \"a\": 1\n}\n";
    let bed = setup_bytes(raw.as_bytes(), "json");
    let eng = engine(&bed.root);

    let loaded = eng.load(&bed.target, ConfigFormat::Json).unwrap();
    let plan = PatchPlan::new(vec![PatchOp::set(&["b"], json!(2))]);
    eng.apply(&loaded, &plan, "写入", None, None).unwrap();

    let after = std::fs::read_to_string(&bed.target).unwrap();
    assert!(!after.contains('\r'), "LF 文件不得被转成 CRLF：{after:?}");
}

/// PowerShell 的 Out-File / 老记事本默认写 UTF-8 BOM。此前 BOM 会让 load 之后的
/// 每一步（preview / apply）都炸在 `expected value at line 1 column 1`。
#[test]
fn utf8_bom_json_is_parsed_and_preserved() {
    let mut raw = Vec::from(&[0xEF, 0xBB, 0xBF][..]);
    raw.extend_from_slice(b"{\r\n  \"mcpServers\": {}\r\n}\r\n");
    let bed = setup_bytes(&raw, "json");
    let eng = engine(&bed.root);

    let loaded = eng.load(&bed.target, ConfigFormat::Json).unwrap();
    let plan = PatchPlan::new(vec![PatchOp::set(
        &["mcpServers", "mcpane"],
        json!({ "type": "http" }),
    )]);

    // 预览也必须能出（此前 preview 与 apply 一样会炸）。
    eng.preview(&loaded, &plan).unwrap();
    eng.apply(&loaded, &plan, "写入", None, None).unwrap();

    let after = std::fs::read(&bed.target).unwrap();
    assert!(
        after.starts_with(&[0xEF, 0xBB, 0xBF]),
        "BOM 必须原样保留（VS Code 的 files.bom=preserve 语义），实际头部：{:02X?}",
        &after[..6.min(after.len())]
    );
    // BOM 之后仍是合法 JSON，且 CRLF 也保住了。
    let text = String::from_utf8(after[3..].to_vec()).unwrap();
    assert!(!text.contains('\n') || text.contains("\r\n"));
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed["mcpServers"]["mcpane"]["type"], "http");
}

/// 没有 BOM 的文件不得被凭空加上 BOM。
#[test]
fn bom_is_not_added_to_files_without_one() {
    let bed = setup_bytes(b"{\"a\": 1}\n", "json");
    let eng = engine(&bed.root);
    let loaded = eng.load(&bed.target, ConfigFormat::Json).unwrap();
    let plan = PatchPlan::new(vec![PatchOp::set(&["b"], json!(2))]);
    eng.apply(&loaded, &plan, "写入", None, None).unwrap();

    let after = std::fs::read(&bed.target).unwrap();
    assert!(!after.starts_with(&[0xEF, 0xBB, 0xBF]), "不得凭空加 BOM");
}

/// CRLF 文件上执行一个内容无变化的计划，必须判定为 no-op：既不写盘也不留快照。
/// 还原若放在 no-op 判断之后，这里会凭空写一次。
#[test]
fn crlf_file_noop_plan_does_not_write() {
    let raw = "{\r\n  \"a\": 1\r\n}\r\n";
    let bed = setup_bytes(raw.as_bytes(), "json");
    let eng = engine(&bed.root);

    let loaded = eng.load(&bed.target, ConfigFormat::Json).unwrap();
    let plan = PatchPlan::new(vec![PatchOp::set(&["a"], json!(1))]);
    let outcome = eng.apply(&loaded, &plan, "写入", None, None).unwrap();

    assert!(!outcome.written, "内容未变必须短路为 no-op");
    assert!(outcome.record.snapshot.is_none(), "no-op 不该留快照");
    assert_eq!(std::fs::read_to_string(&bed.target).unwrap(), raw);
}

/// Diff 必须如实反映落盘变化：CRLF 文件只改一个键时，unified diff 里不该出现
/// "整份文件都变了"，也不该把行尾差异藏起来。
#[test]
fn unified_diff_reflects_real_disk_change_on_crlf() {
    let raw = "{\r\n  \"keep\": \"me\",\r\n  \"a\": 1\r\n}\r\n";
    let bed = setup_bytes(raw.as_bytes(), "json");
    let eng = engine(&bed.root);

    let loaded = eng.load(&bed.target, ConfigFormat::Json).unwrap();
    let plan = PatchPlan::new(vec![PatchOp::set(&["a"], json!(2))]);
    let outcome = eng.apply(&loaded, &plan, "写入", None, None).unwrap();

    // 未触碰的 "keep" 行不得出现在增删行里（`-`/`+` 开头，排除 diff 头部）。
    for line in outcome.diff.unified.lines() {
        if (line.starts_with('-') || line.starts_with('+'))
            && !line.starts_with("---")
            && !line.starts_with("+++")
        {
            assert!(
                !line.contains("keep"),
                "未改动的行不该出现在 diff 里：{}",
                outcome.diff.unified
            );
        }
    }
}

/// 首次写入（文件不存在）恒用 LF，不跟随本机风格——另一个写者是跨平台 CLI，
/// 它们恒写 LF，跟随本机会让行尾在两个写者之间来回抖。此断言平台无关。
#[test]
fn first_write_uses_lf_regardless_of_host() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("mcpane-home");
    let target = dir.path().join("brand-new.json");
    let eng = engine(&root);

    let loaded = eng.load(&target, ConfigFormat::Json).unwrap();
    assert!(loaded.missing);
    let plan = PatchPlan::new(vec![PatchOp::set(&["mcpServers", "mcpane"], json!({}))]);
    eng.apply(&loaded, &plan, "首次写入", None, None).unwrap();

    let after = std::fs::read_to_string(&target).unwrap();
    assert!(after.contains('\n'), "应当写出了多行内容：{after:?}");
    assert!(
        !after.contains('\r'),
        "新建文件必须是 LF（与 Claude Code/Codex 等跨平台 CLI 一致）：{after:?}"
    );
}

/// 行尾还原在 serializer 之外做，所以对全部格式生效——JSON5（OpenClaw）与
/// YAML（Hermes）走的是另外两套 serializer，一并钉住。
#[test]
fn crlf_is_preserved_for_json5_and_yaml_too() {
    let json5 = setup_bytes(b"{\r\n  // comment\r\n  a: 1,\r\n}\r\n", "json");
    let eng = engine(&json5.root);
    let loaded = eng.load(&json5.target, ConfigFormat::Json5).unwrap();
    eng.apply(
        &loaded,
        &PatchPlan::new(vec![PatchOp::set(&["b"], json!(2))]),
        "写入",
        None,
        None,
    )
    .unwrap();
    let after = std::fs::read_to_string(&json5.target).unwrap();
    let (crlf, lone_lf) = crlf_count(&after);
    assert_eq!(lone_lf, 0, "JSON5 不得残留裸 LF：{after:?}");
    assert!(crlf > 0, "JSON5 行尾必须保留：{after:?}");

    let yaml = setup_bytes(b"model:\r\n  default: gpt-4\r\n", "yaml");
    let eng = engine(&yaml.root);
    let loaded = eng.load(&yaml.target, ConfigFormat::Yaml).unwrap();
    eng.apply(
        &loaded,
        &PatchPlan::new(vec![PatchOp::set(&["agent", "max_turns"], json!(10))]),
        "写入",
        None,
        None,
    )
    .unwrap();
    let after = std::fs::read_to_string(&yaml.target).unwrap();
    let (crlf, lone_lf) = crlf_count(&after);
    assert_eq!(lone_lf, 0, "YAML 不得残留裸 LF：{after:?}");
    assert!(crlf > 0, "YAML 行尾必须保留：{after:?}");
}

/// BOM 剥离同样在 parse 入口，对 YAML 也生效——它遇 BOM 的报错
/// （"more than one document"）比 JSON 的更误导，更该覆盖。
#[test]
fn utf8_bom_yaml_is_parsed_and_preserved() {
    let mut raw = Vec::from(&[0xEF, 0xBB, 0xBF][..]);
    raw.extend_from_slice(b"model:\n  default: gpt-4\n");
    let bed = setup_bytes(&raw, "yaml");
    let eng = engine(&bed.root);

    let loaded = eng.load(&bed.target, ConfigFormat::Yaml).unwrap();
    let plan = PatchPlan::new(vec![PatchOp::set(&["agent", "max_turns"], json!(10))]);
    eng.preview(&loaded, &plan).unwrap();
    eng.apply(&loaded, &plan, "写入", None, None).unwrap();

    let after = std::fs::read(&bed.target).unwrap();
    assert!(
        after.starts_with(&[0xEF, 0xBB, 0xBF]),
        "YAML 的 BOM 也要保留"
    );
}
