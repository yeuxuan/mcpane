-- Codex 用量解析器版本。解析算法变更（计费口径/request_id 形态）时递增，
-- 同步入口检测到落后即清 Codex 事件与全部游标做一次性重建（历史污染数据
-- 无法原地修正——事件序号与数值都会变）。默认 1 = thread-v1 时代。
ALTER TABLE app_settings ADD COLUMN codex_parser_version INTEGER NOT NULL DEFAULT 1;
