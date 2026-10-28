-- 定价批次：对齐 cc-switch v3.19.1 上游（9cf4ae4 / f38722a / f42534e，2026-08 复查）。
-- 值逐字复核自其 schema.rs::seed_model_pricing。
-- gpt-5.3-codex-spark 与 gpt-5.6 effort 变体经 normalize_model 归并到既有键，无需新行。

INSERT OR IGNORE INTO model_pricing
    (model_id, display_name, input_per_million, output_per_million, cache_read_per_million, cache_creation_per_million)
VALUES
    ('claude-opus-5',           'Claude Opus 5',           '5',      '25',    '0.50',   '6.25'),
    ('gemini-3.6-flash',        'Gemini 3.6 Flash',        '1.50',   '7.50',  '0.15',   '0'),
    ('gemini-3.5-flash-lite',   'Gemini 3.5 Flash Lite',   '0.30',   '2.50',  '0.03',   '0'),
    ('qwen3.8-max',             'Qwen3.8 Max',             '2',      '6',     '0.25',   '2.50'),
    ('qwen3.6-flash',           'Qwen3.6 Flash',           '0.1875', '1.125', '0.0375', '0'),
    -- HighSpeed 加速档 = 本体 2 倍价（Kimi 官方一贯模式）
    ('kimi-k2.7-code-highspeed','Kimi K2.7 Code HighSpeed','1.90',   '8.00',  '0.38',   '0'),
    ('glm-5-turbo',             'GLM-5-Turbo',             '1.2',    '4',     '0.24',   '0'),
    ('glm-5v-turbo',            'GLM-5V-Turbo',            '1.2',    '4',     '0.24',   '0'),
    -- 腾讯混元 Hy3（官方 CNY 1/4/0.25 按 1 USD ≈ 7.14 折算，阶梯计价取最低档）
    ('hunyuan-hy3',             'Hunyuan Hy3',             '0.14',   '0.56',  '0.035',  '0'),
    ('hy3',                     'Hunyuan Hy3',             '0.14',   '0.56',  '0.035',  '0');

-- OpenAI 2026-07-30 官方降价（luna -80%、terra -20%；cache_write 维持 1.25× 输入价）。
UPDATE model_pricing SET input_per_million='0.20', output_per_million='1.20',
    cache_read_per_million='0.02', cache_creation_per_million='0.25'
    WHERE model_id='gpt-5.6-luna';
UPDATE model_pricing SET input_per_million='2', output_per_million='12',
    cache_read_per_million='0.20', cache_creation_per_million='2.50'
    WHERE model_id='gpt-5.6-terra';

-- deepseek-chat / deepseek-reasoner 自 2026-07 起为 V4 Flash 的 legacy 别名（同价）。
UPDATE model_pricing SET input_per_million='0.14', output_per_million='0.28',
    cache_read_per_million='0.0028'
    WHERE model_id IN ('deepseek-chat', 'deepseek-reasoner');
