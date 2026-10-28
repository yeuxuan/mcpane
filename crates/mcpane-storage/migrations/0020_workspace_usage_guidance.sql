-- 工作区级使用准则：用户自定义的工具路由规则文本，Hub 注入 instructions
-- 让所有客户端共享（ADR-020 主动性轴）。空串 = 未配置。
ALTER TABLE workspaces ADD COLUMN usage_guidance TEXT NOT NULL DEFAULT '';
