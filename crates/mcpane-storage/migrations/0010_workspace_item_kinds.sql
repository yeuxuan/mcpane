-- workspace_tools 扩展为三类条目（tool / prompt / resource）的统一治理表：
-- prompts/resources 聚合后同样需要按 Workspace 启停（prompt 另有 alias）。
-- kind 进主键防止同名 tool 与 prompt 的开关互串；不加 CHECK（应用层收敛，
-- 与 0005/0007 同口径）。
--
-- 主键改动需重建表。本表无子表引用（只引用 workspaces，重建自身无
-- CASCADE 陷阱），直接 new→copy→drop→rename；存量行 kind 取默认 'tool'。
CREATE TABLE workspace_tools_new (
    workspace_id  TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    server_id     TEXT NOT NULL,
    kind          TEXT NOT NULL DEFAULT 'tool',
    original_name TEXT NOT NULL,
    enabled       INTEGER NOT NULL DEFAULT 1,
    alias         TEXT,
    PRIMARY KEY (workspace_id, server_id, kind, original_name)
);
INSERT INTO workspace_tools_new (workspace_id, server_id, original_name, enabled, alias)
    SELECT workspace_id, server_id, original_name, enabled, alias FROM workspace_tools;
DROP TABLE workspace_tools;
ALTER TABLE workspace_tools_new RENAME TO workspace_tools;
