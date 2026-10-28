-- 请求日志补 TASK-407 未落地的摘要列：脱敏后的入参/出参 + 错误原文。
-- 完整明文不入库；超长内容截断（见 logs.rs summarize_json）。
ALTER TABLE hub_request_logs ADD COLUMN request_summary TEXT;
ALTER TABLE hub_request_logs ADD COLUMN response_summary TEXT;
ALTER TABLE hub_request_logs ADD COLUMN error_message TEXT;
