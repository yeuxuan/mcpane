-- 客户端访问控制（deny-list）：无行 = 允许，新增 Server 默认对所有连接可见。
CREATE TABLE connection_server_denials (
    client_connection_id TEXT NOT NULL REFERENCES client_connections(id) ON DELETE CASCADE,
    server_id            TEXT NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,
    PRIMARY KEY (client_connection_id, server_id)
);
