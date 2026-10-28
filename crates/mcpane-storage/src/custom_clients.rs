//! 用户手动添加的 MCP 客户端（见 0023_custom_mcp_clients.sql）。
//!
//! 这里只回答"有哪些自定义客户端、叫什么"。凭证与访问控制不在这一层——
//! 它们按 `connections.client_type` 工作，而本表的 `id` 就是那个取值。

use sqlx::Row;

use crate::{SqlitePool, StorageError, now_unix};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMcpClient {
    /// 稳定 id，同时是 `connections.client_type` 的取值。
    pub id: String,
    pub display: String,
    pub created_at: i64,
}

pub struct CustomClientRepo {
    pool: SqlitePool,
}

impl CustomClientRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 按添加时间排序（先加的在前，与客户端页的展示序一致）。
    pub async fn list(&self) -> Result<Vec<CustomMcpClient>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, display, created_at FROM custom_mcp_clients ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| CustomMcpClient {
                id: row.get("id"),
                display: row.get("display"),
                created_at: row.get::<i64, _>("created_at"),
            })
            .collect())
    }

    /// 新增或改名（id 不变）。
    pub async fn upsert(&self, id: &str, display: &str) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO custom_mcp_clients (id, display, created_at) VALUES (?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET display = excluded.display",
        )
        .bind(id)
        .bind(display)
        .bind(now_unix())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 删除条目本身。名下的连接（Token）由调用方另行撤销——那是吊销凭据的
    /// 动作，必须走 Hub 的控制通道让内存里的注册表同步失效，不能只删库。
    pub async fn remove(&self, id: &str) -> Result<bool, StorageError> {
        let done = sqlx::query("DELETE FROM custom_mcp_clients WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn exists(&self, id: &str) -> Result<bool, StorageError> {
        let row = sqlx::query("SELECT 1 FROM custom_mcp_clients WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::open_in_memory;

    async fn repo() -> CustomClientRepo {
        CustomClientRepo::new(open_in_memory().await.unwrap())
    }

    #[tokio::test]
    async fn upsert_lists_and_removes() {
        let repo = repo().await;
        repo.upsert("trae", "Trae CN").await.unwrap();
        repo.upsert("zed", "Zed").await.unwrap();
        let all = repo.list().await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, "trae");
        assert_eq!(all[0].display, "Trae CN");
        assert!(repo.exists("trae").await.unwrap());

        // 同 id 再 upsert 是改名，不是插第二条。
        repo.upsert("trae", "Trae 国内版").await.unwrap();
        let all = repo.list().await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].display, "Trae 国内版");

        assert!(repo.remove("trae").await.unwrap());
        assert!(!repo.remove("trae").await.unwrap(), "重复删除返回 false");
        assert!(!repo.exists("trae").await.unwrap());
        assert_eq!(repo.list().await.unwrap().len(), 1);
    }
}
