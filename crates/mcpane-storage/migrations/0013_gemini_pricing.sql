-- Gemini 模型定价（随 Gemini CLI 客户端支持引入；口径同 0012：USD/百万 token，
-- 快照摘自 cc-switch schema.rs::seed_model_pricing）。
-- Gemini 的 input 含 cached（cachedContentTokenCount ⊆ promptTokenCount），
-- billable input 在查询层扣 cache_read（与 Codex 同分支）。
INSERT OR IGNORE INTO model_pricing
    (model_id, display_name, input_per_million, output_per_million, cache_read_per_million, cache_creation_per_million)
VALUES
    ('gemini-3.5-flash',             'Gemini 3.5 Flash',             '1.50', '9.00', '0.15',  '0'),
    ('gemini-3.1-pro-preview',       'Gemini 3.1 Pro Preview',       '2',    '12',   '0.20',  '0'),
    ('gemini-3.1-flash-lite',        'Gemini 3.1 Flash Lite',        '0.25', '1.50', '0.025', '0'),
    ('gemini-3.1-flash-lite-preview','Gemini 3.1 Flash Lite Preview','0.25', '1.50', '0.025', '0'),
    ('gemini-3-pro-preview',         'Gemini 3 Pro Preview',         '2',    '12',   '0.2',   '0'),
    ('gemini-3-flash-preview',       'Gemini 3 Flash Preview',       '0.5',  '3',    '0.05',  '0'),
    ('gemini-2.5-pro',               'Gemini 2.5 Pro',               '1.25', '10',   '0.125', '0'),
    ('gemini-2.5-flash',             'Gemini 2.5 Flash',             '0.3',  '2.5',  '0.03',  '0'),
    ('gemini-2.5-flash-lite',        'Gemini 2.5 Flash Lite',        '0.10', '0.40', '0.01',  '0'),
    ('gemini-2.0-flash',             'Gemini 2.0 Flash',             '0.10', '0.40', '0.025', '0');
