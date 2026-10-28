-- 全局出网代理（ADR-017）。
-- 跟随 0002 定下的约定「后续新增偏好项直接加列」：ADD COLUMN 不触发表重建，
-- 既有行自动取默认值，无需搬运数据。
--
-- 拆成 mode + url 两列而非塞一个 JSON blob：mode 是封闭枚举，独立成列后
-- 用 sqlite3 直接看得懂、也能被 CHECK 约束住。
-- 默认 'system'：装完就跟随系统与环境变量，与本次改动之前 reqwest 的默认行为
-- 一致，老用户升级上来不会因为多了这张表而改变出网行为。
ALTER TABLE app_settings
    ADD COLUMN proxy_mode TEXT NOT NULL DEFAULT 'system'
    CHECK (proxy_mode IN ('system', 'direct', 'manual'));

-- 仅 manual 模式有意义；其余模式下为 NULL。
ALTER TABLE app_settings ADD COLUMN proxy_url TEXT;
