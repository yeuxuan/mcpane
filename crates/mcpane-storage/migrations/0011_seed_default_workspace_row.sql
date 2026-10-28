-- 修复：默认工作区（ws_default）在 workspaces 表无行，而 workspace_tools /
-- workspace_servers 以外键引用 workspaces——默认工作区下的工具禁用/别名
-- 自 0001 起就会被 FOREIGN KEY 约束拒绝（写库失败，UI 报错）。集成测试
-- 均在内存 state 层构造覆盖、未走落库链路，故一直未暴露。
--
-- 种子一条 ws_default 行承载外键与设置（与 0008 幽灵行同一方案的完备化）：
-- workspaces 的 list 查询排除该行，前端自建工作区列表语义不变。
INSERT OR IGNORE INTO workspaces (id, name, created_at, tool_discovery)
VALUES ('ws_default', 'Default', strftime('%s', 'now'), 0);
