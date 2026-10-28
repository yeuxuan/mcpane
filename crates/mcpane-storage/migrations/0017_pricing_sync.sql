-- models.dev 自动定价同步状态（JSON 单列：enabled/lastSyncedAt/lastError，
-- 语义由命令层解释）。NULL = 从未配置，按默认启用解释。
ALTER TABLE app_settings ADD COLUMN pricing_sync TEXT;
