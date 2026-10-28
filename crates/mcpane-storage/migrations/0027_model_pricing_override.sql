-- 用户对单个模型的价格覆盖（USD / 百万 token，TEXT 存十进制防浮点漂移）。
-- 与 model_pricing 分表：后者会被 models.dev 同步 INSERT OR REPLACE 覆写，覆盖若放在同一张表会被冲掉。
-- 整行生效（四项都取这里的值），成本在读取时 COALESCE(覆盖, 官方价, 0) 计算，历史事件同样适用。
-- 口径是「按模型」：usage_events 没有供应商维度，无法区分同一模型走不同中转站的价差。
CREATE TABLE model_pricing_override (
    model_id TEXT PRIMARY KEY,
    input_per_million TEXT NOT NULL,
    output_per_million TEXT NOT NULL,
    cache_read_per_million TEXT NOT NULL DEFAULT '0',
    cache_creation_per_million TEXT NOT NULL DEFAULT '0'
);
