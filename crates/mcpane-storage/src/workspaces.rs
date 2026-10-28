//! Workspace 与 Server/Tool 开关（TASK-406 数据面）。

use sqlx::Row;

use crate::{SqlitePool, StorageError, new_id, now_unix};

/// 默认工作区 ID（hub 侧常量的镜像；本表用它承载默认工作区的设置幽灵行）。
pub const DEFAULT_WORKSPACE_ID: &str = "ws_default";

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub project_id: Option<String>,
    /// 工具动态发现模式（tools/list 只暴露 meta 工具，按需检索）。
    pub tool_discovery: bool,
    /// 用户自定义使用准则（注入 instructions，空串 = 未配置）。
    pub usage_guidance: String,
}

/// 治理条目类别（workspace_tools.kind，0010 迁移后三类共用一张表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Tool,
    Prompt,
    /// Resource 以 URI 作 original_name；无 alias 语义（URI 不改写）。
    Resource,
}

impl ItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Prompt => "prompt",
            Self::Resource => "resource",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "tool" => Some(Self::Tool),
            "prompt" => Some(Self::Prompt),
            "resource" => Some(Self::Resource),
            _ => None,
        }
    }
}

/// Workspace 内的条目级覆盖（tool/prompt/resource 三类同构；
/// resource 的 original_name 为 URI 且 alias 恒为 None）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolOverride {
    pub server_id: String,
    pub original_name: String,
    pub enabled: bool,
    pub alias: Option<String>,
    /// 追加到下游描述之后的触发式提示（0024）。None/空串 = 未配置。
    pub description_suffix: Option<String>,
}

pub struct WorkspaceRepo {
    pool: SqlitePool,
}

impl WorkspaceRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn create(&self, name: &str) -> Result<Workspace, StorageError> {
        let workspace = Workspace {
            id: new_id("ws"),
            name: name.to_owned(),
            project_id: None,
            tool_discovery: false,
            usage_guidance: String::new(),
        };
        sqlx::query("INSERT INTO workspaces (id, name, created_at) VALUES (?, ?, ?)")
            .bind(&workspace.id)
            .bind(&workspace.name)
            .bind(now_unix())
            .execute(&self.pool)
            .await?;
        Ok(workspace)
    }

    /// 自建工作区列表（排除默认工作区的设置幽灵行，见 0008 迁移注释）。
    pub async fn list(&self) -> Result<Vec<Workspace>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, name, project_id, tool_discovery, usage_guidance FROM workspaces
             WHERE id != ? ORDER BY created_at",
        )
        .bind(DEFAULT_WORKSPACE_ID)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| Workspace {
                id: r.get("id"),
                name: r.get("name"),
                project_id: r.get("project_id"),
                tool_discovery: r.get::<i64, _>("tool_discovery") == 1,
                usage_guidance: r.get("usage_guidance"),
            })
            .collect())
    }

    /// 单个 Workspace 的工具发现开关（无行 = 关闭；默认工作区无行属常态）。
    pub async fn tool_discovery(&self, workspace_id: &str) -> Result<bool, StorageError> {
        let row = sqlx::query("SELECT tool_discovery FROM workspaces WHERE id = ?")
            .bind(workspace_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some_and(|r| r.get::<i64, _>("tool_discovery") == 1))
    }

    /// 单个 Workspace 的使用准则（无行 = 空串；默认工作区走 0011 种子行）。
    pub async fn usage_guidance(&self, workspace_id: &str) -> Result<String, StorageError> {
        let row = sqlx::query("SELECT usage_guidance FROM workspaces WHERE id = ?")
            .bind(workspace_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get("usage_guidance")).unwrap_or_default())
    }

    /// 设置使用准则。纯 UPDATE + NotFound，理由同 set_tool_discovery。
    pub async fn set_usage_guidance(
        &self,
        workspace_id: &str,
        guidance: &str,
    ) -> Result<(), StorageError> {
        let result = sqlx::query("UPDATE workspaces SET usage_guidance = ? WHERE id = ?")
            .bind(guidance)
            .bind(workspace_id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(StorageError::NotFound(workspace_id.to_owned()));
        }
        Ok(())
    }

    /// 设置工具发现开关。
    ///
    /// 纯 UPDATE 而非 UPSERT：默认工作区自 0011 起有种子行，UPSERT 的 INSERT
    /// 分支对它永远不触发，唯一还能命中的是「id 不存在」——那会插出一行名为
    /// `Default` 的幽灵工作区（已删除的 id、拼错的 id 都会），静默污染工作区
    /// 列表。不存在即报 NotFound，与 `ConnectionRepo::rotate_token` 同策略。
    pub async fn set_tool_discovery(
        &self,
        workspace_id: &str,
        enabled: bool,
    ) -> Result<(), StorageError> {
        let result = sqlx::query("UPDATE workspaces SET tool_discovery = ? WHERE id = ?")
            .bind(enabled as i64)
            .bind(workspace_id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(StorageError::NotFound(workspace_id.to_owned()));
        }
        Ok(())
    }

    /// 开关 Workspace 内的 Server。
    pub async fn set_server_enabled(
        &self,
        workspace_id: &str,
        server_id: &str,
        enabled: bool,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO workspace_servers (workspace_id, server_id, enabled) VALUES (?, ?, ?)
             ON CONFLICT(workspace_id, server_id) DO UPDATE SET enabled = excluded.enabled",
        )
        .bind(workspace_id)
        .bind(server_id)
        .bind(enabled as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Workspace 启用的 Server ID 列表。
    pub async fn enabled_servers(&self, workspace_id: &str) -> Result<Vec<String>, StorageError> {
        let rows = sqlx::query(
            "SELECT server_id FROM workspace_servers WHERE workspace_id = ? AND enabled = 1",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| r.get("server_id")).collect())
    }

    /// 开关/别名 Workspace 内的 Tool（兼容旧调用面；等价 kind=Tool）。
    pub async fn set_tool_override(
        &self,
        workspace_id: &str,
        item: &ToolOverride,
    ) -> Result<(), StorageError> {
        self.set_item_override(workspace_id, ItemKind::Tool, item)
            .await
    }

    pub async fn tool_overrides(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<ToolOverride>, StorageError> {
        self.item_overrides(workspace_id, ItemKind::Tool).await
    }

    /// 开关/别名/触发提示 Workspace 内的条目（三类通用）。alias 冲突由 Catalog 层拒绝。
    ///
    /// **`alias` 与 `description_suffix` 为 `None` 表示「不改动」，不是「清空」**：
    /// 调用方各自只关心一个字段（服务器弹窗切开关、工作区页存提示），要求每个
    /// 调用方都记得把其余字段原样带上，迟早会漏——漏一次就是用户配好的东西
    /// 被静默抹掉。清空走空串。
    pub async fn set_item_override(
        &self,
        workspace_id: &str,
        kind: ItemKind,
        item: &ToolOverride,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO workspace_tools
             (workspace_id, server_id, kind, original_name, enabled, alias, description_suffix)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(workspace_id, server_id, kind, original_name)
             DO UPDATE SET enabled = excluded.enabled,
                           alias = COALESCE(excluded.alias, workspace_tools.alias),
                           description_suffix = COALESCE(
                               excluded.description_suffix,
                               workspace_tools.description_suffix
                           )",
        )
        .bind(workspace_id)
        .bind(&item.server_id)
        .bind(kind.as_str())
        .bind(&item.original_name)
        .bind(item.enabled as i64)
        .bind(&item.alias)
        .bind(&item.description_suffix)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn item_overrides(
        &self,
        workspace_id: &str,
        kind: ItemKind,
    ) -> Result<Vec<ToolOverride>, StorageError> {
        let rows = sqlx::query(
            "SELECT server_id, original_name, enabled, alias, description_suffix
             FROM workspace_tools WHERE workspace_id = ? AND kind = ?",
        )
        .bind(workspace_id)
        .bind(kind.as_str())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| ToolOverride {
                server_id: r.get("server_id"),
                original_name: r.get("original_name"),
                enabled: r.get::<i64, _>("enabled") == 1,
                alias: r.get("alias"),
                description_suffix: r.get("description_suffix"),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::open_in_memory;

    /// description_suffix 的落库往返：ON CONFLICT 分支也得带上它，
    /// 否则「先设别名再写提示」会把提示冲掉。
    #[tokio::test]
    async fn description_suffix_round_trips_through_upsert() {
        let pool = open_in_memory().await.unwrap();
        let repo = WorkspaceRepo::new(pool);
        let item = |suffix: Option<&str>| ToolOverride {
            server_id: "srv_a".into(),
            original_name: "search".into(),
            enabled: true,
            alias: Some("gh_search".into()),
            description_suffix: suffix.map(str::to_owned),
        };

        repo.set_tool_override(DEFAULT_WORKSPACE_ID, &item(Some("Use when: 搜仓库")))
            .await
            .unwrap();
        let rows = repo.tool_overrides(DEFAULT_WORKSPACE_ID).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].description_suffix.as_deref(),
            Some("Use when: 搜仓库")
        );
        assert_eq!(
            rows[0].alias.as_deref(),
            Some("gh_search"),
            "别名不该被冲掉"
        );

        // 覆盖写：同一主键再写一次，新值必须落地（走 ON CONFLICT 分支）。
        repo.set_tool_override(DEFAULT_WORKSPACE_ID, &item(Some("改过的提示")))
            .await
            .unwrap();
        let rows = repo.tool_overrides(DEFAULT_WORKSPACE_ID).await.unwrap();
        assert_eq!(rows[0].description_suffix.as_deref(), Some("改过的提示"));

        // None = 不改动。服务器弹窗切工具开关时不带这个字段，若按「写 NULL」
        // 处理，用户在工作区页配好的提示会被静默抹掉。
        repo.set_tool_override(DEFAULT_WORKSPACE_ID, &item(None))
            .await
            .unwrap();
        let rows = repo.tool_overrides(DEFAULT_WORKSPACE_ID).await.unwrap();
        assert_eq!(
            rows[0].description_suffix.as_deref(),
            Some("改过的提示"),
            "None 是不改动，不是清空"
        );

        // 清空走空串。
        repo.set_tool_override(DEFAULT_WORKSPACE_ID, &item(Some("")))
            .await
            .unwrap();
        let rows = repo.tool_overrides(DEFAULT_WORKSPACE_ID).await.unwrap();
        assert_eq!(rows[0].description_suffix.as_deref(), Some(""));
    }

    /// alias 同理：工作区页保存触发提示时不会带上别名，按「写 NULL」处理
    /// 就会把用户设好的别名冲掉。
    #[tokio::test]
    async fn omitted_fields_do_not_wipe_each_other() {
        let pool = open_in_memory().await.unwrap();
        let repo = WorkspaceRepo::new(pool);
        let base = |alias: Option<&str>, suffix: Option<&str>| ToolOverride {
            server_id: "srv_a".into(),
            original_name: "search".into(),
            enabled: true,
            alias: alias.map(str::to_owned),
            description_suffix: suffix.map(str::to_owned),
        };

        // 先设别名，再单独存提示（不带别名）——别名必须还在。
        repo.set_tool_override(DEFAULT_WORKSPACE_ID, &base(Some("gh_search"), None))
            .await
            .unwrap();
        repo.set_tool_override(DEFAULT_WORKSPACE_ID, &base(None, Some("Use when: X")))
            .await
            .unwrap();
        let rows = repo.tool_overrides(DEFAULT_WORKSPACE_ID).await.unwrap();
        assert_eq!(
            rows[0].alias.as_deref(),
            Some("gh_search"),
            "别名被提示写入冲掉了"
        );
        assert_eq!(rows[0].description_suffix.as_deref(), Some("Use when: X"));

        // 反向：只切开关，两个字段都得留着。
        repo.set_tool_override(
            DEFAULT_WORKSPACE_ID,
            &ToolOverride {
                enabled: false,
                ..base(None, None)
            },
        )
        .await
        .unwrap();
        let rows = repo.tool_overrides(DEFAULT_WORKSPACE_ID).await.unwrap();
        assert!(!rows[0].enabled);
        assert_eq!(rows[0].alias.as_deref(), Some("gh_search"));
        assert_eq!(rows[0].description_suffix.as_deref(), Some("Use when: X"));
    }

    /// kind 隔离：同名 tool 与 prompt 的覆盖互不串扰；resource 以 URI 为键。
    /// 特意用 ws_default：它在库里靠 0011 种子行满足外键——此前默认工作区的
    /// 工具禁用会被 FOREIGN KEY 拒绝（本测试同时是该修复的回归锁）。
    #[tokio::test]
    async fn item_overrides_are_isolated_by_kind() {
        let pool = open_in_memory().await.unwrap();
        let repo = WorkspaceRepo::new(pool);

        let item = |name: &str, enabled: bool, alias: Option<&str>| ToolOverride {
            server_id: "srv_a".into(),
            original_name: name.into(),
            enabled,
            alias: alias.map(str::to_owned),
            description_suffix: None,
        };
        // 同名 "review"：tool 禁用、prompt 启用带 alias——0010 之前会互相覆盖。
        repo.set_item_override(
            DEFAULT_WORKSPACE_ID,
            ItemKind::Tool,
            &item("review", false, None),
        )
        .await
        .unwrap();
        repo.set_item_override(
            DEFAULT_WORKSPACE_ID,
            ItemKind::Prompt,
            &item("review", true, Some("cr")),
        )
        .await
        .unwrap();
        repo.set_item_override(
            DEFAULT_WORKSPACE_ID,
            ItemKind::Resource,
            &item("mem://a", false, None),
        )
        .await
        .unwrap();

        let tools = repo
            .item_overrides(DEFAULT_WORKSPACE_ID, ItemKind::Tool)
            .await
            .unwrap();
        assert_eq!(tools.len(), 1);
        assert!(!tools[0].enabled, "tool 覆盖不被 prompt 写入串改");

        let prompts = repo
            .item_overrides(DEFAULT_WORKSPACE_ID, ItemKind::Prompt)
            .await
            .unwrap();
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].enabled);
        assert_eq!(prompts[0].alias.as_deref(), Some("cr"));

        let resources = repo
            .item_overrides(DEFAULT_WORKSPACE_ID, ItemKind::Resource)
            .await
            .unwrap();
        assert_eq!(resources[0].original_name, "mem://a");

        // 旧 tool API 与 kind=Tool 完全等价（兼容面）。
        assert_eq!(
            repo.tool_overrides(DEFAULT_WORKSPACE_ID)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    /// 默认工作区（库里无行）与自建工作区的发现开关都能存取；
    /// 幽灵行不得泄漏进自建工作区列表。
    #[tokio::test]
    async fn tool_discovery_flag_roundtrip_including_default_workspace() {
        let pool = open_in_memory().await.unwrap();
        let repo = WorkspaceRepo::new(pool);

        // 无行 = 关闭（默认工作区常态）。
        assert!(!repo.tool_discovery(DEFAULT_WORKSPACE_ID).await.unwrap());
        assert!(!repo.tool_discovery("ws_missing").await.unwrap());

        // 默认工作区：0011 的种子行承载开关。
        repo.set_tool_discovery(DEFAULT_WORKSPACE_ID, true)
            .await
            .unwrap();
        assert!(repo.tool_discovery(DEFAULT_WORKSPACE_ID).await.unwrap());
        assert!(
            repo.list().await.unwrap().is_empty(),
            "种子行不得出现在自建工作区列表"
        );

        // 自建工作区：命中已有行，只改开关不动名字。
        let ws = repo.create("研发").await.unwrap();
        assert!(!ws.tool_discovery, "新建默认关闭");
        repo.set_tool_discovery(&ws.id, true).await.unwrap();
        let listed = repo.list().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "研发", "不得改名");
        assert!(listed[0].tool_discovery);

        // 关回。
        repo.set_tool_discovery(&ws.id, false).await.unwrap();
        assert!(!repo.list().await.unwrap()[0].tool_discovery);
    }

    #[tokio::test]
    async fn usage_guidance_roundtrip_including_default_workspace() {
        let pool = open_in_memory().await.unwrap();
        let repo = WorkspaceRepo::new(pool);

        // 无行/未配置 = 空串。
        assert_eq!(repo.usage_guidance(DEFAULT_WORKSPACE_ID).await.unwrap(), "");
        assert_eq!(repo.usage_guidance("ws_missing").await.unwrap(), "");

        repo.set_usage_guidance(DEFAULT_WORKSPACE_ID, "代码搜索一律先用 fast_context_search")
            .await
            .unwrap();
        assert_eq!(
            repo.usage_guidance(DEFAULT_WORKSPACE_ID).await.unwrap(),
            "代码搜索一律先用 fast_context_search"
        );

        let ws = repo.create("研发").await.unwrap();
        assert_eq!(ws.usage_guidance, "", "新建默认为空");
        repo.set_usage_guidance(&ws.id, "准则A").await.unwrap();
        assert_eq!(repo.list().await.unwrap()[0].usage_guidance, "准则A");

        // 不存在的 id 报 NotFound，不插幽灵行（同 set_tool_discovery 策略）。
        assert!(matches!(
            repo.set_usage_guidance("ws_deleted", "x").await,
            Err(StorageError::NotFound(_))
        ));
    }

    /// 回归：不存在的 id 必须报错而不是插出幽灵工作区。
    /// 此前是 UPSERT，已删除或拼错的 id 会静默插进一行名为 `Default` 的工作区，
    /// 并出现在用户的工作区列表里。
    #[tokio::test]
    async fn tool_discovery_on_unknown_workspace_errors_instead_of_creating_a_ghost() {
        let pool = open_in_memory().await.unwrap();
        let repo = WorkspaceRepo::new(pool);

        let error = repo.set_tool_discovery("ws_deleted", true).await;
        assert!(
            matches!(error, Err(StorageError::NotFound(ref id)) if id == "ws_deleted"),
            "不存在的 Workspace 应报 NotFound，实际：{error:?}"
        );
        assert!(
            repo.list().await.unwrap().is_empty(),
            "不得因一次设置写入而凭空多出一个工作区"
        );
    }
}
