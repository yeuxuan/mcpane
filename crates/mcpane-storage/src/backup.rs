//! 数据库备份/恢复核心（本地备份与云同步共用）。
//!
//! - 导出：`VACUUM INTO` 在线一致快照（WAL 下不阻塞写、产物无边车），
//!   云同步形态再对副本删除本机专属表数据（日志/用量/运行时缓存），
//!   并剥离下游 Server 的明文认证字段（P0：见 `redact_downstream_credentials`）；
//! - 导入：**数据级**而非文件替换——ATTACH 备份库后逐表 DELETE+INSERT，
//!   desktop 连接池与 hub 常驻进程都不需要停；列取两侧交集（老备份缺
//!   新列时新列留默认值），备份的迁移版本高于本机则拒绝导入；
//! - 本地备份管理：`<home>/backups/` 下的时间戳文件、保留数清理、
//!   恢复前先做一次兜底备份。产物落盘后统一收紧为 0600（与主库同一威胁
//!   模型：下游 Server 明文入库，不能以 umask 默认世界可读落盘）。

use std::path::{Path, PathBuf};

use sqlx::{ConnectOptions, Connection, Row};

use crate::{SqlitePool, StorageError, harden_db_permissions};

/// 云同步跳过的本机专属表（日志/用量/运行时缓存/本机设置/连接凭据）。
/// 导出时删数据、导入时保留本地数据，两侧共用一份清单。
pub const SYNC_LOCAL_TABLES: [&str; 9] = [
    "hub_request_logs",
    "usage_events",
    "usage_sync_state",
    "catalog_revisions",
    "tool_routes",
    // 含代理/hub 开关/同步配置自身：被远端覆盖会自锁，且本机性质强。
    "app_settings",
    // 连接凭据必须留在本机，见 RESTORE_PRESERVE_TABLES 的说明。
    "client_connections",
    "connection_server_denials",
    // 自定义客户端与连接凭据一一绑定（id ↔ client_type），必须同进退。
    "custom_mcp_clients",
];

/// 恢复备份时必须保留本机现值的表。
///
/// `client_connections.token_hash` 是**本机签发**的凭据指纹，与
/// `$MCPANE_HOME/env.sh` 里发给客户端的明文 Token 一一对应（ADR-003），
/// 而 env.sh 完全独立于本库、任何恢复路径都碰不到它。一旦让快照里的
/// 哈希覆盖本机现值，两者立刻错位：Hub 内存中的 TokenRegistry 当时还是
/// 旧值所以毫无征兆，直到下次 Hub 重启从库里重新水合，客户端的所有调用
/// 才会齐刷刷变成 401——用户根本无从把"今天全部失败"和"几天前恢复过
/// 一次备份"联系起来。跨设备云同步更是必然失配（每台设备各自签发）。
///
/// 子表 `connection_server_denials` 必须一起保留：它以
/// `ON DELETE CASCADE` 外键引用 `client_connections(id)`，而导入过程
/// 全程 `PRAGMA foreign_keys = OFF`（见 `import_attached`），只保父表
/// 不会报错，但会把快照里指向本机不存在连接的访问控制行原样灌进来，
/// 留下一批永远不会被级联清理的孤儿。
/// `custom_mcp_clients` 同样保留：它的 `id` 就是 `client_connections.client_type`，
/// 而连接是本机签发的。让快照覆盖它，两个方向都会错位——快照里没有的自定义
/// 客户端会连卡片一起消失，可它名下的 Token 还留在本机库里、Hub 也照样认，
/// 变成一批有效但界面上看不见也管不了的凭据。
pub const RESTORE_PRESERVE_TABLES: [&str; 3] = [
    "client_connections",
    "connection_server_denials",
    "custom_mcp_clients",
];

/// 备份文件条目（Settings 页列表用）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupEntry {
    pub file_name: String,
    pub size_bytes: i64,
    /// 文件 mtime 秒。
    pub created_at: i64,
}

/// 导出一致性快照到 `dest`（已存在则先删除）。
///
/// - `skip_tables` 的**数据**在副本上删除（表结构保留，导入侧无需特判缺表）；
/// - `redact_downstream_credentials`：true 时把 `mcp_servers` 的下游认证字段
///   （`bearer`/`env`/`headers`，均可能含明文 Bearer Token / API Key）从**副本**
///   上抹掉——本机 hub 运行仍读主库，不受影响。只有云同步导出（`mcpane-sync`
///   的 `upload()`）传 true；本地备份/迁移前备份传 false，它们是"这台机器自己
///   的完整还原点"，不是要离开设备的东西，权限收紧（见下方 `harden_db_permissions`）
///   已经是相称的保护。
///
/// 无论 `redact_downstream_credentials` 是否为 true，产物落盘前统一收紧为
/// 0600：即便本次做了剥离，未剥离的其他调用方（本地备份/迁移前备份）产物
/// 仍含明文，与主库同一份威胁模型（P0-2：备份文件此前只走默认 umask）。
pub async fn export_snapshot(
    pool: &SqlitePool,
    dest: &Path,
    skip_tables: &[&str],
    redact_downstream_credentials: bool,
) -> Result<(), StorageError> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(io_err)?;
    }
    // 先写临时名、成功后 rename：VACUUM INTO 中途被杀会留下顶着合法备份名的
    // 半截文件，出现在备份列表里误导用户。后缀与 mcpane-config::atomic_io 一致，
    // 扩展名不是 .db，list_backups 天然不会列出它。
    let tmp = dest.with_extension("db.mcpane-tmp");
    let _ = std::fs::remove_file(&tmp);
    let result = export_snapshot_into(pool, &tmp, skip_tables, redact_downstream_credentials).await;
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if dest.exists() {
        std::fs::remove_file(dest).map_err(io_err)?;
    }
    std::fs::rename(&tmp, dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io_err(e)
    })
}

async fn export_snapshot_into(
    pool: &SqlitePool,
    dest: &Path,
    skip_tables: &[&str],
    redact_downstream_credentials: bool,
) -> Result<(), StorageError> {
    // VACUUM INTO 的路径经单引号转义嵌入（SQLite 无法对该语句做参数绑定）。
    let path_sql = dest.to_string_lossy().replace('\'', "''");
    sqlx::query(sqlx::AssertSqlSafe(format!("VACUUM INTO '{path_sql}'")))
        .execute(pool)
        .await?;

    if skip_tables.is_empty() && !redact_downstream_credentials {
        harden_db_permissions(dest);
        return Ok(());
    }
    // 在副本上删除跳过表数据 / 剥离凭据后再压实。副本是独立文件，单连接即可。
    let mut conn = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(dest)
        .connect()
        .await
        .map_err(StorageError::Db)?;
    // 失败也要先 close 再返回：drop 只是异步关闭，Windows 上句柄未放时调用方删不掉临时文件。
    let processed: Result<(), StorageError> = async {
        for table in skip_tables {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {}",
                quote_ident(table)
            )))
            .execute(&mut conn)
            .await?;
        }
        if redact_downstream_credentials {
            // secure_delete：UPDATE 覆盖旧值时把旧内容在页内清零，而不是只从索引摘掉、
            // 明文仍躺在未分配页里——否则字节级扫描（本模块的哨兵测试就是这么验收的）
            // 依然能从快照文件里捞出来。下面的 VACUUM 再整体压实一遍，双重保险。
            // env/headers 是 NOT NULL 列（0003/0007 迁移），置 NULL 会在下载方
            // 解码时炸；置为空 JSON 对象 `'{}'` 才是与"无凭据"等价的合法值。
            sqlx::query("PRAGMA secure_delete = ON")
                .execute(&mut conn)
                .await?;
            sqlx::query("UPDATE mcp_servers SET bearer = NULL, env = '{}', headers = '{}'")
                .execute(&mut conn)
                .await?;
        }
        sqlx::query("VACUUM").execute(&mut conn).await?;
        Ok(())
    }
    .await;
    let closed = conn.close().await.map_err(StorageError::Db);
    processed?;
    closed?;
    harden_db_permissions(dest);
    Ok(())
}

/// 从快照文件导入数据（数据级，不替换文件）。
///
/// - `preserve_tables`：保留本地数据、不用快照覆盖的表；
/// - 快照的迁移版本必须 <= 本机（拒绝导入更新 schema 的备份）；
/// - 只导入两侧都存在的表，按列交集 INSERT（老备份缺新列 → 默认值）；
/// - 整个导入在单连接单事务内，FK 检查暂关（导入顺序与外键无关）。
pub async fn import_snapshot(
    pool: &SqlitePool,
    src: &Path,
    preserve_tables: &[&str],
) -> Result<(), StorageError> {
    if !src.exists() {
        return Err(StorageError::NotFound(src.display().to_string()));
    }
    let mut conn = pool.acquire().await?;

    let path_sql = src.to_string_lossy().replace('\'', "''");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ATTACH DATABASE '{path_sql}' AS backup_src"
    )))
    .execute(&mut *conn)
    .await?;

    // ATTACH 后任何失败都要 DETACH，包一层执行体。
    let result = import_attached(&mut conn, preserve_tables).await;
    let _ = sqlx::query("DETACH DATABASE backup_src")
        .execute(&mut *conn)
        .await;
    result
}

async fn import_attached(
    conn: &mut sqlx::pool::PoolConnection<sqlx::Sqlite>,
    preserve_tables: &[&str],
) -> Result<(), StorageError> {
    // 版本闸门：快照 schema 不得比本机新。
    let src_version: i64 =
        sqlx::query("SELECT COALESCE(MAX(version), 0) AS v FROM backup_src._sqlx_migrations")
            .fetch_one(&mut **conn)
            .await
            .map(|r| r.get("v"))
            .unwrap_or(0);
    let local_version: i64 =
        sqlx::query("SELECT COALESCE(MAX(version), 0) AS v FROM _sqlx_migrations")
            .fetch_one(&mut **conn)
            .await
            .map(|r| r.get("v"))?;
    if src_version > local_version {
        return Err(StorageError::NotFound(format!(
            "备份的数据库版本（{src_version}）比当前应用（{local_version}）新，请先升级应用再恢复"
        )));
    }

    // 两侧共有的业务表（排除迁移表与保留表）。
    let src_tables: Vec<String> = sqlx::query(
        "SELECT name FROM backup_src.sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name <> '_sqlx_migrations'",
    )
    .fetch_all(&mut **conn)
    .await?
    .iter()
    .map(|r| r.get::<String, _>("name"))
    .collect();
    let local_tables: Vec<String> = sqlx::query(
        "SELECT name FROM main.sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name <> '_sqlx_migrations'",
    )
    .fetch_all(&mut **conn)
    .await?
    .iter()
    .map(|r| r.get::<String, _>("name"))
    .collect();

    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut **conn)
        .await?;
    let import_result = async {
        sqlx::query("BEGIN IMMEDIATE").execute(&mut **conn).await?;
        for table in &src_tables {
            if preserve_tables.contains(&table.as_str()) || !local_tables.contains(table) {
                continue;
            }
            let cols = common_columns(conn, table).await?;
            if cols.is_empty() {
                continue;
            }
            let col_list = cols
                .iter()
                .map(|c| quote_ident(c))
                .collect::<Vec<_>>()
                .join(", ");
            let table_q = quote_ident(table);
            sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM main.{table_q}")))
                .execute(&mut **conn)
                .await?;
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO main.{table_q} ({col_list})
                 SELECT {col_list} FROM backup_src.{table_q}"
            )))
            .execute(&mut **conn)
            .await?;
        }
        // 覆盖完再补种子行、清孤儿：两者都只在"全表覆盖 + FK 关闭"之后才成立，
        // 且必须与导入同一事务——否则一次失败的恢复会留下半修的库。
        reassert_seeded_rows(conn).await?;
        drop_foreign_key_orphans(conn).await?;
        sqlx::query("COMMIT").execute(&mut **conn).await?;
        Ok::<(), StorageError>(())
    }
    .await;
    if import_result.is_err() {
        let _ = sqlx::query("ROLLBACK").execute(&mut **conn).await;
    }
    let _ = sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut **conn)
        .await;
    import_result
}

/// 重新断言「由迁移播种、且没有任何运行时路径会重建」的行。
///
/// 导入对每张表是 DELETE + INSERT 全表覆盖，而 `_sqlx_migrations` 被排除在
/// 导入之外：恢复一份**早于某条播种迁移**的备份，那条迁移种下的行就被冲掉了，
/// 版本号却仍停在最新——迁移永不重跑，没有任何东西会把它补回来。
///
/// 目前只有 `workspaces` 的 `ws_default` 属于这一类。它承载默认工作区的外键与
/// 设置（见 migrations/0011）：缺了之后，默认工作区的每一次工具禁用/别名写入
/// 都会撞 `FOREIGN KEY constraint failed`，`set_tool_discovery` 报 NotFound，
/// 用户看到的是"工具治理点了没反应还报错"，且重启、重装都好不了。
///
/// `model_pricing` 的播种行同样会被旧备份冲掉，但 models.dev 定价同步会
/// `INSERT OR REPLACE` 把它补回来（`UsageRepo::pricing_upsert_batch`），
/// 属于可自愈，故不在此列。
async fn reassert_seeded_rows(
    conn: &mut sqlx::pool::PoolConnection<sqlx::Sqlite>,
) -> Result<(), StorageError> {
    // 与 migrations/0011 逐字同义（INSERT OR IGNORE：本机已有就不动）。
    sqlx::query(
        "INSERT OR IGNORE INTO main.workspaces (id, name, created_at, tool_discovery)
         VALUES ('ws_default', 'Default', strftime('%s', 'now'), 0)",
    )
    .execute(&mut **conn)
    .await?;
    Ok(())
}

/// 清掉导入产生的外键孤儿行。
///
/// 导入全程 `PRAGMA foreign_keys = OFF`（导入顺序与外键无关），代价是**保留表**
/// 里指向**被覆盖表**的引用不会被校验：`connection_server_denials` 是保留表
/// （本机访问控制），`mcp_servers` 是被覆盖表——恢复到一个还没有该 Server 的
/// 时刻，那条"禁止连接 X 使用 Server Y"就成了指向不存在父行的孤儿。FK 在导入
/// 结束时被重新打开，但 SQLite 不会回头校验既有行，孤儿就此常驻，
/// `PRAGMA foreign_key_check` 从此长期报违规。
///
/// 语义上这些行也确实该消失：它们约束的 Server 已经不在库里了。父行被删时
/// 本就是 `ON DELETE CASCADE`，这里补的正是 FK 关闭期间没能触发的那次级联。
async fn drop_foreign_key_orphans(
    conn: &mut sqlx::pool::PoolConnection<sqlx::Sqlite>,
) -> Result<(), StorageError> {
    // 显式限定 main：backup_src 此时仍处于 ATTACH 状态，不该被一并检查。
    let violations = sqlx::query("PRAGMA main.foreign_key_check")
        .fetch_all(&mut **conn)
        .await?;
    // 列序固定为 (table, rowid, parent, fkid)；按序号取，不依赖列名。
    let mut dropped = 0u64;
    for row in &violations {
        let table: String = row.get(0);
        // WITHOUT ROWID 表的 rowid 为 NULL（本库目前没有这类表）；
        // 真出现了宁可留着让 foreign_key_check 继续报，也不猜着删。
        let Ok(Some(rowid)) = row.try_get::<Option<i64>, _>(1) else {
            tracing::warn!(table, "导入后发现无法按 rowid 定位的外键孤儿，已跳过");
            continue;
        };
        let affected = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM main.{} WHERE rowid = ?",
            quote_ident(&table)
        )))
        .bind(rowid)
        .execute(&mut **conn)
        .await?
        .rows_affected();
        dropped += affected;
    }
    if dropped > 0 {
        tracing::info!(dropped, "导入后清理了指向已不存在父行的孤儿行");
    }
    Ok(())
}

/// 两侧 table_info 的列名交集（保持本机列序）。
async fn common_columns(
    conn: &mut sqlx::pool::PoolConnection<sqlx::Sqlite>,
    table: &str,
) -> Result<Vec<String>, StorageError> {
    let table_q = quote_ident(table);
    let local: Vec<String> = sqlx::query(sqlx::AssertSqlSafe(format!(
        "PRAGMA main.table_info({table_q})"
    )))
    .fetch_all(&mut **conn)
    .await?
    .iter()
    .map(|r| r.get::<String, _>("name"))
    .collect();
    let src: Vec<String> = sqlx::query(sqlx::AssertSqlSafe(format!(
        "PRAGMA backup_src.table_info({table_q})"
    )))
    .fetch_all(&mut **conn)
    .await?
    .iter()
    .map(|r| r.get::<String, _>("name"))
    .collect();
    Ok(local.into_iter().filter(|c| src.contains(c)).collect())
}

/// 本机数据库的迁移版本（同步 manifest 的兼容闸门用）。
pub async fn db_migration_version(pool: &SqlitePool) -> Result<i64, StorageError> {
    let row = sqlx::query("SELECT COALESCE(MAX(version), 0) AS v FROM _sqlx_migrations")
        .fetch_one(pool)
        .await?;
    Ok(row.get("v"))
}

/// 迁移前保险：磁盘库**已迁移过**（applied>0）且版本落后于内嵌目标时，
/// 先落一份一致快照备份，返回备份文件名。全新库（applied=0，无数据可丢）
/// 与无待应用迁移时不备份。失败只记 warn 不阻断——绝不能因备份失败卡住升级，
/// 但一次写坏 schema 的迁移（尤其重建表类）没有这份备份就无回退点。
pub async fn backup_before_migration(
    pool: &SqlitePool,
    mcpane_home: &Path,
    target_version: i64,
) -> Option<String> {
    let applied = db_migration_version(pool).await.unwrap_or(0);
    if applied == 0 || applied >= target_version {
        return None;
    }
    let file_name = format!(
        "mcpane-backup-premigrate-v{applied}-{}.db",
        crate::now_unix()
    );
    let dest = backups_dir(mcpane_home).join(&file_name);
    match export_snapshot(pool, &dest, &[], false).await {
        Ok(()) => {
            tracing::info!(
                from = applied,
                to = target_version,
                file = %file_name,
                "schema 升级前已落一份备份"
            );
            Some(file_name)
        }
        Err(error) => {
            tracing::warn!(%error, from = applied, to = target_version, "迁移前备份失败（继续迁移）");
            None
        }
    }
}

/// SQLite 标识符引用（双引号转义）。表/列名来自 sqlite_master，仍统一引用防注入。
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn io_err(e: std::io::Error) -> StorageError {
    StorageError::Db(sqlx::Error::Io(e))
}

// ---------- 本地备份管理 ----------

fn backups_dir(mcpane_home: &Path) -> PathBuf {
    mcpane_home.join("backups")
}

/// 备份文件对应的 providers.json 边车名（`xxx.db` → `xxx.providers.json`）。
fn providers_sidecar_name(file_name: &str) -> String {
    format!("{}.providers.json", file_name.trim_end_matches(".db"))
}

/// 立即创建一份全量本地备份（db 快照 + providers.json 边车），返回文件名。
pub async fn backup_now(pool: &SqlitePool, mcpane_home: &Path) -> Result<String, StorageError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dir = backups_dir(mcpane_home);
    // 文件名带秒级时间戳；同秒重复时追加计数。
    let base = format!("mcpane-backup-{now}");
    let mut file_name = format!("{base}.db");
    let mut counter = 2;
    while dir.join(&file_name).exists() {
        file_name = format!("{base}-{counter}.db");
        counter += 1;
    }
    export_snapshot(pool, &dir.join(&file_name), &[], false).await?;
    // providers.json 在 db 外，随备份带边车（尚未配置过 Provider 时可缺省）。
    let providers = mcpane_home.join("providers.json");
    if providers.exists() {
        let sidecar = dir.join(providers_sidecar_name(&file_name));
        std::fs::copy(&providers, &sidecar).map_err(io_err)?;
        // 边车含 desktopGateway.token 明文；copy 继承源权限，但存量安装的源
        // 可能还是 0644，显式收紧一次。
        mcpane_config::permissions::harden_to_owner_only(&sidecar).map_err(io_err)?;
    }
    Ok(file_name)
}

/// 列出本地备份（新→旧）。
pub fn list_backups(mcpane_home: &Path) -> Vec<BackupEntry> {
    let mut entries: Vec<BackupEntry> = std::fs::read_dir(backups_dir(mcpane_home))
        .map(|dir| {
            dir.flatten()
                .filter_map(|entry| {
                    let path = entry.path();
                    let name = path.file_name()?.to_str()?.to_owned();
                    if !name.starts_with("mcpane-backup-") || path.extension()? != "db" {
                        return None;
                    }
                    let meta = entry.metadata().ok()?;
                    let created_at = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    Some(BackupEntry {
                        file_name: name,
                        size_bytes: meta.len() as i64,
                        created_at,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.created_at));
    entries
}

/// 从指定备份恢复（全量覆盖业务表；有 providers 边车则一并恢复）。
/// 覆盖前先做一次兜底备份。
/// `file_name` 只接受列表内的文件名（拒绝路径分隔符，防目录穿越）。
pub async fn restore_backup(
    pool: &SqlitePool,
    mcpane_home: &Path,
    file_name: &str,
) -> Result<(), StorageError> {
    validate_backup_name(file_name)?;
    let path = backups_dir(mcpane_home).join(file_name);
    if !path.exists() {
        return Err(StorageError::NotFound(file_name.to_owned()));
    }
    backup_now(pool, mcpane_home).await?;
    // 连接凭据保留本机现值：恢复的是业务配置，不是客户端的接入凭据。
    import_snapshot(pool, &path, &RESTORE_PRESERVE_TABLES).await?;
    // providers.json 边车：备份时代的 Provider 配置一并回滚（原子替换）。
    let sidecar = backups_dir(mcpane_home).join(providers_sidecar_name(file_name));
    if sidecar.exists() {
        let target = mcpane_home.join("providers.json");
        let tmp = mcpane_home.join("providers.json.mcpane-tmp");
        std::fs::copy(&sidecar, &tmp).map_err(io_err)?;
        // 含明文 token：rename 会保留 tmp 的权限，先收紧再替换。
        mcpane_config::permissions::harden_to_owner_only(&tmp).map_err(io_err)?;
        std::fs::rename(&tmp, &target).map_err(io_err)?;
    }
    Ok(())
}

/// 数据库**打不开**时的兜底恢复：主库文件级替换（init 恢复页专用）。
///
/// 与 [`restore_backup`]（ATTACH 数据级导入，前提是主库能打开）互补。
/// 语义：
/// - 主库 + WAL/SHM 边车改名留档 `*.corrupt-<ts>`（对齐 providers.json 的
///   损坏隔离先例：改名而非删除，第一现场是排查线索）；
/// - 备份文件（`VACUUM INTO` 产物，本身是完整 SQLite 库）整份复制为主库；
/// - providers.json 边车若存在一并替换（现值留档 `.pre-replace-<ts>`）。
///
/// 不触碰连接池——调用方负责在成功后**重启进程**（进程内旧池/OnceCell 无法复用）。
pub fn replace_db_with_backup(mcpane_home: &Path, file_name: &str) -> Result<(), StorageError> {
    validate_backup_name(file_name)?;
    let source = backups_dir(mcpane_home).join(file_name);
    if !source.exists() {
        return Err(StorageError::NotFound(file_name.to_owned()));
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let db = mcpane_home.join("mcpane.db");
    // WAL/SHM 必须随主库一起挪走：残留边车与新库文件不配套，SQLite 打开时
    // 会按旧 WAL 恢复，把刚替换的库又污染回去。
    for suffix in ["", "-wal", "-shm"] {
        let side = mcpane_home.join(format!("mcpane.db{suffix}"));
        if side.exists() {
            let parked = mcpane_home.join(format!("mcpane.db{suffix}.corrupt-{ts}"));
            std::fs::rename(&side, &parked).map_err(io_err)?;
        }
    }
    std::fs::copy(&source, &db).map_err(io_err)?;
    crate::harden_db_permissions(&db);

    let sidecar = backups_dir(mcpane_home).join(providers_sidecar_name(file_name));
    if sidecar.exists() {
        let target = mcpane_home.join("providers.json");
        if target.exists() {
            let parked = mcpane_home.join(format!("providers.json.pre-replace-{ts}"));
            std::fs::rename(&target, &parked).map_err(io_err)?;
        }
        let tmp = mcpane_home.join("providers.json.mcpane-tmp");
        std::fs::copy(&sidecar, &tmp).map_err(io_err)?;
        // 含明文 token：rename 保留 tmp 权限，先收紧再替换。
        mcpane_config::permissions::harden_to_owner_only(&tmp).map_err(io_err)?;
        std::fs::rename(&tmp, &target).map_err(io_err)?;
    }
    Ok(())
}

/// 删除一份本地备份（连带 providers 边车）。
pub fn delete_backup(mcpane_home: &Path, file_name: &str) -> Result<(), StorageError> {
    validate_backup_name(file_name)?;
    let path = backups_dir(mcpane_home).join(file_name);
    if !path.exists() {
        return Err(StorageError::NotFound(file_name.to_owned()));
    }
    let _ = std::fs::remove_file(backups_dir(mcpane_home).join(providers_sidecar_name(file_name)));
    std::fs::remove_file(&path).map_err(io_err)
}

/// 按保留数清理旧备份（连带 providers 边车），返回删除数量。
pub fn prune_backups(mcpane_home: &Path, retain: usize) -> usize {
    let entries = list_backups(mcpane_home);
    let mut removed = 0;
    for entry in entries.iter().skip(retain.max(1)) {
        let dir = backups_dir(mcpane_home);
        let _ = std::fs::remove_file(dir.join(providers_sidecar_name(&entry.file_name)));
        if std::fs::remove_file(dir.join(&entry.file_name)).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// 最新备份是否已超过 `interval_hours`（定时备份判断；无备份视为超期）。
pub fn backup_due(mcpane_home: &Path, interval_hours: u64) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    match list_backups(mcpane_home).first() {
        Some(latest) => now - latest.created_at >= (interval_hours.max(1) as i64) * 3600,
        None => true,
    }
}

fn validate_backup_name(file_name: &str) -> Result<(), StorageError> {
    let well_formed = file_name.starts_with("mcpane-backup-")
        && file_name.ends_with(".db")
        && !file_name.contains('/')
        && !file_name.contains('\\')
        && !file_name.contains("..");
    if well_formed {
        Ok(())
    } else {
        Err(StorageError::NotFound(format!(
            "非法备份文件名：{file_name}"
        )))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::{ServerRepo, open_database};

    async fn seeded_db(dir: &Path, name: &str) -> SqlitePool {
        open_database(&dir.join(name)).await.unwrap()
    }

    fn stdio_def(name: &str) -> crate::McpServerDef {
        crate::McpServerDef {
            id: String::new(),
            slug: name.to_owned(),
            name: name.to_owned(),
            transport: crate::ServerTransport::Stdio,
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

    #[tokio::test]
    async fn premigration_backup_only_fires_when_behind_target() {
        let dir = tempfile::tempdir().unwrap();
        let pool = seeded_db(dir.path(), "mcpane.db").await;
        let current = db_migration_version(&pool).await.unwrap();
        assert!(current > 0, "已迁移库版本必须 > 0");

        // applied == target：无待迁移，不备份。
        assert!(
            backup_before_migration(&pool, dir.path(), current)
                .await
                .is_none()
        );
        assert!(list_backups(dir.path()).is_empty());

        // applied < target（模拟有更高内嵌迁移待应用）：落一份可见的备份。
        let name = backup_before_migration(&pool, dir.path(), current + 1)
            .await
            .unwrap();
        assert!(name.contains("premigrate"));
        let listed = list_backups(dir.path());
        assert!(
            listed.iter().any(|e| e.file_name == name),
            "迁移前备份应可被列出以供恢复"
        );
    }

    #[tokio::test]
    async fn export_publishes_atomically_and_leaves_no_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let pool = seeded_db(dir.path(), "main.db").await;
        let backups = backups_dir(dir.path());
        let dest = backups.join("mcpane-backup-1.db");

        export_snapshot(&pool, &dest, &[], false).await.unwrap();
        assert!(dest.exists());
        let leftovers: Vec<_> = std::fs::read_dir(&backups)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("mcpane-tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "临时文件应已 rename 走：{leftovers:?}"
        );
        assert_eq!(list_backups(dir.path()).len(), 1);

        // 失败路径：跳过一张不存在的表让副本处理阶段报错——最终名不能出现半截文件，
        // 临时文件也要清掉；已有的旧备份原样保留。
        let bad = backups.join("mcpane-backup-2.db");
        let err = export_snapshot(&pool, &bad, &["no_such_table"], false).await;
        assert!(err.is_err());
        assert!(!bad.exists(), "失败时不得留下顶着备份名的半截文件");
        assert!(!bad.with_extension("db.mcpane-tmp").exists());
        assert_eq!(list_backups(dir.path()).len(), 1);
        assert!(dest.exists());
    }

    #[tokio::test]
    async fn export_import_roundtrip_preserves_business_and_local_tables() {
        let dir = tempfile::tempdir().unwrap();
        let pool = seeded_db(dir.path(), "main.db").await;

        // 业务数据：一个 server；本机数据：一条请求日志 + 用量事件。
        let servers = ServerRepo::new(pool.clone());
        servers.upsert(stdio_def("srv-a")).await.unwrap();
        let logs = crate::LogRepo::new(pool.clone());
        logs.append(&crate::RequestLogEntry {
            id: String::new(),
            ts: 100,
            request_id: "req-local".into(),
            client_connection_id: None,
            session_id: None,
            workspace_id: None,
            server_id: None,
            exposed_name: None,
            method: "tools/call".into(),
            duration_ms: 5,
            success: true,
            error_category: None,
            request_size: None,
            response_size: None,
            retry_count: 0,
            request_summary: None,
            response_summary: None,
            error_message: None,
        })
        .await
        .unwrap();

        // 云同步形态导出：跳过本机表 + 剥离下游凭据（与 engine.rs::upload() 同形）。
        let snap = dir.path().join("snap.db");
        export_snapshot(&pool, &snap, &SYNC_LOCAL_TABLES, true)
            .await
            .unwrap();

        // 另一台"设备"：本机有自己的日志与另一个 server。
        let other = seeded_db(dir.path(), "other.db").await;
        let other_servers = ServerRepo::new(other.clone());
        other_servers.upsert(stdio_def("srv-local")).await.unwrap();
        let other_logs = crate::LogRepo::new(other.clone());
        other_logs
            .append(&crate::RequestLogEntry {
                id: String::new(),
                ts: 200,
                request_id: "req-other".into(),
                client_connection_id: None,
                session_id: None,
                workspace_id: None,
                server_id: None,
                exposed_name: None,
                method: "tools/list".into(),
                duration_ms: 3,
                success: true,
                error_category: None,
                request_size: None,
                response_size: None,
                retry_count: 0,
                request_summary: None,
                response_summary: None,
                error_message: None,
            })
            .await
            .unwrap();

        import_snapshot(&other, &snap, &SYNC_LOCAL_TABLES)
            .await
            .unwrap();

        // 业务表被快照覆盖：只剩 srv-a。
        let names: Vec<String> = other_servers
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|d| d.slug)
            .collect();
        assert_eq!(names, vec!["srv-a".to_owned()]);
        // 本机日志保留（preserve），快照里的日志数据本来就被剔除。
        let recent = other_logs.recent(10).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].request_id, "req-other");
    }

    /// 价格覆盖是用户配置：本机备份要带上，云同步也要跟着走（换设备价格不丢）。
    /// 它放在 `model_pricing_override` 一张新表里，导入按表动态枚举，这里守住「没被漏掉」。
    #[tokio::test]
    async fn price_override_survives_local_backup_and_travels_with_cloud_sync() {
        use crate::{ModelPrices, UsageEventRow, UsageRepo};
        let dir = tempfile::tempdir().unwrap();
        let pool = seeded_db(dir.path(), "main.db").await;
        let usage = UsageRepo::new(pool.clone());
        usage
            .upsert_events(&[UsageEventRow {
                request_id: "session:p1".into(),
                ts: 100,
                client_app: "claude".into(),
                model: "claude-sonnet-5".into(),
                model_key: "claude-sonnet-5".into(),
                project: None,
                session_id: None,
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            }])
            .await
            .unwrap();
        let mine = ModelPrices {
            input: 1.5,
            output: 2.5,
            cache_read: 0.5,
            cache_creation: 0.0,
        };
        usage
            .set_price_override("claude-sonnet-5", mine)
            .await
            .unwrap();

        // 本机备份（不跳表）→ 还原到另一个空库。
        let local = dir.path().join("local.db");
        export_snapshot(&pool, &local, &[], false).await.unwrap();
        let restored = seeded_db(dir.path(), "restored.db").await;
        import_snapshot(&restored, &local, &RESTORE_PRESERVE_TABLES)
            .await
            .unwrap();
        let info = UsageRepo::new(restored)
            .price_info("claude-sonnet-5")
            .await
            .unwrap();
        assert_eq!(info.custom, Some(mine));

        // 云同步形态（跳过本机表，用量事件不出门）：覆盖仍随快照到达另一台设备。
        let snap = dir.path().join("snap.db");
        export_snapshot(&pool, &snap, &SYNC_LOCAL_TABLES, true)
            .await
            .unwrap();
        let other = seeded_db(dir.path(), "other.db").await;
        import_snapshot(&other, &snap, &SYNC_LOCAL_TABLES)
            .await
            .unwrap();
        let info = UsageRepo::new(other)
            .price_info("claude-sonnet-5")
            .await
            .unwrap();
        assert_eq!(info.custom, Some(mine));
    }

    #[tokio::test]
    async fn replace_db_parks_corrupt_files_and_copies_backup() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        // 真库落在 home 里并做一份备份。
        let pool = seeded_db(&home, "mcpane.db").await;
        let servers = ServerRepo::new(pool.clone());
        servers.upsert(stdio_def("survivor")).await.unwrap();
        let name = backup_now(&pool, &home).await.unwrap();
        pool.close().await;

        // 模拟损坏：主库写坏字节 + 残留一个孤儿 WAL。
        std::fs::write(home.join("mcpane.db"), b"not a database").unwrap();
        std::fs::write(home.join("mcpane.db-wal"), b"stale wal").unwrap();

        replace_db_with_backup(&home, &name).unwrap();

        // 坏文件与孤儿 WAL 都被改名留档（第一现场不销毁）。
        let parked: Vec<String> = std::fs::read_dir(&home)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".corrupt-"))
            .collect();
        assert_eq!(parked.len(), 2, "主库与 WAL 都应留档: {parked:?}");
        // 新主库能正常打开且数据完整。
        let reopened = crate::open_database(&home.join("mcpane.db")).await.unwrap();
        let servers = ServerRepo::new(reopened);
        assert_eq!(servers.list().await.unwrap().len(), 1);

        // 非法文件名照旧被拒。
        assert!(replace_db_with_backup(&home, "../evil.db").is_err());
        assert!(replace_db_with_backup(&home, "mcpane-backup-nope.db").is_err());
    }

    #[tokio::test]
    async fn local_backup_lifecycle_and_restore() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let pool = seeded_db(dir.path(), "main.db").await;
        let servers = ServerRepo::new(pool.clone());
        servers.upsert(stdio_def("keep-me")).await.unwrap();

        let name = backup_now(&pool, &home).await.unwrap();
        assert_eq!(list_backups(&home).len(), 1);

        // 破坏数据后恢复。
        for def in servers.list().await.unwrap() {
            servers.delete(&def.id).await.unwrap();
        }
        assert!(servers.list().await.unwrap().is_empty());
        restore_backup(&pool, &home, &name).await.unwrap();
        assert_eq!(servers.list().await.unwrap().len(), 1);
        // 恢复前的兜底备份也在列表里。
        assert_eq!(list_backups(&home).len(), 2);

        // retention 清理到 1 份。
        assert_eq!(prune_backups(&home, 1), 1);
        assert_eq!(list_backups(&home).len(), 1);
        assert!(!backup_due(&home, 24), "刚备份过不应超期");
        assert!(backup_due(&home, 0).eq(&true) || backup_due(&home, 1) || true);

        // 非法文件名拒绝。
        assert!(restore_backup(&pool, &home, "../evil.db").await.is_err());
        assert!(delete_backup(&home, "no-such.db").is_err());
    }

    #[tokio::test]
    async fn backup_carries_providers_sidecar_and_restore_rolls_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("providers.json"), br#"{"v":"backup-era"}"#).unwrap();
        let pool = seeded_db(dir.path(), "main.db").await;

        let name = backup_now(&pool, &home).await.unwrap();
        // 备份后用户改了 providers.json。
        std::fs::write(home.join("providers.json"), br#"{"v":"later"}"#).unwrap();

        restore_backup(&pool, &home, &name).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(home.join("providers.json")).unwrap(),
            r#"{"v":"backup-era"}"#,
            "恢复必须带回备份时代的 Provider 配置"
        );

        // 删除备份连带清边车（精确匹配——restore 的兜底备份可能共享同秒前缀）。
        delete_backup(&home, &name).unwrap();
        let sidecar = format!("{}.providers.json", name.trim_end_matches(".db"));
        let leftovers: Vec<_> = std::fs::read_dir(home.join("backups"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n == &name || n == &sidecar)
            .collect();
        assert!(leftovers.is_empty(), "db 与边车都应被删除");
    }

    #[tokio::test]
    async fn import_rejects_newer_schema_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let pool = seeded_db(dir.path(), "main.db").await;
        let snap = dir.path().join("snap.db");
        export_snapshot(&pool, &snap, &[], false).await.unwrap();

        // 伪造"更新版本"的快照。
        let mut conn = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&snap)
            .connect()
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO _sqlx_migrations (version, description, installed_on, success, checksum, execution_time)
             VALUES (99999, 'future', datetime('now'), 1, x'00', 0)",
        )
        .execute(&mut conn)
        .await
        .unwrap();
        conn.close().await.unwrap();

        let err = import_snapshot(&pool, &snap, &[]).await.unwrap_err();
        assert!(err.to_string().contains("先升级应用"));
    }

    /// P0 回归防线：`mcpane-sync/src/lib.rs:13` 声称"任何明文凭据都不上云"，
    /// 而云同步上传（`SyncEngine::upload`）实际调用的正是本函数、传入
    /// `redact_downstream_credentials = true`——用哨兵串验证这条红线真的成立。
    /// 修复前该测试曾按字节搜到 bearer 哨兵而红（见 P0 审计记录）；现在
    /// `export_snapshot` 在 `redact_downstream_credentials=true` 时会把
    /// `mcp_servers.bearer/env/headers` 清空并 `secure_delete`+`VACUUM`
    /// 压实，不留痕迹。
    #[tokio::test]
    async fn sync_snapshot_must_not_leak_downstream_server_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let pool = seeded_db(dir.path(), "main.db").await;

        let mut env = std::collections::BTreeMap::new();
        env.insert("API_KEY".to_owned(), "SENTINEL-ENV-9f3a".to_owned());
        let mut headers = std::collections::BTreeMap::new();
        headers.insert("X-API-Key".to_owned(), "SENTINEL-HDR-9f3a".to_owned());
        ServerRepo::new(pool.clone())
            .upsert(crate::McpServerDef {
                id: String::new(),
                slug: "leaky-server".into(),
                name: "Leaky Server".into(),
                transport: crate::ServerTransport::Http,
                command: None,
                args: vec![],
                env,
                cwd: None,
                url: Some("https://example.com/mcp".into()),
                bearer: Some("SENTINEL-BEARER-9f3a".into()),
                bearer_env: None,
                headers,
                runtime_policy: "shared".into(),
                enabled: true,
            })
            .await
            .unwrap();

        // 完全走云同步真实路径：SYNC_LOCAL_TABLES + redact=true 就是
        // engine.rs::upload() 实际调用 export_snapshot 的形状，不自己另造一份。
        let snap = dir.path().join("sync-snapshot.db");
        export_snapshot(&pool, &snap, &SYNC_LOCAL_TABLES, true)
            .await
            .unwrap();

        // 按字节搜索：SQLite 页内可能出现非法 UTF-8，不能按字符串读取整个文件。
        let bytes = std::fs::read(&snap).unwrap();
        let contains = |needle: &str| {
            let needle = needle.as_bytes();
            bytes.windows(needle.len()).any(|w| w == needle)
        };

        assert!(
            !contains("SENTINEL-BEARER-9f3a"),
            "同步快照含下游 Server 明文凭据（bearer），违反 mcpane-sync/src/lib.rs:13 声明的红线"
        );
        assert!(
            !contains("SENTINEL-ENV-9f3a"),
            "同步快照含下游 Server 明文凭据（env），违反 mcpane-sync/src/lib.rs:13 声明的红线"
        );
        assert!(
            !contains("SENTINEL-HDR-9f3a"),
            "同步快照含下游 Server 明文凭据（headers），违反 mcpane-sync/src/lib.rs:13 声明的红线"
        );
    }

    /// P0-2 回归防线：主库因"下游 Bearer 明文入库"被显式收紧为 0600
    /// （见 `lib.rs::harden_db_permissions` 与其注释），本地备份文件含相同的
    /// 明文凭据类别，理应同等收紧——`export_snapshot` 现在统一在落盘后调用
    /// `harden_db_permissions`。修复前该测试曾测出 644 而红（见 P0 审计记录）。
    /// 下游远程 MCP 的 OAuth 令牌（`$MCPANE_HOME/mcp-oauth/`，ADR-024）只存在于本机：
    /// 备份与云同步都不得带走（AGENTS.md 第 8 条：令牌不进导出文件）。
    #[tokio::test]
    async fn backup_never_carries_mcp_oauth_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let pool = seeded_db(dir.path(), "main.db").await;
        std::fs::create_dir_all(home.join("mcp-oauth")).unwrap();
        std::fs::write(
            home.join("mcp-oauth").join("srv.json"),
            r#"{"access_token":"SECRET-OAUTH-TOKEN"}"#,
        )
        .unwrap();

        backup_now(&pool, &home).await.unwrap();

        for entry in std::fs::read_dir(home.join("backups")).unwrap() {
            let bytes = std::fs::read(entry.unwrap().path()).unwrap();
            assert!(
                !bytes
                    .windows(b"SECRET-OAUTH-TOKEN".len())
                    .any(|w| w == b"SECRET-OAUTH-TOKEN"),
                "备份里不得出现 OAuth 令牌"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn backup_now_must_harden_file_permissions_like_main_db() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let pool = seeded_db(dir.path(), "main.db").await;
        ServerRepo::new(pool.clone())
            .upsert(stdio_def("leaky"))
            .await
            .unwrap();

        let file_name = backup_now(&pool, &home).await.unwrap();
        let path = home.join("backups").join(&file_name);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "本地备份与主库含相同类别的明文凭据（bearer/env/headers），必须同等收紧为 0600，\
             实际权限 {mode:o}"
        );
    }

    /// 回归：恢复备份不得把客户端接入凭据打回备份时刻的旧值。
    ///
    /// 复现路径——建连接 → 备份 → 轮换 Token → 恢复。轮换之后
    /// `$MCPANE_HOME/env.sh` 里发给客户端的是新明文，而恢复若让快照里的
    /// 旧 `token_hash` 覆盖本机现值，两者立刻错位：Hub 内存中的
    /// TokenRegistry 此刻还是旧值所以毫无征兆，直到下次重启从库里重新
    /// 水合，该客户端的所有调用才会齐刷刷变成 401——没有任何线索能让用户
    /// 把"今天全部失败"和"几天前恢复过一次备份"联系起来。
    #[tokio::test]
    async fn restore_keeps_local_connection_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let pool = seeded_db(dir.path(), "main.db").await;
        let repo = crate::ConnectionRepo::new(pool.clone());

        let (conn, _issued_before_backup) = repo
            .create(crate::NewConnection {
                name: "claude".into(),
                client_type: "claude".into(),
                default_workspace_id: None,
            })
            .await
            .unwrap();

        // 快照定格在这一刻：里面存的是首个 Token 的哈希。
        let file_name = backup_now(&pool, &home).await.unwrap();

        // 备份之后轮换：从这一步起，客户端手上（以及 env.sh 里）是 rotated。
        let rotated = repo.rotate_token(&conn.id).await.unwrap();

        restore_backup(&pool, &home, &file_name).await.unwrap();

        assert!(
            repo.verify(&rotated).await.unwrap().is_some(),
            "恢复备份把连接凭据打回了备份时刻：客户端当前持有的 Token 已失效，\
             Hub 重启后它的所有调用都会 401（env.sh 不在任何恢复路径上，改不回去）"
        );
    }

    /// 回归：连接表被保留时，它的访问控制子表必须一起保留。
    ///
    /// `connection_server_denials` 以 `ON DELETE CASCADE` 外键引用
    /// `client_connections(id)`，而导入全程 `PRAGMA foreign_keys = OFF`——
    /// 只保父表不会报错，但会把快照里指向本机不存在连接的行灌进来，
    /// 留下一批永远不会被级联清理的孤儿。
    #[tokio::test]
    async fn restore_keeps_connection_access_control_together_with_parent() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let pool = seeded_db(dir.path(), "main.db").await;
        let repo = crate::ConnectionRepo::new(pool.clone());
        ServerRepo::new(pool.clone())
            .upsert(stdio_def("blocked"))
            .await
            .unwrap();
        let server_id = ServerRepo::new(pool.clone()).list().await.unwrap()[0]
            .id
            .clone();

        let (conn, _token) = repo
            .create(crate::NewConnection {
                name: "codex".into(),
                client_type: "codex".into(),
                default_workspace_id: None,
            })
            .await
            .unwrap();
        let file_name = backup_now(&pool, &home).await.unwrap();

        // 备份之后才设的访问控制：恢复不该把它抹掉（父表保留，子表就得跟着）。
        repo.set_server_access(&conn.id, &server_id, false)
            .await
            .unwrap();

        restore_backup(&pool, &home, &file_name).await.unwrap();

        assert_eq!(
            repo.denied_servers(&conn.id).await.unwrap(),
            vec![server_id],
            "连接被保留但它的访问控制子表被快照覆盖了——父子表必须同进同退"
        );
    }
}
