-- 增量同步游标补记文件字节数：Codex 全程持有 rollout 追加句柄，Windows NTFS 上
-- 文件增长而 mtime 不动，只看 mtime 会把活跃会话永久跳过。0 = 升级前的旧行，
-- 解析器对其退回只看 mtime，避免升级后全量重扫。
ALTER TABLE usage_sync_state ADD COLUMN last_size INTEGER NOT NULL DEFAULT 0;
