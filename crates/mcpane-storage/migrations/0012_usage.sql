-- 用量/成本统计（对齐 cc-switch 的会话用量解析）。
--
-- 背景：MCPane 无 LLM 代理层，采集不到模型请求流量，token 用量的唯一现实
-- 数据源是解析客户端本地会话文件——Claude `~/.claude/projects/**/*.jsonl`
-- 的 assistant.message.usage，Codex `~/.codex/sessions/**/rollout-*.jsonl`
-- 的 token_count 事件。本表与 hub_request_logs（MCP 工具调用日志）语义完全
-- 不同，独立建表，不复用。

-- 用量事件：一条 = 一次可计费的模型消息（Claude 每消息独立；Codex 每 token_count
-- 事件的相邻快照 delta）。成本不落库——查询时按 model_key JOIN model_pricing
-- 动态估算，定价表更新后历史成本自动重算。
CREATE TABLE usage_events (
    -- 去重主键：Claude "session:<msg_id>"；Codex "codex_session:thread-v1:<tid>:<idx>"。
    -- 配合 INSERT OR IGNORE 防止重复扫描双算。
    request_id TEXT PRIMARY KEY,
    -- 事件秒级时间：Claude 取消息 timestamp（RFC3339 解析），Codex 取事件时间；
    -- 解析失败回落导入时刻。用于趋势/时间窗聚合与 provider 归属关联。
    ts INTEGER NOT NULL,
    client_app TEXT NOT NULL,              -- 'claude' | 'codex'
    -- 原始 model 名（保真展示）。
    model TEXT NOT NULL,
    -- 归一化后的定价匹配键（小写、剥 provider/ 前缀、剥日期/effort 后缀）。
    -- 与 model_pricing.model_id 精确 JOIN；无法归一时等于 model。
    model_key TEXT NOT NULL,
    project TEXT,                          -- Claude 项目目录解码；Codex 为 NULL
    session_id TEXT,                       -- 会话/线程 id
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    -- Codex 无 cache_creation 维度，恒 0。
    cache_creation_tokens INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_usage_events_ts ON usage_events (ts);
CREATE INDEX idx_usage_events_app_model ON usage_events (client_app, model_key);

-- 增量同步游标：每个已解析文件记 mtime（纳秒）+ 已处理行数，下次只解析新增行。
-- mtime 未变则整文件跳过；变了则重扫，靠 last_line_offset 防重复。
CREATE TABLE usage_sync_state (
    file_path TEXT PRIMARY KEY,
    last_modified INTEGER NOT NULL,        -- mtime 纳秒
    last_line_offset INTEGER NOT NULL DEFAULT 0,
    last_synced_at INTEGER NOT NULL
);

-- 模型定价（USD / 百万 token）。字符串存储避免二进制浮点漂移，Rust 侧用 f64 解析。
-- 成本语义（与 cc-switch 一致）：Claude 的 input_tokens 已是 fresh input，直接计费；
-- Codex 的 input_tokens 含 cache，计费前扣除 cache_read + cache_creation（在查询/服务层处理）。
-- model_id 存「归一化后」形式，与 usage_events.model_key 对齐。
CREATE TABLE model_pricing (
    model_id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    input_per_million TEXT NOT NULL,
    output_per_million TEXT NOT NULL,
    cache_read_per_million TEXT NOT NULL DEFAULT '0',
    cache_creation_per_million TEXT NOT NULL DEFAULT '0'
);

-- 定价 seed（快照，摘自 cc-switch schema.rs::seed_model_pricing，取当代主流模型）。
-- 后续新模型可追加或经设置页补录；缺定价的 model 成本记 0 并在 UI 标注「无定价」。
INSERT OR IGNORE INTO model_pricing
    (model_id, display_name, input_per_million, output_per_million, cache_read_per_million, cache_creation_per_million)
VALUES
    -- Claude（model_key 已去日期后缀）
    ('claude-fable-5',    'Claude Fable 5',    '10',   '50',   '1.00', '12.50'),
    ('claude-mythos-5',   'Claude Mythos 5',   '10',   '50',   '1.00', '12.50'),
    ('claude-opus-4-8',   'Claude Opus 4.8',   '5',    '25',   '0.50', '6.25'),
    ('claude-sonnet-5',   'Claude Sonnet 5',   '3',    '15',   '0.30', '3.75'),
    ('claude-opus-4-7',   'Claude Opus 4.7',   '5',    '25',   '0.50', '6.25'),
    ('claude-opus-4-6',   'Claude Opus 4.6',   '5',    '25',   '0.50', '6.25'),
    ('claude-sonnet-4-6', 'Claude Sonnet 4.6', '3',    '15',   '0.30', '3.75'),
    ('claude-opus-4-5',   'Claude Opus 4.5',   '5',    '25',   '0.50', '6.25'),
    ('claude-sonnet-4-5', 'Claude Sonnet 4.5', '3',    '15',   '0.30', '3.75'),
    ('claude-haiku-4-5',  'Claude Haiku 4.5',  '1',    '5',    '0.10', '1.25'),
    ('claude-opus-4',     'Claude Opus 4',     '15',   '75',   '1.50', '18.75'),
    ('claude-opus-4-1',   'Claude Opus 4.1',   '15',   '75',   '1.50', '18.75'),
    ('claude-sonnet-4',   'Claude Sonnet 4',   '3',    '15',   '0.30', '3.75'),
    ('claude-3-5-haiku',  'Claude 3.5 Haiku',  '0.80', '4',    '0.08', '1'),
    ('claude-3-5-sonnet', 'Claude 3.5 Sonnet', '3',    '15',   '0.30', '3.75'),
    -- OpenAI / Codex（model_key 去 -codex 与 effort 后缀）
    -- GPT-5.6 家族有 Sol/Terra/Luna 三档独立定价，normalize 不剥这些后缀；
    -- 裸名 gpt-5.6 是 sol 的官方别名。5.6 家族起 cache write 收 1.25× 输入价。
    ('gpt-5.6',           'GPT-5.6 Sol',       '5',    '30',   '0.50', '6.25'),
    ('gpt-5.6-sol',       'GPT-5.6 Sol',       '5',    '30',   '0.50', '6.25'),
    ('gpt-5.6-terra',     'GPT-5.6 Terra',     '2.50', '15',   '0.25', '3.125'),
    ('gpt-5.6-luna',      'GPT-5.6 Luna',      '1',    '6',    '0.10', '1.25'),
    -- gpt-5.3-codex 经 normalize 剥 -codex 后的键
    ('gpt-5.3',           'GPT-5.3',           '1.75', '14',   '0.175','0'),
    ('gpt-5.5',           'GPT-5.5',           '5',    '30',   '0.50', '0'),
    ('gpt-5.4',           'GPT-5.4',           '2.50', '15',   '0.25', '0'),
    ('gpt-5.4-mini',      'GPT-5.4 mini',      '0.75', '4.50', '0.075','0'),
    ('gpt-5.4-nano',      'GPT-5.4 nano',      '0.20', '1.25', '0.02', '0'),
    ('gpt-5.2',           'GPT-5.2',           '1.75', '14',   '0.175','0'),
    ('gpt-5.1',           'GPT-5.1',           '1.25', '10',   '0.125','0'),
    ('gpt-5',             'GPT-5',             '1.25', '10',   '0.125','0'),
    ('gpt-5-mini',        'GPT-5 mini',        '0.25', '2',    '0.025','0'),
    ('gpt-5-nano',        'GPT-5 nano',        '0.05', '0.40', '0.005','0'),
    ('codex-mini',        'Codex mini',        '0.75', '3',    '0.025','0'),
    ('o3',                'OpenAI o3',         '2',    '8',    '0.50', '0'),
    ('o4-mini',           'OpenAI o4-mini',    '1.10', '4.40', '0.275','0'),
    ('gpt-4.1',           'GPT-4.1',           '2',    '8',    '0.50', '0'),
    ('gpt-4.1-mini',      'GPT-4.1 mini',      '0.40', '1.60', '0.10', '0'),
    -- DeepSeek
    ('deepseek-v4-pro',   'DeepSeek V4 Pro',   '0.435','0.87', '0.003625','0'),
    ('deepseek-v4-flash', 'DeepSeek V4 Flash', '0.14', '0.28', '0.0028','0'),
    ('deepseek-v3.2',     'DeepSeek V3.2',     '0.28', '0.42', '0.028','0'),
    ('deepseek-chat',     'DeepSeek Chat',     '0.27', '1.10', '0.07', '0'),
    ('deepseek-reasoner', 'DeepSeek Reasoner', '0.55', '2.19', '0.14', '0'),
    -- Zhipu GLM
    ('glm-4.6',           'GLM-4.6',           '0.6',  '2.2',  '0.11', '0'),
    ('glm-4.7',           'GLM-4.7',           '0.6',  '2.2',  '0.11', '0'),
    ('glm-5',             'GLM-5',             '1',    '3.2',  '0.2',  '0'),
    ('glm-5.1',           'GLM-5.1',           '1.4',  '4.4',  '0.26', '0'),
    ('glm-5.2',           'GLM-5.2',           '1.4',  '4.4',  '0.26', '0'),
    -- Kimi
    ('kimi-k2-thinking',  'Kimi K2 Thinking',  '0.55', '2.20', '0.10', '0'),
    ('kimi-k2.5',         'Kimi K2.5',         '0.60', '3.00', '0.10', '0'),
    ('kimi-k2.6',         'Kimi K2.6',         '0.95', '4.00', '0.16', '0'),
    ('kimi-k2.7-code',    'Kimi K2.7 Code',    '0.95', '4.00', '0.19', '0'),
    -- xAI Grok
    ('grok-4',            'Grok 4',            '3',    '15',   '0.75', '0'),
    ('grok-4.3',          'Grok 4.3',          '1.25', '2.50', '0.20', '0'),
    ('grok-code-fast-1',  'Grok Code Fast 1',  '1',    '2',    '0.20', '0'),
    -- MiniMax
    ('minimax-m2.1',      'MiniMax M2.1',      '0.27', '0.95', '0.03', '0'),
    ('minimax-m2.5',      'MiniMax M2.5',      '0.15', '0.95', '0.03', '0'),
    -- Qwen
    ('qwen3-max',         'Qwen3 Max',         '0.78', '3.90', '0',    '0'),
    ('qwen3-coder-plus',  'Qwen3 Coder Plus',  '0.65', '3.25', '0.13', '0');
