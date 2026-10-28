-- 开机自启默认开启的一次性播种标记。
--
-- 开关的事实源仍是操作系统（LaunchAgent / 注册表 / .desktop），不能把
-- 「当前开没开」入库——用户在系统设置里关掉之后，库里的 bool 会撒谎。
-- 这里只记「有没有向系统推过默认开启」：没推过就推一次，推过了就不再碰，
-- 避免用户关掉后又被每次启动复活。
--
-- DEFAULT 0：新装与升级上来的存量库都视为尚未播种，首次启动会写入登录项。
ALTER TABLE app_settings ADD COLUMN autostart_seeded INTEGER NOT NULL DEFAULT 0;
