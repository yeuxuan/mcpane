# ADR-004：Session-Pinned Workspace

- 状态：Accepted
- 日期：2026-07-11

## 背景

并非所有 MCP 客户端都可靠支持工具列表热刷新（`notifications/tools/list_changed`）。
若 Workspace 切换即时改变会话内的工具名、Schema 或路由目标，客户端可能持有过期
Catalog 并调用到错误的下游。

## 决策

1. P0 采用 **Session-Pinned Workspace**：initialize 时按 Bearer Token →
   ClientConnection → 默认 Workspace 生成稳定 **Catalog Snapshot**，将其
   Revision 固定到该 MCP Session；会话内工具名与 Schema 保持稳定。
2. Workspace 切换默认**对新 Session 生效**；Hub 可 best-effort 发送
   `tools/list_changed`，但不把热刷新作为正确性前提。
3. UI 必须区分"当前会话生效"与"下次连接生效"，并提供重连/重建 Session 操作。
4. Catalog Revision 一旦绑定 Session，不得在该 Session 中悄悄改变路由目标。

## 理由

- 正确性优先：以最保守的客户端能力为基线；
- Snapshot 使路由可审计、可回放（Revision 入库）。

## 后果

- 需要 `catalog_revisions` / `tool_routes` 表与 Snapshot 生成逻辑（TASK-405/406）；
- Workspace 切换体验依赖 UI 明示与 best-effort 通知。
