-- MCPane P0 核心表（§6.2 子集：GATE-3 所需）。
-- 其余表（providers 等）随后续迁移加入；Provider 暂为 JSON 存储。

CREATE TABLE client_connections (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    client_type   TEXT NOT NULL CHECK (client_type IN ('claude', 'codex', 'custom')),
    token_hash    TEXT NOT NULL,
    token_prefix  TEXT NOT NULL,
    default_workspace_id TEXT,
    enabled       INTEGER NOT NULL DEFAULT 1,
    created_at    INTEGER NOT NULL,
    last_used_at  INTEGER
);
CREATE UNIQUE INDEX idx_client_connections_token_hash ON client_connections(token_hash);

CREATE TABLE mcp_servers (
    id            TEXT PRIMARY KEY,
    slug          TEXT NOT NULL UNIQUE,
    name          TEXT NOT NULL,
    transport     TEXT NOT NULL CHECK (transport IN ('stdio', 'http')),
    -- stdio
    command       TEXT,
    args          TEXT,             -- JSON array
    env           TEXT,             -- JSON object（不含 Secret，Secret 走引用）
    cwd           TEXT,
    -- http
    url           TEXT,
    bearer_env    TEXT,             -- Bearer Token 的环境变量名
    -- runtime
    runtime_policy TEXT NOT NULL DEFAULT 'shared'
        CHECK (runtime_policy IN ('shared', 'per-client-connection', 'per-upstream-session')),
    enabled       INTEGER NOT NULL DEFAULT 1,
    created_at    INTEGER NOT NULL
);

CREATE TABLE workspaces (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    project_id  TEXT,
    created_at  INTEGER NOT NULL
);

CREATE TABLE workspace_servers (
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    server_id    TEXT NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,
    enabled      INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (workspace_id, server_id)
);

CREATE TABLE workspace_tools (
    workspace_id  TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    server_id     TEXT NOT NULL,
    original_name TEXT NOT NULL,
    enabled       INTEGER NOT NULL DEFAULT 1,
    alias         TEXT,
    PRIMARY KEY (workspace_id, server_id, original_name)
);

CREATE TABLE catalog_revisions (
    id           TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    created_at   INTEGER NOT NULL
);

CREATE TABLE tool_routes (
    catalog_revision_id TEXT NOT NULL REFERENCES catalog_revisions(id) ON DELETE CASCADE,
    exposed_name  TEXT NOT NULL,
    server_id     TEXT NOT NULL,
    original_name TEXT NOT NULL,
    schema_hash   TEXT NOT NULL,
    PRIMARY KEY (catalog_revision_id, exposed_name)
);
CREATE INDEX idx_tool_routes_server ON tool_routes(server_id, original_name);

CREATE TABLE hub_request_logs (
    id            TEXT PRIMARY KEY,
    ts            INTEGER NOT NULL,
    request_id    TEXT NOT NULL,
    client_connection_id TEXT,
    session_id    TEXT,
    workspace_id  TEXT,
    server_id     TEXT,
    exposed_name  TEXT,
    method        TEXT NOT NULL,
    duration_ms   INTEGER NOT NULL,
    success       INTEGER NOT NULL,
    error_category TEXT,
    request_size  INTEGER,
    response_size INTEGER,
    retry_count   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_hub_request_logs_ts ON hub_request_logs(ts);
CREATE INDEX idx_hub_request_logs_conn ON hub_request_logs(client_connection_id, ts);
