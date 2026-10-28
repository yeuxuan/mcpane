-- 云同步配置（JSON 单列：provider/webdav/s3 表单 + server-owned 状态）。
-- 凭据（WebDAV 密码 / S3 SecretKey）只存系统密钥库引用，绝不落库。
ALTER TABLE app_settings ADD COLUMN sync_config TEXT;
