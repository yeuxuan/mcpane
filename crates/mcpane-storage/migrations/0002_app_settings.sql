-- 应用级偏好设置（单行表）。当前仅 hub_enabled；后续新增偏好项直接加列。
CREATE TABLE app_settings (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    hub_enabled INTEGER NOT NULL DEFAULT 1
);
INSERT INTO app_settings (id, hub_enabled) VALUES (1, 1);
