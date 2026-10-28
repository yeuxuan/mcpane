-- 放宽 mcp_servers.transport：原 CHECK 只允许 ('stdio','http')，新增 SSE 下游
-- （HTTP+SSE legacy 传输）时 INSERT 被 CHECK 拒绝。与 0005 处理 client_type 的
-- 口径一致：合法性由应用层收敛（commands/servers.rs server_upsert 与
-- control.rs test_server 显式枚举），表层不再枚举，避免每新增传输类型都重建表。
--
-- SQLite 无法修改 CHECK，只能重建。陷阱同 0005：
-- 1. workspace_servers / connection_server_denials 以 ON DELETE CASCADE 引用本表，
--    直接 DROP 父表会级联清空；
-- 2. RENAME 只在 foreign_keys=ON 的连接上才改写其他表的 REFERENCES 子句。
-- 因此子表先备份再删除，等父表拿到最终名后按最终名重建——不依赖任何 pragma 状态。
-- （workspace_tools 的 server_id 无外键约束，不受影响。）

CREATE TABLE mcp_servers_new (
    id            TEXT PRIMARY KEY,
    slug          TEXT NOT NULL UNIQUE,
    name          TEXT NOT NULL,
    transport     TEXT NOT NULL,
    -- stdio
    command       TEXT,
    args          TEXT,             -- JSON array
    env           TEXT,             -- JSON object（不含 Secret，Secret 走引用）
    cwd           TEXT,
    -- http / sse
    url           TEXT,
    bearer        TEXT,
    bearer_env    TEXT,             -- Bearer Token 的环境变量名
    headers       TEXT NOT NULL DEFAULT '{}',
    -- runtime
    runtime_policy TEXT NOT NULL DEFAULT 'shared'
        CHECK (runtime_policy IN ('shared', 'per-client-connection', 'per-upstream-session')),
    enabled       INTEGER NOT NULL DEFAULT 1,
    created_at    INTEGER NOT NULL
);
INSERT INTO mcp_servers_new
    (id, slug, name, transport, command, args, env, cwd, url, bearer, bearer_env,
     headers, runtime_policy, enabled, created_at)
    SELECT id, slug, name, transport, command, args, env, cwd, url, bearer, bearer_env,
           headers, runtime_policy, enabled, created_at
    FROM mcp_servers;

CREATE TABLE workspace_servers_backup (
    workspace_id TEXT NOT NULL,
    server_id    TEXT NOT NULL,
    enabled      INTEGER NOT NULL
);
INSERT INTO workspace_servers_backup
    SELECT workspace_id, server_id, enabled FROM workspace_servers;

CREATE TABLE connection_server_denials_backup (
    client_connection_id TEXT NOT NULL,
    server_id            TEXT NOT NULL
);
INSERT INTO connection_server_denials_backup
    SELECT client_connection_id, server_id FROM connection_server_denials;

DROP TABLE workspace_servers;
DROP TABLE connection_server_denials;
DROP TABLE mcp_servers;
ALTER TABLE mcp_servers_new RENAME TO mcp_servers;

CREATE TABLE workspace_servers (
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    server_id    TEXT NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,
    enabled      INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (workspace_id, server_id)
);
INSERT INTO workspace_servers
    SELECT workspace_id, server_id, enabled FROM workspace_servers_backup;
DROP TABLE workspace_servers_backup;

CREATE TABLE connection_server_denials (
    client_connection_id TEXT NOT NULL REFERENCES client_connections(id) ON DELETE CASCADE,
    server_id            TEXT NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,
    PRIMARY KEY (client_connection_id, server_id)
);
INSERT INTO connection_server_denials
    SELECT client_connection_id, server_id FROM connection_server_denials_backup;
DROP TABLE connection_server_denials_backup;
