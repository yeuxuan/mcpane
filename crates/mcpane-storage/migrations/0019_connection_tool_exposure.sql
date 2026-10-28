-- 客户端连接的工具暴露模式：flat（平铺）/ gateway（网关）/ auto（按客户端类型自适应）。
-- 不加 CHECK：枚举收敛在应用层（同 0005 的理由，避免加模式就要重建表）。
ALTER TABLE client_connections ADD COLUMN tool_exposure_mode TEXT NOT NULL DEFAULT 'auto';
