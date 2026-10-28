# ADR-006：Tool 命名空间

- 状态：Accepted
- 日期：2026-07-11

## 背景

多个下游 MCP Server 常含同名 Tool（如 `search`）。聚合后若不加命名空间，
后注册者会覆盖先注册者，路由歧义不可接受。

## 决策

1. 默认暴露名：`{server_slug}__{original_tool_name}`
   （如 `github__search`、`filesystem__search`）。
2. MCPane 内建 Tool 固定使用 `mcpane__` 前缀（如 `mcpane__echo`）。
3. 规则：
   - 名称规范化后必须符合 MCP Tool name 约束；
   - 超长截断并追加稳定短哈希；
   - 当前 Workspace 内暴露名必须唯一；
   - 允许用户 alias，alias 冲突时拒绝保存；
   - 持久化 `original_name` / `exposed_name` / `server_id` / `schema_hash`；
   - Catalog Revision 绑定 Session 后不得改变路由目标（见 ADR-004）。

## 理由

- 双下划线分隔符可读且与常见 Tool 命名字符集无冲突；
- 稳定短哈希保证重启/重建后的暴露名可复现。

## 后果

- 需要唯一索引 `tool_routes(catalog_revision_id, exposed_name)`；
- 名称规范化逻辑需要 golden tests（TASK-405）。
