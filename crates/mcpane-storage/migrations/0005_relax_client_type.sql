-- 放宽 client_connections.client_type：原 CHECK 只允许 ('claude','codex','custom')，
-- 而 Hub 与桌面端支持 cursor/vscode/cline/devin——这些客户端接入发 Token 时 INSERT
-- 被 CHECK 拒绝。类型合法性由应用层收敛（auth.rs ClientType::parse 未知值归 custom），
-- 表层不再枚举，避免每新增客户端类型都要重建表。
--
-- SQLite 无法修改 CHECK，只能重建。注意两个陷阱：
-- 1. connection_server_denials 以 ON DELETE CASCADE 引用本表，直接 DROP 父表会级联清空；
-- 2. RENAME 只在 foreign_keys=ON 的连接上才改写其他表的 REFERENCES 子句。
-- 因此子表先备份再删除，等父表拿到最终名后按最终名重建——不依赖任何 pragma 状态。

CREATE TABLE client_connections_new (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    client_type   TEXT NOT NULL,
    token_hash    TEXT NOT NULL,
    token_prefix  TEXT NOT NULL,
    default_workspace_id TEXT,
    enabled       INTEGER NOT NULL DEFAULT 1,
    created_at    INTEGER NOT NULL,
    last_used_at  INTEGER
);
INSERT INTO client_connections_new
    SELECT id, name, client_type, token_hash, token_prefix,
           default_workspace_id, enabled, created_at, last_used_at
    FROM client_connections;

CREATE TABLE connection_server_denials_backup (
    client_connection_id TEXT NOT NULL,
    server_id            TEXT NOT NULL
);
INSERT INTO connection_server_denials_backup
    SELECT client_connection_id, server_id FROM connection_server_denials;

DROP TABLE connection_server_denials;
DROP TABLE client_connections;
ALTER TABLE client_connections_new RENAME TO client_connections;

CREATE TABLE connection_server_denials (
    client_connection_id TEXT NOT NULL REFERENCES client_connections(id) ON DELETE CASCADE,
    server_id            TEXT NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,
    PRIMARY KEY (client_connection_id, server_id)
);
INSERT INTO connection_server_denials
    SELECT client_connection_id, server_id FROM connection_server_denials_backup;
DROP TABLE connection_server_denials_backup;

CREATE UNIQUE INDEX idx_client_connections_token_hash ON client_connections(token_hash);
