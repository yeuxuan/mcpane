//! MCP Server 定义（TASK-404 数据面）。

use sqlx::Row;

use crate::{SqlitePool, StorageError, new_id, now_unix};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServerTransport {
    Stdio,
    Http,
    /// HTTP+SSE（MCP 2024-11-05 legacy 传输，存量远程 Server 仍在用）。
    Sse,
}

impl ServerTransport {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Http => "http",
            Self::Sse => "sse",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerDef {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub transport: ServerTransport,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: std::collections::BTreeMap<String, String>,
    pub cwd: Option<String>,
    pub url: Option<String>,
    /// Bearer Token 直填值（下游 HTTP 认证；优先于 bearer_env）。
    pub bearer: Option<String>,
    /// Bearer Token 环境变量名（间接取值：先查 env 列，再查 Hub 进程环境）。
    pub bearer_env: Option<String>,
    /// 自定义 HTTP 请求头（非 Bearer 认证如 X-API-Key）。
    pub headers: std::collections::BTreeMap<String, String>,
    pub runtime_policy: String,
    pub enabled: bool,
}

pub struct ServerRepo {
    pool: SqlitePool,
}

impl ServerRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn upsert(&self, mut def: McpServerDef) -> Result<McpServerDef, StorageError> {
        if def.id.is_empty() {
            def.id = new_id("srv");
        }
        sqlx::query(
            "INSERT INTO mcp_servers
             (id, slug, name, transport, command, args, env, cwd, url, bearer, bearer_env,
              headers, runtime_policy, enabled, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
               slug=excluded.slug, name=excluded.name, transport=excluded.transport,
               command=excluded.command, args=excluded.args, env=excluded.env,
               cwd=excluded.cwd, url=excluded.url, bearer=excluded.bearer,
               bearer_env=excluded.bearer_env, headers=excluded.headers,
               runtime_policy=excluded.runtime_policy, enabled=excluded.enabled",
        )
        .bind(&def.id)
        .bind(&def.slug)
        .bind(&def.name)
        .bind(def.transport.as_str())
        .bind(&def.command)
        .bind(serde_json::to_string(&def.args).unwrap_or_else(|_| "[]".into()))
        .bind(serde_json::to_string(&def.env).unwrap_or_else(|_| "{}".into()))
        .bind(&def.cwd)
        .bind(&def.url)
        .bind(&def.bearer)
        .bind(&def.bearer_env)
        .bind(serde_json::to_string(&def.headers).unwrap_or_else(|_| "{}".into()))
        .bind(&def.runtime_policy)
        .bind(def.enabled as i64)
        .bind(now_unix())
        .execute(&self.pool)
        .await?;
        Ok(def)
    }

    pub async fn get(&self, id: &str) -> Result<Option<McpServerDef>, StorageError> {
        let row = sqlx::query("SELECT * FROM mcp_servers WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(row_to_server))
    }

    pub async fn list(&self) -> Result<Vec<McpServerDef>, StorageError> {
        let rows = sqlx::query("SELECT * FROM mcp_servers ORDER BY created_at")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(row_to_server).collect())
    }

    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), StorageError> {
        sqlx::query("UPDATE mcp_servers SET enabled = ? WHERE id = ?")
            .bind(enabled as i64)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete(&self, id: &str) -> Result<(), StorageError> {
        sqlx::query("DELETE FROM mcp_servers WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 写入/撤回用户为某工具声明的注解（ADR-002 Gate 的例外口）。
    ///
    /// 键不含 workspace：注解陈述的是工具行为，跨工作区恒成立（见迁移 0025）。
    /// `None` = 撤回，直接删行——"没有声明"和"声明了个空的"必须是同一个状态。
    pub async fn set_tool_annotations(
        &self,
        server_id: &str,
        original_name: &str,
        annotations: Option<mcpane_hub_protocol::DeclaredAnnotations>,
    ) -> Result<(), StorageError> {
        let Some(annotations) = annotations else {
            sqlx::query(
                "DELETE FROM tool_declared_annotations
                 WHERE server_id = ? AND original_name = ?",
            )
            .bind(server_id)
            .bind(original_name)
            .execute(&self.pool)
            .await?;
            return Ok(());
        };
        // 四个 bool 的结构体序列化不可能失败；兜底串取规范默认值（最保守的
        // 那一组），不让一个理论分支变成静默写入空串。同 servers.rs 上方
        // args/env 的写法。
        let payload = serde_json::to_string(&annotations).unwrap_or_else(|_| {
            r#"{"readOnlyHint":false,"destructiveHint":true,"idempotentHint":false,"openWorldHint":true}"#
                .into()
        });
        sqlx::query(
            "INSERT INTO tool_declared_annotations (server_id, original_name, annotations)
             VALUES (?, ?, ?)
             ON CONFLICT(server_id, original_name)
             DO UPDATE SET annotations = excluded.annotations",
        )
        .bind(server_id)
        .bind(original_name)
        .bind(payload)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 全部声明，键为 `(server_id, original_name)`。
    ///
    /// 单条 JSON 坏掉只丢这一条并 warn，不把整次读取拖成 Err——上层据此判定
    /// "读取失败、跳过本轮重建"，一个手改坏的字段会冻住所有工作区的 Catalog。
    pub async fn tool_annotations(
        &self,
    ) -> Result<
        std::collections::BTreeMap<(String, String), mcpane_hub_protocol::DeclaredAnnotations>,
        StorageError,
    > {
        let rows = sqlx::query(
            "SELECT server_id, original_name, annotations FROM tool_declared_annotations",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                let server_id: String = r.get("server_id");
                let original_name: String = r.get("original_name");
                let raw: String = r.get("annotations");
                match serde_json::from_str(&raw) {
                    Ok(value) => Some(((server_id, original_name), value)),
                    Err(error) => {
                        tracing::warn!(
                            %error, server_id, original_name,
                            "declared annotations 解析失败，按未声明处理"
                        );
                        None
                    }
                }
            })
            .collect())
    }
}

fn row_to_server(row: &sqlx::sqlite::SqliteRow) -> McpServerDef {
    let transport: String = row.get("transport");
    McpServerDef {
        id: row.get("id"),
        slug: row.get("slug"),
        name: row.get("name"),
        transport: match transport.as_str() {
            "stdio" => ServerTransport::Stdio,
            "sse" => ServerTransport::Sse,
            _ => ServerTransport::Http,
        },
        command: row.get("command"),
        args: serde_json::from_str(row.get::<String, _>("args").as_str()).unwrap_or_default(),
        env: serde_json::from_str(row.get::<String, _>("env").as_str()).unwrap_or_default(),
        cwd: row.get("cwd"),
        url: row.get("url"),
        bearer: row.get("bearer"),
        bearer_env: row.get("bearer_env"),
        headers: serde_json::from_str(row.get::<String, _>("headers").as_str()).unwrap_or_default(),
        runtime_policy: row.get("runtime_policy"),
        enabled: row.get::<i64, _>("enabled") == 1,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::open_in_memory;

    fn declared() -> mcpane_hub_protocol::DeclaredAnnotations {
        mcpane_hub_protocol::DeclaredAnnotations {
            read_only_hint: true,
            destructive_hint: false,
            idempotent_hint: true,
            open_world_hint: false,
        }
    }

    /// 声明的键里没有 workspace：标一次，处处生效。
    ///
    /// 这条正是把注解从 `workspace_tools` 挪出来的理由——挂在工作区上的话，
    /// 用户新建一个工作区，同一个工具就又变回"无注解"、又调不动了，得重标
    /// 一遍；更糟的是同一个工具能在两个工作区里被声明成互相矛盾的两种行为。
    #[tokio::test]
    async fn declarations_are_global_not_per_workspace() {
        let pool = open_in_memory().await.unwrap();
        let repo = ServerRepo::new(pool.clone());
        repo.set_tool_annotations("srv_a", "search", Some(declared()))
            .await
            .unwrap();

        // 建一个新工作区并在其中禁用别的工具——工作区级治理动它不得。
        let workspaces = crate::WorkspaceRepo::new(pool);
        let ws = workspaces.create("新项目").await.unwrap();
        workspaces
            .set_tool_override(
                &ws.id,
                &crate::ToolOverride {
                    server_id: "srv_a".into(),
                    original_name: "search".into(),
                    enabled: false,
                    alias: Some("gh_search".into()),
                    description_suffix: None,
                },
            )
            .await
            .unwrap();

        let all = repo.tool_annotations().await.unwrap();
        assert_eq!(
            all.get(&("srv_a".to_owned(), "search".to_owned())),
            Some(&declared()),
            "声明不随工作区走，也不该被工作区级的启停/别名影响"
        );
        assert_eq!(all.len(), 1, "只有一条声明，不会按工作区复制出多条");
    }

    /// 撤回是删行，不是写一个空声明——"没声明过"与"撤回了"必须是同一个状态。
    #[tokio::test]
    async fn withdrawing_a_declaration_removes_the_row() {
        let pool = open_in_memory().await.unwrap();
        let repo = ServerRepo::new(pool);
        repo.set_tool_annotations("srv_a", "search", Some(declared()))
            .await
            .unwrap();
        repo.set_tool_annotations("srv_a", "search", None)
            .await
            .unwrap();
        assert!(repo.tool_annotations().await.unwrap().is_empty());
    }

    /// 库里单条 JSON 坏掉只丢这一条，不能把整次读取拖成 Err——上层据此判定
    /// "读取失败、跳过本轮重建"，一个坏字段会冻住所有工作区的 Catalog。
    #[tokio::test]
    async fn corrupt_declaration_degrades_to_unset_without_losing_the_rest() {
        let pool = open_in_memory().await.unwrap();
        let repo = ServerRepo::new(pool.clone());
        repo.set_tool_annotations("srv_a", "good", Some(declared()))
            .await
            .unwrap();
        repo.set_tool_annotations("srv_a", "bad", Some(declared()))
            .await
            .unwrap();
        sqlx::query("UPDATE tool_declared_annotations SET annotations = ? WHERE original_name = ?")
            .bind("{ 这不是 JSON")
            .bind("bad")
            .execute(&pool)
            .await
            .unwrap();

        let all = repo.tool_annotations().await.unwrap();
        assert_eq!(all.len(), 1, "坏的那条丢掉，好的那条必须还在");
        assert!(all.contains_key(&("srv_a".to_owned(), "good".to_owned())));
    }

    #[tokio::test]
    async fn upsert_get_roundtrip_preserves_env_and_cwd() {
        let pool = open_in_memory().await.unwrap();
        let repo = ServerRepo::new(pool);

        let mut env = std::collections::BTreeMap::new();
        env.insert("API_KEY".to_owned(), "secret".to_owned());
        let def = repo
            .upsert(McpServerDef {
                id: String::new(),
                slug: "demo".into(),
                name: "Demo".into(),
                transport: ServerTransport::Stdio,
                command: Some("npx".into()),
                args: vec!["-y".into(), "demo-mcp".into()],
                env,
                cwd: Some("/tmp/project".into()),
                url: None,
                bearer: None,
                bearer_env: None,
                headers: std::collections::BTreeMap::new(),
                runtime_policy: "per-client-connection".into(),
                enabled: true,
            })
            .await
            .unwrap();

        let fetched = repo.get(&def.id).await.unwrap().unwrap();
        assert_eq!(fetched.slug, "demo");
        assert_eq!(
            fetched.env.get("API_KEY").map(String::as_str),
            Some("secret")
        );
        assert_eq!(fetched.cwd.as_deref(), Some("/tmp/project"));

        // 编辑路径：带 id 再次 upsert 是更新而非新增。
        let updated = repo
            .upsert(McpServerDef {
                name: "Demo Pro".into(),
                enabled: false,
                ..fetched
            })
            .await
            .unwrap();
        assert_eq!(updated.id, def.id);
        let all = repo.list().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "Demo Pro");
        assert!(!all[0].enabled);

        assert!(repo.get("srv_missing").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn http_auth_fields_roundtrip() {
        let pool = open_in_memory().await.unwrap();
        let repo = ServerRepo::new(pool);

        let mut headers = std::collections::BTreeMap::new();
        headers.insert("X-API-Key".to_owned(), "key-123".to_owned());
        let def = repo
            .upsert(McpServerDef {
                id: String::new(),
                slug: "remote".into(),
                name: "Remote".into(),
                transport: ServerTransport::Http,
                command: None,
                args: vec![],
                env: std::collections::BTreeMap::new(),
                cwd: None,
                url: Some("https://example.com/mcp".into()),
                bearer: Some("tok-secret".into()),
                bearer_env: Some("MY_TOKEN".into()),
                headers,
                runtime_policy: "shared".into(),
                enabled: true,
            })
            .await
            .unwrap();

        let fetched = repo.get(&def.id).await.unwrap().unwrap();
        assert_eq!(fetched.bearer.as_deref(), Some("tok-secret"));
        assert_eq!(fetched.bearer_env.as_deref(), Some("MY_TOKEN"));
        assert_eq!(
            fetched.headers.get("X-API-Key").map(String::as_str),
            Some("key-123")
        );
    }

    /// SSE 传输往返：0007 放宽 CHECK 后 'sse' 可入库，读回不得被
    /// 兜底分支误判成 Http（row_to_server 曾是二分逻辑）。
    #[tokio::test]
    async fn sse_transport_roundtrip() {
        let pool = open_in_memory().await.unwrap();
        let repo = ServerRepo::new(pool);

        let def = repo
            .upsert(McpServerDef {
                id: String::new(),
                slug: "legacy-sse".into(),
                name: "Legacy SSE".into(),
                transport: ServerTransport::Sse,
                command: None,
                args: vec![],
                env: std::collections::BTreeMap::new(),
                cwd: None,
                url: Some("https://example.com/sse".into()),
                bearer: None,
                bearer_env: Some("SSE_TOKEN".into()),
                headers: std::collections::BTreeMap::new(),
                runtime_policy: "shared".into(),
                enabled: true,
            })
            .await
            .unwrap();

        let fetched = repo.get(&def.id).await.unwrap().unwrap();
        assert_eq!(fetched.transport, ServerTransport::Sse);
        assert_eq!(fetched.url.as_deref(), Some("https://example.com/sse"));
    }
}
