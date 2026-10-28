-- HTTP 下游认证补全：Bearer Token 直填 + 自定义请求头。
-- 明文入库与 env 列（stdio API Key）风险面一致：本地单用户 SQLite。
ALTER TABLE mcp_servers ADD COLUMN bearer TEXT;
ALTER TABLE mcp_servers ADD COLUMN headers TEXT NOT NULL DEFAULT '{}';
