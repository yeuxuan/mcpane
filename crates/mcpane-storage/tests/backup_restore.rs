//! 备份 / 恢复的端到端集成测试（真实 SQLite 文件，非内存库）。
//!
//! 这一路的单测都在同一个 `open_database` 出来的当代库上跑，照不出恢复真正的
//! 现实形态：**用户手里的备份往往比当前应用旧**。旧备份缺列、缺迁移播种的行，
//! 而 `_sqlx_migrations` 不参与导入——版本号仍停在最新，迁移永不重跑。
//! 本文件专门盯这条时间轴。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use mcpane_storage::{
    ConnectionRepo, ItemKind, McpServerDef, NewConnection, ServerRepo, ServerTransport, SqlitePool,
    ToolOverride, WorkspaceRepo, backup_now, delete_backup, list_backups, open_database,
    restore_backup,
};
use sqlx::Row;

const WS_DEFAULT: &str = "ws_default";

async fn db(home: &Path) -> SqlitePool {
    open_database(&home.join("mcpane.db")).await.unwrap()
}

fn server(slug: &str) -> McpServerDef {
    McpServerDef {
        id: String::new(),
        slug: slug.to_owned(),
        name: slug.to_owned(),
        transport: ServerTransport::Stdio,
        command: Some("echo".into()),
        args: vec![],
        env: Default::default(),
        cwd: None,
        url: None,
        bearer: None,
        bearer_env: None,
        headers: Default::default(),
        runtime_policy: "shared".into(),
        enabled: true,
    }
}

fn override_of(name: &str) -> ToolOverride {
    ToolOverride {
        server_id: "srv_a".into(),
        original_name: name.into(),
        enabled: false,
        alias: None,
        description_suffix: None,
    }
}

async fn count(pool: &SqlitePool, sql: &'static str) -> i64 {
    sqlx::query(sql).fetch_one(pool).await.unwrap().get(0)
}

async fn fk_violations(pool: &SqlitePool) -> Vec<(String, String)> {
    sqlx::query("PRAGMA main.foreign_key_check")
        .fetch_all(pool)
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get::<String, _>(0), r.get::<String, _>(2)))
        .collect()
}

/// 回归：恢复一份**早于 0011 播种迁移**的备份后，默认工作区必须仍然可用。
///
/// 导入对每张表是 DELETE + INSERT 全表覆盖，`_sqlx_migrations` 不参与导入。
/// 于是 0011 种下的 `ws_default` 行被旧备份冲掉，而版本号仍是最新——迁移永不
/// 重跑，全仓也没有任何运行时路径会重建它。后果不是"少一行"，而是默认工作区
/// 的每一次工具禁用/别名写入都撞 `FOREIGN KEY constraint failed`
///（0011 的注释原话：写库失败、UI 报错），且重启重装都好不了。
#[tokio::test]
async fn restoring_a_pre_seed_backup_keeps_the_default_workspace_usable() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let pool = db(home).await;

    // 造一份「0011 之前」的备份：种子行尚不存在的那个时代。
    sqlx::query("DELETE FROM workspaces WHERE id = ?")
        .bind(WS_DEFAULT)
        .execute(&pool)
        .await
        .unwrap();
    let old_backup = backup_now(&pool, home).await.unwrap();

    // 本机是健康的当代库。
    pool.close().await;
    std::fs::remove_file(home.join("mcpane.db")).unwrap();
    let pool = db(home).await;
    assert_eq!(
        count(
            &pool,
            "SELECT COUNT(*) FROM workspaces WHERE id = 'ws_default'"
        )
        .await,
        1
    );

    restore_backup(&pool, home, &old_backup).await.unwrap();

    let repo = WorkspaceRepo::new(pool.clone());
    repo.set_item_override(WS_DEFAULT, ItemKind::Tool, &override_of("dangerous_tool"))
        .await
        .expect("恢复旧备份后默认工作区必须还能禁用工具");
    repo.set_tool_discovery(WS_DEFAULT, true)
        .await
        .expect("恢复旧备份后默认工作区的发现开关必须还能设置");
    assert!(
        repo.list().await.unwrap().is_empty(),
        "补回的种子行不得作为自建工作区出现在列表里"
    );
}

/// 回归：恢复之后数据库不得留下外键违规行。
///
/// 导入全程 `PRAGMA foreign_keys = OFF`。`connection_server_denials` 是保留表
///（本机访问控制不随备份走），`mcp_servers` 是被覆盖表——恢复到一个还没有该
/// Server 的时刻，那条"禁止连接 X 使用 Server Y"就指向了不存在的父行。FK 在
/// 导入结束时重新打开，但 SQLite 不回头校验既有行，孤儿就此常驻。
#[tokio::test]
async fn restoring_leaves_no_foreign_key_orphans() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let pool = db(home).await;

    // 备份时刻：还没有任何 Server。
    let empty_backup = backup_now(&pool, home).await.unwrap();

    // 之后新增 Server + 连接 + 针对该 Server 的访问控制拒绝。
    let created = ServerRepo::new(pool.clone())
        .upsert(server("fs"))
        .await
        .unwrap();
    let (conn, _token) = ConnectionRepo::new(pool.clone())
        .create(NewConnection {
            name: "claude".into(),
            client_type: "claude".into(),
            default_workspace_id: None,
        })
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO connection_server_denials (client_connection_id, server_id) VALUES (?, ?)",
    )
    .bind(&conn.id)
    .bind(&created.id)
    .execute(&pool)
    .await
    .unwrap();

    restore_backup(&pool, home, &empty_backup).await.unwrap();

    assert!(
        fk_violations(&pool).await.is_empty(),
        "恢复后不得留下外键孤儿：{:?}",
        fk_violations(&pool).await
    );
    // 连接本身属于保留表，不能被这次清理牵连删掉。
    assert_eq!(
        count(&pool, "SELECT COUNT(*) FROM client_connections").await,
        1,
        "本机签发的连接凭据必须原样保留"
    );
}

/// 旧 schema 的备份（缺列、迁移版本更低）恢复到当代库：按列交集导入，
/// 备份没有的新列取本机默认值，不得整体失败。
///
/// 这是最常见的真实恢复形态——用户在旧版本备份、升级应用后再恢复。
#[tokio::test]
async fn restoring_an_older_schema_backup_fills_new_columns_with_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let pool = db(home).await;

    let repo = WorkspaceRepo::new(pool.clone());
    let ws = repo.create("研发").await.unwrap();
    repo.set_tool_discovery(&ws.id, true).await.unwrap();
    let backup = backup_now(&pool, home).await.unwrap();

    // 把这份备份改造成「0008 之前」的形态：workspaces 没有 tool_discovery 列，
    // 迁移版本回落到 7。这正是旧版应用产出的备份的样子。
    let backup_path = home.join("backups").join(&backup);
    {
        let old = SqlitePool::connect(&format!("sqlite://{}", backup_path.display()))
            .await
            .unwrap();
        sqlx::query("ALTER TABLE workspaces DROP COLUMN tool_discovery")
            .execute(&old)
            .await
            .unwrap();
        sqlx::query("DELETE FROM _sqlx_migrations WHERE version >= 8")
            .execute(&old)
            .await
            .unwrap();
        old.close().await;
    }

    restore_backup(&pool, home, &backup).await.unwrap();

    // 数据来自备份，缺的新列取本机默认（0 = 关闭），而不是让整次恢复失败。
    let listed = repo.list().await.unwrap();
    assert_eq!(listed.len(), 1, "备份里的工作区必须恢复出来");
    assert_eq!(listed[0].name, "研发");
    assert!(
        !listed[0].tool_discovery,
        "旧备份没有这一列，应取本机默认值而不是报错"
    );
    // 种子行与外键完整性照旧成立。
    assert_eq!(
        count(
            &pool,
            "SELECT COUNT(*) FROM workspaces WHERE id = 'ws_default'"
        )
        .await,
        1
    );
    assert!(fk_violations(&pool).await.is_empty());
}

/// 备份文件名是外部输入（前端传下来的字符串），必须挡住目录穿越，
/// 且不得因为拒绝而误删/误读库外的文件。
#[tokio::test]
async fn backup_file_names_are_validated_against_path_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let pool = db(home).await;

    // 库外的"敏感文件"，任何一次调用都不许碰它。
    let outside = dir.path().join("secret.db");
    std::fs::write(&outside, b"do not touch").unwrap();

    for evil in [
        "../secret.db",
        "mcpane-backup-../../secret.db",
        "mcpane-backup-/etc/passwd.db",
        "..\\secret.db",
        "mcpane-backup-..\\..\\secret.db",
        "not-a-backup.db",
        "mcpane-backup-1.txt",
    ] {
        assert!(
            restore_backup(&pool, home, evil).await.is_err(),
            "恢复必须拒绝非法文件名：{evil}"
        );
        assert!(
            delete_backup(home, evil).is_err(),
            "删除必须拒绝非法文件名：{evil}"
        );
    }
    assert_eq!(
        std::fs::read(&outside).unwrap(),
        b"do not touch",
        "库外文件不得被读写或删除"
    );
}

/// 恢复会先落一份兜底备份——这是"恢复错了还能回去"的唯一依据。
#[tokio::test]
async fn restore_takes_a_safety_backup_of_the_current_state_first() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let pool = db(home).await;

    let servers = ServerRepo::new(pool.clone());
    servers.upsert(server("before")).await.unwrap();
    let snapshot = backup_now(&pool, home).await.unwrap();

    // 备份之后又加了一个 Server；恢复会把它冲掉。
    servers.upsert(server("added-after")).await.unwrap();
    assert_eq!(count(&pool, "SELECT COUNT(*) FROM mcp_servers").await, 2);

    restore_backup(&pool, home, &snapshot).await.unwrap();
    assert_eq!(count(&pool, "SELECT COUNT(*) FROM mcp_servers").await, 1);

    // 兜底备份应当存在，且能把 added-after 找回来。
    let safety = list_backups(home)
        .into_iter()
        .find(|entry| entry.file_name != snapshot)
        .expect("恢复前必须留下一份兜底备份");
    restore_backup(&pool, home, &safety.file_name)
        .await
        .unwrap();
    assert_eq!(
        count(&pool, "SELECT COUNT(*) FROM mcp_servers").await,
        2,
        "兜底备份必须能把恢复前的状态整体找回来"
    );
}

/// `VACUUM INTO` 是在线一致快照：导出期间并发写入不得让产物出现半截事务。
#[tokio::test]
async fn export_is_a_consistent_snapshot_under_concurrent_writes() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let pool = db(home).await;
    let servers = ServerRepo::new(pool.clone());
    for i in 0..20 {
        servers.upsert(server(&format!("s{i}"))).await.unwrap();
    }

    // 一边持续写入，一边做备份。
    let writer_pool = pool.clone();
    let writer = tokio::spawn(async move {
        let repo = ServerRepo::new(writer_pool);
        for i in 20..80 {
            repo.upsert(server(&format!("s{i}"))).await.unwrap();
        }
    });
    let name = backup_now(&pool, home).await.unwrap();
    writer.await.unwrap();

    // 快照必须是一个自洽的库：能打开、能查、无外键违规。
    let path = home.join("backups").join(&name);
    let snap = SqlitePool::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    let rows: i64 = sqlx::query("SELECT COUNT(*) FROM mcp_servers")
        .fetch_one(&snap)
        .await
        .unwrap()
        .get(0);
    assert!(
        (20..=80).contains(&rows),
        "快照条数应落在导出窗口内，实际 {rows}"
    );
    let bad = sqlx::query("PRAGMA main.foreign_key_check")
        .fetch_all(&snap)
        .await
        .unwrap();
    assert!(bad.is_empty(), "在线快照不得包含外键违规");
    let integrity: String = sqlx::query("PRAGMA integrity_check")
        .fetch_one(&snap)
        .await
        .unwrap()
        .get(0);
    assert_eq!(integrity, "ok", "快照必须通过完整性检查");
    snap.close().await;
}

/// 保留数清理：只留最新 N 份，边车一起删，且不误删非备份文件。
#[tokio::test]
async fn pruning_keeps_the_newest_and_removes_sidecars() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let pool = db(home).await;
    std::fs::write(home.join("providers.json"), r#"{"providers":[]}"#).unwrap();

    let mut names = Vec::new();
    for _ in 0..4 {
        names.push(backup_now(&pool, home).await.unwrap());
    }
    assert_eq!(list_backups(home).len(), 4);
    // 每份都应带上 providers.json 边车。
    let backups = home.join("backups");
    for name in &names {
        let sidecar = backups.join(format!("{}.providers.json", name.trim_end_matches(".db")));
        assert!(sidecar.exists(), "备份应带 providers 边车：{name}");
    }
    // 无关文件不得被清理波及。
    let bystander = backups.join("readme.txt");
    std::fs::write(&bystander, b"keep me").unwrap();

    let removed = mcpane_storage::prune_backups(home, 2);
    assert_eq!(removed, 2, "4 份保留 2 份应删掉 2 份");
    assert_eq!(list_backups(home).len(), 2);
    // 被删那两份的边车必须一起消失，不留孤儿边车。
    let orphan_sidecars = std::fs::read_dir(&backups)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".providers.json"))
        .count();
    assert_eq!(orphan_sidecars, 2, "边车数量应与剩余备份一致");
    assert!(bystander.exists(), "非备份文件不得被清理");
}
