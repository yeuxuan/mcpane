-- 用户为下游工具**自己声明**的 ToolAnnotations（ADR-002 Gate 的显式例外口）。
--
-- 键是 (server_id, original_name) 而**不含 workspace_id**：注解是对工具行为的
-- 陈述（"这个工具只读"），不是按项目分的策略。挂在 workspace 上会有两个坏处
-- ——换个工作区就要重标一遍，且同一个工具能在两个工作区里被声明成互相矛盾的
-- 两种行为；一个以"谁说的要分得清"为前提的功能，不该允许用户自相矛盾。
--
-- 独立成表而不是给 workspace_tools 加列，正是因为这个键不同。附带的好处：
-- 启停/别名与注解从此在物理上就不可能互相覆盖。
--
-- server_id 不加外键：workspace_tools.server_id 也没加，理由见 0007 注释
-- （mcp_servers 被 CHECK 逼着重建过两次，每多一张 CASCADE 子表就多一个
-- "重建父表先备份子表"的陷阱）。删 Server 后残留的行无害——键再也匹配不上。
--
-- annotations 为 NOT NULL：撤回声明是 DELETE 整行，不是把它置空。
CREATE TABLE tool_declared_annotations (
    server_id     TEXT NOT NULL,
    original_name TEXT NOT NULL,
    annotations   TEXT NOT NULL,
    PRIMARY KEY (server_id, original_name)
);
