-- 定价补录：对齐 cc-switch v3.18.0 上游新增行（2026-07 新模型）。
-- kimi-k3 / 裸名 k3：Kimi For Coding 套餐的 K3（kimi-coding 预设用户产生的
-- model key 即为这两种形态，缺行会 unpriced 记 0 成本）。
-- grok-4.5：xAI 新旗舰（上游 cdf0ee3 官方 list 价）。
INSERT OR IGNORE INTO model_pricing
    (model_id, display_name, input_per_million, output_per_million, cache_read_per_million, cache_creation_per_million)
VALUES
    ('kimi-k3',  'Kimi K3',  '3.00', '15.00', '0.30', '0'),
    ('k3',       'Kimi K3',  '3.00', '15.00', '0.30', '0'),
    ('grok-4.5', 'Grok 4.5', '2',    '6',     '0.50', '0');
