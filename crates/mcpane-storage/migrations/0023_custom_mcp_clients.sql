-- 用户手动添加的 MCP 客户端（MCPane 尚未内置的那些）。
--
-- 只存"这个客户端存在、叫什么"。它没有配置文件形态可言——接入方式是用户拿走
-- 配置片段自己粘进去，MCPane 不写它的配置文件，因此这里既无 config_path 也无
-- token_env。
--
-- id 同时是 `connections.client_type` 的取值：凭证发放与访问控制本来就按这个
-- 字符串工作（0005 迁移已解除 client_type 的 CHECK 枚举约束），自定义客户端
-- 因此不需要在授权链路上新增任何东西。
CREATE TABLE custom_mcp_clients (
    id TEXT PRIMARY KEY,
    display TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
