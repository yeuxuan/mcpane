//! ClientConnection 与 Token 生命周期（TASK-401）。

use std::collections::{HashMap, HashSet};

use sha2::{Digest, Sha256};
use sqlx::Row;
use subtle::ConstantTimeEq;

use crate::{SqlitePool, StorageError, new_id, now_unix};

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientConnection {
    pub id: String,
    pub name: String,
    pub client_type: String,
    pub token_prefix: String,
    pub default_workspace_id: Option<String>,
    /// `flat` / `gateway` / `auto`（未知值按 auto 处理，收敛在应用层）。
    pub tool_exposure_mode: String,
    pub enabled: bool,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

#[derive(Debug)]
pub struct NewConnection {
    pub name: String,
    /// `claude` / `codex` / `custom`
    pub client_type: String,
    pub default_workspace_id: Option<String>,
}

pub struct ConnectionRepo {
    pool: SqlitePool,
}

fn hash_token(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 使用系统安全随机源生成 256-bit Token（hex 编码，64 字符）。
/// 唯一实现：hub 侧发 Token 也复用（此前双实现漂移风险）。
pub fn generate_token() -> String {
    use rand::TryRngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut bytes)
        .unwrap_or_else(|error| panic!("系统安全随机源不可用: {error}"));
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl ConnectionRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 创建连接：返回记录与**仅此一次**展示的完整 Token。
    pub async fn create(
        &self,
        input: NewConnection,
    ) -> Result<(ClientConnection, String), StorageError> {
        let token = generate_token();
        let record = ClientConnection {
            id: new_id("conn"),
            name: input.name,
            client_type: input.client_type,
            token_prefix: token.chars().take(8).collect(),
            default_workspace_id: input.default_workspace_id,
            tool_exposure_mode: "auto".into(),
            enabled: true,
            created_at: now_unix(),
            last_used_at: None,
        };
        sqlx::query(
            "INSERT INTO client_connections
             (id, name, client_type, token_hash, token_prefix, default_workspace_id, enabled, created_at)
             VALUES (?, ?, ?, ?, ?, ?, 1, ?)",
        )
        .bind(&record.id)
        .bind(&record.name)
        .bind(&record.client_type)
        .bind(hash_token(&token))
        .bind(&record.token_prefix)
        .bind(&record.default_workspace_id)
        .bind(record.created_at)
        .execute(&self.pool)
        .await?;
        Ok((record, token))
    }

    /// 以固定 ID 写入/更新连接（dev 环境变量 Token 引导用；只存哈希）。
    pub async fn upsert_with_token(
        &self,
        id: &str,
        name: &str,
        client_type: &str,
        token: &str,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO client_connections
             (id, name, client_type, token_hash, token_prefix, enabled, created_at)
             VALUES (?, ?, ?, ?, ?, 1, ?)
             ON CONFLICT(id) DO UPDATE SET
               name = excluded.name, client_type = excluded.client_type,
               token_hash = excluded.token_hash, token_prefix = excluded.token_prefix,
               enabled = 1",
        )
        .bind(id)
        .bind(name)
        .bind(client_type)
        .bind(hash_token(token))
        .bind(token.chars().take(8).collect::<String>())
        .bind(now_unix())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 读取连接的 token_hash（注册表水合用；永不返回明文）。
    pub async fn list_with_hashes(&self) -> Result<Vec<(ClientConnection, String)>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, name, client_type, token_hash, token_prefix, default_workspace_id,
                    tool_exposure_mode, enabled, created_at, last_used_at
             FROM client_connections ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| (row_to_connection(r), r.get::<String, _>("token_hash")))
            .collect())
    }

    /// 常量时间验证 Token；命中则更新 last_used_at 并返回连接。
    pub async fn verify(&self, token: &str) -> Result<Option<ClientConnection>, StorageError> {
        let candidate = hash_token(token);
        let row = sqlx::query(
            "SELECT id, name, client_type, token_hash, token_prefix, default_workspace_id,
                    tool_exposure_mode, enabled, created_at, last_used_at
             FROM client_connections WHERE token_hash = ?",
        )
        .bind(&candidate)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };
        let stored: String = row.get("token_hash");
        let equal: bool = stored.as_bytes().ct_eq(candidate.as_bytes()).into();
        let enabled: i64 = row.get("enabled");
        if !equal || enabled != 1 {
            return Ok(None);
        }
        let connection = row_to_connection(&row);
        sqlx::query("UPDATE client_connections SET last_used_at = ? WHERE id = ?")
            .bind(now_unix())
            .bind(&connection.id)
            .execute(&self.pool)
            .await?;
        Ok(Some(connection))
    }

    pub async fn list(&self) -> Result<Vec<ClientConnection>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, name, client_type, token_prefix, default_workspace_id,
                    tool_exposure_mode, enabled, created_at, last_used_at
             FROM client_connections ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(row_to_connection).collect())
    }

    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<bool, StorageError> {
        let result = sqlx::query("UPDATE client_connections SET enabled = ? WHERE id = ?")
            .bind(enabled as i64)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 轮换 Token：生成新 Token（仅此一次返回明文），旧 Token 立即失效。
    pub async fn rotate_token(&self, id: &str) -> Result<String, StorageError> {
        let token = generate_token();
        let prefix: String = token.chars().take(8).collect();
        let result = sqlx::query(
            "UPDATE client_connections SET token_hash = ?, token_prefix = ? WHERE id = ?",
        )
        .bind(hash_token(&token))
        .bind(prefix)
        .bind(id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(StorageError::NotFound(id.to_owned()));
        }
        Ok(token)
    }

    /// 绑定连接的默认 Workspace。连接不存在即报 NotFound——静默 Ok 会让上层
    /// 把「绑定成功」回报给用户，而 Hub 侧的映射里根本不会出现这条连接。
    pub async fn set_default_workspace(
        &self,
        id: &str,
        workspace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let result =
            sqlx::query("UPDATE client_connections SET default_workspace_id = ? WHERE id = ?")
                .bind(workspace_id)
                .bind(id)
                .execute(&self.pool)
                .await?;
        if result.rows_affected() == 0 {
            return Err(StorageError::NotFound(id.to_owned()));
        }
        Ok(())
    }

    /// 设置连接的工具暴露模式（flat/gateway/auto）。合法性由调用方校验；
    /// 连接不存在报 NotFound，理由同 set_default_workspace。
    pub async fn set_tool_exposure(&self, id: &str, mode: &str) -> Result<(), StorageError> {
        let result =
            sqlx::query("UPDATE client_connections SET tool_exposure_mode = ? WHERE id = ?")
                .bind(mode)
                .bind(id)
                .execute(&self.pool)
                .await?;
        if result.rows_affected() == 0 {
            return Err(StorageError::NotFound(id.to_owned()));
        }
        Ok(())
    }

    pub async fn delete(&self, id: &str) -> Result<(), StorageError> {
        sqlx::query("DELETE FROM client_connections WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 访问控制（deny-list）：allowed=false 拒绝该连接访问某 Server，true 恢复。
    /// FK 同时校验连接与 Server 存在，deny 不存在的目标返回 Db 错误。
    pub async fn set_server_access(
        &self,
        connection_id: &str,
        server_id: &str,
        allowed: bool,
    ) -> Result<(), StorageError> {
        if allowed {
            sqlx::query(
                "DELETE FROM connection_server_denials
                 WHERE client_connection_id = ? AND server_id = ?",
            )
            .bind(connection_id)
            .bind(server_id)
            .execute(&self.pool)
            .await?;
        } else {
            sqlx::query(
                "INSERT OR IGNORE INTO connection_server_denials
                 (client_connection_id, server_id) VALUES (?, ?)",
            )
            .bind(connection_id)
            .bind(server_id)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    /// 单个连接被拒绝的 Server ID 列表（无行 = 全部允许）。
    pub async fn denied_servers(&self, connection_id: &str) -> Result<Vec<String>, StorageError> {
        let rows = sqlx::query(
            "SELECT server_id FROM connection_server_denials WHERE client_connection_id = ?",
        )
        .bind(connection_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| r.get("server_id")).collect())
    }

    /// 全量 denial 映射（Hub 启动/reload 水合内存用）。
    pub async fn denied_servers_map(
        &self,
    ) -> Result<HashMap<String, HashSet<String>>, StorageError> {
        let rows =
            sqlx::query("SELECT client_connection_id, server_id FROM connection_server_denials")
                .fetch_all(&self.pool)
                .await?;
        let mut map: HashMap<String, HashSet<String>> = HashMap::new();
        for row in &rows {
            map.entry(row.get("client_connection_id"))
                .or_default()
                .insert(row.get("server_id"));
        }
        Ok(map)
    }
}

fn row_to_connection(row: &sqlx::sqlite::SqliteRow) -> ClientConnection {
    ClientConnection {
        id: row.get("id"),
        name: row.get("name"),
        client_type: row.get("client_type"),
        token_prefix: row.get("token_prefix"),
        default_workspace_id: row.get("default_workspace_id"),
        tool_exposure_mode: row.get("tool_exposure_mode"),
        enabled: row.get::<i64, _>("enabled") == 1,
        created_at: row.get("created_at"),
        last_used_at: row.get("last_used_at"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::open_in_memory;

    #[tokio::test]
    async fn token_lifecycle_create_verify_rotate_revoke() {
        let pool = open_in_memory().await.unwrap();
        let repo = ConnectionRepo::new(pool);

        let (record, token) = repo
            .create(NewConnection {
                name: "Claude Code".into(),
                client_type: "claude".into(),
                default_workspace_id: None,
            })
            .await
            .unwrap();
        assert_eq!(token.len(), 64, "256-bit hex");
        assert_eq!(record.token_prefix, token[..8]);

        // 验证成功并更新 last_used。
        let verified = repo.verify(&token).await.unwrap().unwrap();
        assert_eq!(verified.id, record.id);
        assert!(repo.list().await.unwrap()[0].last_used_at.is_some());

        // 错误 Token 拒绝。
        assert!(repo.verify("wrong").await.unwrap().is_none());

        // 轮换：旧 Token 失效，新 Token 生效。
        let new_token = repo.rotate_token(&record.id).await.unwrap();
        assert_ne!(new_token, token);
        assert!(repo.verify(&token).await.unwrap().is_none());
        assert!(repo.verify(&new_token).await.unwrap().is_some());

        // 撤销后拒绝，恢复后放行。
        repo.set_enabled(&record.id, false).await.unwrap();
        assert!(repo.verify(&new_token).await.unwrap().is_none());
        repo.set_enabled(&record.id, true).await.unwrap();
        assert!(repo.verify(&new_token).await.unwrap().is_some());

        // 删除后永久拒绝（区别于撤销：撤销可恢复，删除不行）。
        repo.delete(&record.id).await.unwrap();
        assert!(repo.verify(&new_token).await.unwrap().is_none());
        assert!(repo.list().await.unwrap().is_empty());
    }

    /// 0005 迁移回归：client_type 不再受 CHECK 枚举限制（cursor/vscode 等可发 Token），
    /// 且重建后的表保住外键级联与 token_hash 唯一索引。
    #[tokio::test]
    async fn generic_client_types_survive_table_rebuild() {
        let pool = open_in_memory().await.unwrap();
        let repo = ConnectionRepo::new(pool.clone());
        let servers = crate::ServerRepo::new(pool.clone());

        // 原 CHECK 会拒绝的类型现在均可创建。
        for client_type in ["cursor", "vscode", "cline", "devin", "trae", "qoder"] {
            let (record, _) = repo
                .create(NewConnection {
                    name: client_type.to_owned(),
                    client_type: client_type.to_owned(),
                    default_workspace_id: None,
                })
                .await
                .unwrap_or_else(|e| panic!("{client_type} 应可创建连接: {e}"));
            assert_eq!(record.client_type, client_type);
        }

        // 外键仍指向重建后的父表：denial 可写入且随连接删除级联清空。
        let server = servers
            .upsert(crate::McpServerDef {
                id: String::new(),
                slug: "demo".into(),
                name: "Demo".into(),
                transport: crate::ServerTransport::Stdio,
                command: Some("npx".into()),
                args: vec![],
                env: std::collections::BTreeMap::new(),
                cwd: None,
                url: None,
                bearer: None,
                bearer_env: None,
                headers: std::collections::BTreeMap::new(),
                runtime_policy: "per-client-connection".into(),
                enabled: true,
            })
            .await
            .unwrap();
        let cursor_id = repo.list().await.unwrap()[0].id.clone();
        repo.set_server_access(&cursor_id, &server.id, false)
            .await
            .unwrap();
        repo.delete(&cursor_id).await.unwrap();
        let rows = sqlx::query("SELECT COUNT(*) AS n FROM connection_server_denials")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows.get::<i64, _>("n"), 0);

        // token_hash 唯一索引已随重建恢复：重复哈希被拒绝。
        repo.upsert_with_token("conn_dup_a", "a", "custom", "same-token")
            .await
            .unwrap();
        assert!(
            repo.upsert_with_token("conn_dup_b", "b", "custom", "same-token")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn server_access_denylist_lifecycle() {
        let pool = open_in_memory().await.unwrap();
        let repo = ConnectionRepo::new(pool.clone());
        let servers = crate::ServerRepo::new(pool.clone());

        let server = servers
            .upsert(crate::McpServerDef {
                id: String::new(),
                slug: "demo".into(),
                name: "Demo".into(),
                transport: crate::ServerTransport::Stdio,
                command: Some("npx".into()),
                args: vec![],
                env: std::collections::BTreeMap::new(),
                cwd: None,
                url: None,
                bearer: None,
                bearer_env: None,
                headers: std::collections::BTreeMap::new(),
                runtime_policy: "per-client-connection".into(),
                enabled: true,
            })
            .await
            .unwrap();
        let (connection, _) = repo
            .create(NewConnection {
                name: "Claude Code".into(),
                client_type: "claude".into(),
                default_workspace_id: None,
            })
            .await
            .unwrap();

        // 默认无 denial（全部允许）。
        assert!(
            repo.denied_servers(&connection.id)
                .await
                .unwrap()
                .is_empty()
        );

        // deny 出现且幂等。
        repo.set_server_access(&connection.id, &server.id, false)
            .await
            .unwrap();
        repo.set_server_access(&connection.id, &server.id, false)
            .await
            .unwrap();
        assert_eq!(
            repo.denied_servers(&connection.id).await.unwrap(),
            vec![server.id.clone()]
        );
        let map = repo.denied_servers_map().await.unwrap();
        assert!(map.get(&connection.id).unwrap().contains(&server.id));

        // 恢复允许后消失。
        repo.set_server_access(&connection.id, &server.id, true)
            .await
            .unwrap();
        assert!(
            repo.denied_servers(&connection.id)
                .await
                .unwrap()
                .is_empty()
        );

        // deny 不存在的 Server 被 FK 拒绝。
        assert!(
            repo.set_server_access(&connection.id, "srv_missing", false)
                .await
                .is_err()
        );

        // 删除连接后 denial 行级联清空。
        repo.set_server_access(&connection.id, &server.id, false)
            .await
            .unwrap();
        repo.delete(&connection.id).await.unwrap();
        let rows = sqlx::query("SELECT COUNT(*) AS n FROM connection_server_denials")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows.get::<i64, _>("n"), 0);
    }

    #[tokio::test]
    async fn tool_exposure_mode_defaults_to_auto_and_persists() {
        let pool = open_in_memory().await.unwrap();
        let repo = ConnectionRepo::new(pool);
        let (record, _) = repo
            .create(NewConnection {
                name: "x".into(),
                client_type: "codex".into(),
                default_workspace_id: None,
            })
            .await
            .unwrap();
        assert_eq!(record.tool_exposure_mode, "auto");
        assert_eq!(repo.list().await.unwrap()[0].tool_exposure_mode, "auto");

        repo.set_tool_exposure(&record.id, "gateway").await.unwrap();
        assert_eq!(repo.list().await.unwrap()[0].tool_exposure_mode, "gateway");

        // 连接不存在必须报 NotFound，不能静默 Ok。
        assert!(matches!(
            repo.set_tool_exposure("conn_missing", "flat").await,
            Err(StorageError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn database_never_stores_plaintext_token() {
        let pool = open_in_memory().await.unwrap();
        let repo = ConnectionRepo::new(pool.clone());
        let (_, token) = repo
            .create(NewConnection {
                name: "x".into(),
                client_type: "codex".into(),
                default_workspace_id: None,
            })
            .await
            .unwrap();
        let row = sqlx::query("SELECT token_hash, token_prefix FROM client_connections")
            .fetch_one(&pool)
            .await
            .unwrap();
        let hash: String = row.get("token_hash");
        let prefix: String = row.get("token_prefix");
        assert_ne!(hash, token);
        assert_eq!(prefix.len(), 8);
        assert!(!hash.contains(&token));
    }
}
