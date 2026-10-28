//! 读真实形态的 cc-switch 数据库：复制后只读打开、缺列容错、MCP 表缺席不算错。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use mcpane_ccswitch::{CcSwitchSource, read_all};

/// 刻意只建**早期 schema 有的那几列**（没有 notes / icon / in_failover_queue）：
/// 用户装的可能是任意历史版本，按最新 schema 写死列就会整库读不出来。
async fn seed_db(path: &Path, with_mcp: bool) {
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("建库");
    sqlx::query(
        "CREATE TABLE providers (
            id TEXT NOT NULL, app_type TEXT NOT NULL, name TEXT NOT NULL,
            settings_config TEXT NOT NULL, website_url TEXT, category TEXT,
            meta TEXT NOT NULL DEFAULT '{}', is_current BOOLEAN NOT NULL DEFAULT 0,
            PRIMARY KEY (id, app_type))",
    )
    .execute(&pool)
    .await
    .expect("建表");
    sqlx::query(
        "INSERT INTO providers (id, app_type, name, settings_config, website_url, category, meta, is_current)
         VALUES
         ('p1','claude','中转甲','{\"env\":{\"ANTHROPIC_BASE_URL\":\"https://a.test\",\"ANTHROPIC_AUTH_TOKEN\":\"sk-a\"}}','https://a.test',NULL,'{}',1),
         ('p2','claude','官方','{\"env\":{}}',NULL,'official','{}',0),
         ('p3','codex','中转乙','{\"auth\":{\"OPENAI_API_KEY\":\"sk-b\"},\"config\":\"model_provider = \\\"custom\\\"\\n\\n[model_providers.custom]\\nbase_url = \\\"https://b.test/v1\\\"\\n\"}',NULL,NULL,'{}',0)",
    )
    .execute(&pool)
    .await
    .expect("插入 provider");
    if with_mcp {
        sqlx::query("CREATE TABLE mcp_servers (id TEXT PRIMARY KEY, name TEXT NOT NULL, server_config TEXT NOT NULL)")
            .execute(&pool)
            .await
            .expect("建 mcp 表");
        sqlx::query(
            "INSERT INTO mcp_servers (id, name, server_config) VALUES
             ('m1','context7','{\"type\":\"http\",\"url\":\"https://mcp.context7.com/mcp\"}')",
        )
        .execute(&pool)
        .await
        .expect("插入 mcp");
    }
    pool.close().await;
}

#[tokio::test]
async fn reads_providers_and_servers_from_db() {
    let dir = tempfile::tempdir().expect("临时目录");
    let db = dir.path().join("cc-switch.db");
    seed_db(&db, true).await;

    let data = read_all(&CcSwitchSource::Db(db)).await.expect("读取");
    assert_eq!(data.providers.len(), 2, "官方项不该进来");
    let claude = data
        .providers
        .iter()
        .find(|p| p.profile.name == "中转甲")
        .expect("claude 条目");
    assert_eq!(claude.profile.base_url.as_deref(), Some("https://a.test"));
    assert_eq!(claude.api_key.as_deref(), Some("sk-a"));
    let codex = data
        .providers
        .iter()
        .find(|p| p.profile.name == "中转乙")
        .expect("codex 条目");
    assert_eq!(codex.profile.base_url.as_deref(), Some("https://b.test/v1"));
    assert_eq!(data.servers.len(), 1);
    assert_eq!(data.servers[0].0, "context7");
}

#[tokio::test]
async fn missing_mcp_table_is_not_an_error() {
    let dir = tempfile::tempdir().expect("临时目录");
    let db = dir.path().join("cc-switch.db");
    seed_db(&db, false).await;

    let data = read_all(&CcSwitchSource::Db(db)).await.expect("读取");
    assert_eq!(data.providers.len(), 2);
    assert!(data.servers.is_empty());
}

/// 源文件不能被改动：cc-switch 可能正开着，写它就是在动用户的活数据。
#[tokio::test]
async fn source_file_is_left_untouched() {
    let dir = tempfile::tempdir().expect("临时目录");
    let db = dir.path().join("cc-switch.db");
    seed_db(&db, true).await;
    let before = std::fs::metadata(&db)
        .expect("元数据")
        .modified()
        .expect("mtime");

    read_all(&CcSwitchSource::Db(db.clone()))
        .await
        .expect("读取");

    let after = std::fs::metadata(&db)
        .expect("元数据")
        .modified()
        .expect("mtime");
    assert_eq!(before, after, "读取不得改写源文件");
}

/// cc-switch 允许把数据目录挪走（云盘同步），覆盖值在它自己的 Tauri Store 里。
/// 不读这份覆盖，这类用户会被告知"没装 cc-switch"——而他们恰恰配置最多。
#[cfg(target_os = "macos")]
#[tokio::test]
async fn honours_custom_config_dir_from_cc_switch_store() {
    let home = tempfile::tempdir().expect("临时 home");
    let moved = home.path().join("CloudDrive/cc-switch-data");
    std::fs::create_dir_all(&moved).expect("建目录");
    seed_db(&moved.join("cc-switch.db"), true).await;

    let store = home
        .path()
        .join("Library/Application Support/com.ccswitch.desktop");
    std::fs::create_dir_all(&store).expect("建 store 目录");
    std::fs::write(
        store.join("app_paths.json"),
        serde_json::json!({ "app_config_dir_override": "~/CloudDrive/cc-switch-data" }).to_string(),
    )
    .expect("写 store");

    assert_eq!(mcpane_ccswitch::config_dir(home.path()), moved);
    let source = mcpane_ccswitch::discover(home.path()).expect("应找到挪走的库");
    let data = read_all(&source).await.expect("读取");
    assert_eq!(data.providers.len(), 2);
}

/// 没有覆盖（或覆盖指向已不存在的目录）时回落到默认目录，不能把用户带进死路。
#[test]
fn falls_back_to_default_dir_when_override_missing() {
    let home = tempfile::tempdir().expect("临时 home");
    assert_eq!(
        mcpane_ccswitch::config_dir(home.path()),
        home.path().join(".cc-switch")
    );
}

/// 新版 schema 带故障转移队列：名次按 `COALESCE(sort_index, 999999)` 排，
/// 不在队列的没有名次；早期 schema 缺这两列时全部视为不在队列（上面的用例覆盖）。
#[tokio::test]
async fn reads_failover_queue_rank_from_newer_schema() {
    let dir = tempfile::tempdir().expect("临时目录");
    let db = dir.path().join("cc-switch.db");
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&db)
        .create_if_missing(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("建库");
    sqlx::query(
        "CREATE TABLE providers (
            id TEXT NOT NULL, app_type TEXT NOT NULL, name TEXT NOT NULL,
            settings_config TEXT NOT NULL, sort_index INTEGER,
            meta TEXT NOT NULL DEFAULT '{}', in_failover_queue BOOLEAN NOT NULL DEFAULT 0,
            PRIMARY KEY (id, app_type))",
    )
    .execute(&pool)
    .await
    .expect("建表");
    sqlx::query(
        "INSERT INTO providers (id, app_type, name, settings_config, sort_index, in_failover_queue) VALUES
         ('a','claude','甲','{\"env\":{\"ANTHROPIC_BASE_URL\":\"https://a.test\"}}', 5, 1),
         ('b','claude','乙','{\"env\":{\"ANTHROPIC_BASE_URL\":\"https://b.test\"}}', NULL, 1),
         ('c','claude','丙','{\"env\":{\"ANTHROPIC_BASE_URL\":\"https://c.test\"}}', 1, 0)",
    )
    .execute(&pool)
    .await
    .expect("插入");
    pool.close().await;

    let data = read_all(&CcSwitchSource::Db(db)).await.expect("读取");
    let rank = |name: &str| {
        data.providers
            .iter()
            .find(|p| p.profile.name == name)
            .expect("条目")
            .failover_rank
    };
    assert_eq!(rank("甲"), Some(5));
    assert_eq!(rank("乙"), Some(999_999));
    assert_eq!(rank("丙"), None);
}

/// cc-switch 的本地代理请求覆盖与调过的代理参数随迁移带过来；默认值不搬。
#[tokio::test]
async fn reads_request_overrides_and_tuned_proxy_config() {
    let dir = tempfile::tempdir().expect("临时目录");
    let db = dir.path().join("cc-switch.db");
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&db)
        .create_if_missing(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("建库");
    for sql in [
        "CREATE TABLE providers (
            id TEXT NOT NULL, app_type TEXT NOT NULL, name TEXT NOT NULL,
            settings_config TEXT NOT NULL, meta TEXT NOT NULL DEFAULT '{}',
            PRIMARY KEY (id, app_type))",
        r#"INSERT INTO providers (id, app_type, name, settings_config, meta) VALUES
         ('a','claude','甲','{"env":{"ANTHROPIC_BASE_URL":"https://a.test"}}',
          '{"localProxyRequestOverrides":{"headers":{"X-Route":"blue"},"body":{"enable_thinking":false}}}')"#,
        "CREATE TABLE proxy_config (app_type TEXT PRIMARY KEY,
            streaming_first_byte_timeout INTEGER NOT NULL DEFAULT 60,
            streaming_idle_timeout INTEGER NOT NULL DEFAULT 120,
            circuit_failure_threshold INTEGER NOT NULL DEFAULT 4,
            circuit_timeout_seconds INTEGER NOT NULL DEFAULT 60)",
        "INSERT INTO proxy_config (app_type, streaming_first_byte_timeout, circuit_timeout_seconds)
         VALUES ('claude', 180, 5), ('codex', 60, 60), ('pi', 300, 60)",
    ] {
        sqlx::query(sql).execute(&pool).await.expect("建数据");
    }
    pool.close().await;

    let data = read_all(&CcSwitchSource::Db(db)).await.expect("读取");
    let profile = &data.providers[0].profile;
    assert_eq!(profile.headers["X-Route"], "blue");
    assert_eq!(profile.route_body_override["enable_thinking"], false);
    assert_eq!(data.routing_tuning.len(), 1, "默认值与不支持的工具不搬");
    let (app, tuning) = data.routing_tuning[0];
    assert_eq!(app, mcpane_provider::ClientApp::Claude);
    assert_eq!(tuning.first_byte_timeout_secs, 180);
    assert_eq!(tuning.cooldown_secs, 10, "低于下限的值截到允许范围");
}
