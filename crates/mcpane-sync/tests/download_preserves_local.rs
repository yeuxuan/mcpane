//! 云同步下载对**本机专属数据**的影响（跨 crate 集成）。
//!
//! `SYNC_LOCAL_TABLES` 的全部意义就是「这些表不随快照走」：日志、用量、
//! 本机设置，以及最要命的 `client_connections`——它存的是本机签发给各客户端的
//! Token 哈希，与 `$MCPANE_HOME/env.sh` 里发出去的明文一一对应，任何一次覆盖
//! 都会让所有客户端在下次 Hub 重启后齐刷刷 401。engine 的往返单测只验了
//! 「远端业务数据到位」，没有一条验「本机数据还在」——这里补上。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Mutex;

use mcpane_storage::{
    ConnectionRepo, McpServerDef, NewConnection, ServerRepo, ServerTransport, SqlitePool,
};
use mcpane_sync::{PutCondition, RemoteObject, RemoteStore, SyncEngine, SyncError};
use sqlx::Row;

/// 进程内远端：只需满足 RemoteStore 的语义，条件写按 etag 判定。
#[derive(Default)]
struct MemStore {
    objects: Mutex<HashMap<String, (Vec<u8>, String)>>,
}

impl RemoteStore for MemStore {
    async fn test(&self) -> Result<(), SyncError> {
        Ok(())
    }
    async fn ensure_dirs(&self) -> Result<(), SyncError> {
        Ok(())
    }
    async fn get(&self, key: &str, _max: u64) -> Result<Option<RemoteObject>, SyncError> {
        Ok(self
            .objects
            .lock()
            .unwrap()
            .get(key)
            .map(|(data, etag)| RemoteObject {
                data: data.clone(),
                etag: Some(etag.clone()),
            }))
    }
    async fn put(
        &self,
        key: &str,
        body: Vec<u8>,
        condition: PutCondition,
    ) -> Result<(), SyncError> {
        let mut objects = self.objects.lock().unwrap();
        let current = objects.get(key).map(|(_, etag)| etag.clone());
        match condition {
            PutCondition::IfMatch(expected) if current.as_deref() != Some(expected.as_str()) => {
                return Err(SyncError::Conflict(Default::default()));
            }
            PutCondition::IfAbsent if current.is_some() => {
                return Err(SyncError::Conflict(Default::default()));
            }
            _ => {}
        }
        let etag = format!("etag-{}", body.len());
        objects.insert(key.to_owned(), (body, etag));
        Ok(())
    }
    async fn delete(&self, key: &str) -> Result<(), SyncError> {
        self.objects.lock().unwrap().remove(key);
        Ok(())
    }
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

async fn count(pool: &SqlitePool, sql: &'static str) -> i64 {
    sqlx::query(sql).fetch_one(pool).await.unwrap().get(0)
}

struct Device {
    pool: SqlitePool,
    home: std::path::PathBuf,
}

async fn device(root: &std::path::Path, tag: &str) -> Device {
    let home = root.join(format!("home-{tag}"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("providers.json"), br#"{"schemaVersion":2}"#).unwrap();
    let pool = mcpane_storage::open_database(&root.join(format!("{tag}.db")))
        .await
        .unwrap();
    Device { pool, home }
}

/// 下载**绝不能**动本机专属数据：客户端连接凭据、请求日志、用量、本机设置。
///
/// 尤其是 `client_connections`：它的 token_hash 是本机签发的凭据指纹，被远端
/// 覆盖后 Hub 一重启，所有客户端的调用会齐刷刷 401，而用户完全无从把
/// 「今天全都失败」和「几天前同步过一次」联系起来。
#[tokio::test]
async fn download_never_touches_local_only_tables() {
    let dir = tempfile::tempdir().unwrap();
    let store = MemStore::default();

    // 设备 A：有业务数据，上传。
    let a = device(dir.path(), "a").await;
    ServerRepo::new(a.pool.clone())
        .upsert(server("from-device-a"))
        .await
        .unwrap();
    SyncEngine::new(&a.pool, &a.home)
        .upload(&store, None, false)
        .await
        .unwrap();

    // 设备 B：有自己的本机数据——两个客户端连接、日志、用量、改过的设置。
    let b = device(dir.path(), "b").await;
    let conns = ConnectionRepo::new(b.pool.clone());
    let (claude, claude_token) = conns
        .create(NewConnection {
            name: "claude".into(),
            client_type: "claude".into(),
            default_workspace_id: None,
        })
        .await
        .unwrap();
    conns
        .create(NewConnection {
            name: "codex".into(),
            client_type: "codex".into(),
            default_workspace_id: None,
        })
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO hub_request_logs
         (id, ts, request_id, client_connection_id, workspace_id, method, duration_ms, success)
         VALUES ('log1', 1, 'req1', 'c', 'ws_default', 'tools/call', 5, 1)",
    )
    .execute(&b.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE app_settings SET hub_enabled = 0 WHERE id = 1")
        .execute(&b.pool)
        .await
        .unwrap();

    // B 下载 A 的快照。
    SyncEngine::new(&b.pool, &b.home)
        .download(&store)
        .await
        .unwrap();

    // 业务数据来自 A。
    let slugs: Vec<String> = ServerRepo::new(b.pool.clone())
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.slug)
        .collect();
    assert_eq!(
        slugs,
        vec!["from-device-a".to_owned()],
        "业务数据应来自远端"
    );

    // 本机数据一条不少。
    assert_eq!(
        count(&b.pool, "SELECT COUNT(*) FROM client_connections").await,
        2,
        "本机签发的客户端连接必须原样保留——被覆盖等于静默吊销所有客户端"
    );
    assert!(
        conns.verify(&claude_token).await.unwrap().is_some(),
        "同步后本机发出去的 Token 必须仍然验得过（连接 {}）",
        claude.id
    );
    assert_eq!(
        count(&b.pool, "SELECT COUNT(*) FROM hub_request_logs").await,
        1,
        "本机请求日志不随快照走"
    );
    assert_eq!(
        count(&b.pool, "SELECT hub_enabled FROM app_settings WHERE id = 1").await,
        0,
        "本机设置（含 Hub 开关、代理、同步配置本身）不得被远端覆盖，否则会自锁"
    );
}

/// 下载之后数据库不得留下外键违规行。
///
/// 本机保留表里唯一 FK 指向被覆盖表的是 `connection_server_denials → mcp_servers`：
/// 本机对某个 Server 设过访问控制，而远端快照里没有这个 Server（别的设备根本
/// 没建过它），那条访问控制就悬空了。导入全程 FK 是关的，不清理就会常驻。
#[tokio::test]
async fn download_leaves_no_dangling_access_control() {
    let dir = tempfile::tempdir().unwrap();
    let store = MemStore::default();

    let a = device(dir.path(), "a").await;
    ServerRepo::new(a.pool.clone())
        .upsert(server("shared"))
        .await
        .unwrap();
    SyncEngine::new(&a.pool, &a.home)
        .upload(&store, None, false)
        .await
        .unwrap();

    // 设备 B 上有一个只属于本机的 Server，并对它设了访问控制。
    let b = device(dir.path(), "b").await;
    let local_only = ServerRepo::new(b.pool.clone())
        .upsert(server("only-on-b"))
        .await
        .unwrap();
    let (conn, _) = ConnectionRepo::new(b.pool.clone())
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
    .bind(&local_only.id)
    .execute(&b.pool)
    .await
    .unwrap();

    SyncEngine::new(&b.pool, &b.home)
        .download(&store)
        .await
        .unwrap();

    let violations: Vec<String> = sqlx::query("PRAGMA main.foreign_key_check")
        .fetch_all(&b.pool)
        .await
        .unwrap()
        .iter()
        .map(|r| format!("{} → {}", r.get::<String, _>(0), r.get::<String, _>(2)))
        .collect();
    assert!(
        violations.is_empty(),
        "同步下载后不得留下外键孤儿：{violations:?}"
    );
    // 连接本身属于本机保留表，不能被这次清理牵连删掉。
    assert_eq!(
        count(&b.pool, "SELECT COUNT(*) FROM client_connections").await,
        1,
        "清理孤儿访问控制不得连累连接本身"
    );
}

/// 从一台**数据更旧**的设备同步下来后，默认工作区必须仍然可用。
///
/// 与本地备份恢复同一条路径（都走 `import_snapshot` 的全表覆盖，而
/// `_sqlx_migrations` 不参与导入）：远端快照若早于某条播种迁移，种子行会被
/// 冲掉且没有任何东西会补回来。这条锁的是跨设备形态。
#[tokio::test]
async fn download_from_an_older_device_keeps_default_workspace_usable() {
    let dir = tempfile::tempdir().unwrap();
    let store = MemStore::default();

    // 设备 A 模拟「早于 0011」的数据：workspaces 里没有 ws_default 种子行。
    let a = device(dir.path(), "a").await;
    ServerRepo::new(a.pool.clone())
        .upsert(server("from-a"))
        .await
        .unwrap();
    sqlx::query("DELETE FROM workspaces WHERE id = 'ws_default'")
        .execute(&a.pool)
        .await
        .unwrap();
    SyncEngine::new(&a.pool, &a.home)
        .upload(&store, None, false)
        .await
        .unwrap();

    let b = device(dir.path(), "b").await;
    SyncEngine::new(&b.pool, &b.home)
        .download(&store)
        .await
        .unwrap();

    let repo = mcpane_storage::WorkspaceRepo::new(b.pool.clone());
    repo.set_item_override(
        "ws_default",
        mcpane_storage::ItemKind::Tool,
        &mcpane_storage::ToolOverride {
            server_id: "srv_a".into(),
            original_name: "dangerous".into(),
            enabled: false,
            alias: None,
            description_suffix: None,
        },
    )
    .await
    .expect("同步下载后默认工作区必须还能禁用工具");
    repo.set_tool_discovery("ws_default", true)
        .await
        .expect("同步下载后默认工作区的发现开关必须还能设置");
}
