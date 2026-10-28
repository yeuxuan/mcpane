-- 自动更新的启动静默检查开关（TASK-703 子项）。默认开启：面向大众的
-- 桌面应用，安全修复要能触达不看设置页的用户；不想被打扰的用户可关。
ALTER TABLE app_settings ADD COLUMN auto_update_check INTEGER NOT NULL DEFAULT 1;
