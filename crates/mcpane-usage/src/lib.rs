//! MCPane 用量解析——从 Claude / Codex 本地会话 JSONL 抽取 token 用量。
//!
//! MCPane 无 LLM 代理层，采集不到模型请求流量；token 用量的唯一现实数据源
//! 是客户端自己写下的会话文件（对齐 cc-switch 的生产验证方案）：
//! - Claude：`~/.claude/projects/**/*.jsonl` 的 `assistant.message.usage`；
//! - Codex：`~/.codex/sessions/**/rollout-*.jsonl` 的 `token_count` 事件。
//!
//! 本 crate 是纯解析库：只做「文件 → `UsageEvent` 列表 + 增量游标」，不碰
//! 数据库——落库、成本估算、聚合查询都在 mcpane-storage / desktop 命令层。
//! 私有会话格式不是稳定契约，所有解析都逐行容错：坏行跳过、缺字段取默认。

mod claude;
mod codex;
mod gemini;

pub use claude::{ClaudeSessionFile, collect_claude_files, parse_claude_file};
pub use codex::{RolloutIndex, build_rollout_index, collect_codex_files, parse_codex_file};
pub use gemini::{collect_gemini_files, parse_gemini_file};

use std::path::Path;

/// 一条可计费的模型用量事件（Claude 每条 assistant 消息；Codex 每个非零
/// token_count delta）。字段与 usage_events 表一一对应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageEvent {
    /// 去重主键：Claude `session:<msg_id>`；
    /// Codex `codex_session:thread-v2:<thread_id>:<event_index>`
    ///（v 段即解析器版本，升版时须同步递增命令层 `CODEX_PARSER_VERSION` 触发重建）。
    pub request_id: String,
    /// 事件秒级时间戳；原始 timestamp 解析失败为 0，由调用方回落导入时刻。
    pub ts: i64,
    /// 'claude' | 'codex'
    pub client_app: &'static str,
    /// 原始 model 名（保真展示）。
    pub model: String,
    /// 归一化定价匹配键，与 model_pricing.model_id 精确 JOIN。
    pub model_key: String,
    /// Claude 取 projects 下目录名原样；Codex 无此维度。
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    /// Codex 日志无 cache_creation 维度，恒 0。
    pub cache_creation_tokens: i64,
}

impl UsageEvent {
    /// 任一计费维度 > 0 才值得入库。Anthropic 在受理请求时即对
    /// input + cache 计费，不能用 output>0 或 stop_reason 过滤
    /// （会系统性丢弃 Workflow/子 agent 的短命请求）。
    pub fn has_billable_tokens(&self) -> bool {
        self.input_tokens > 0
            || self.output_tokens > 0
            || self.cache_read_tokens > 0
            || self.cache_creation_tokens > 0
    }
}

/// 单文件增量同步游标（对应 usage_sync_state 一行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyncCursor {
    /// 文件 mtime 纳秒。与 `last_size` 一起判断"未变则整文件跳过"。
    pub last_modified_nanos: i64,
    /// 已处理行数（1-based 行号高水位）。文件变了从头重扫时，
    /// 行号 <= 此值的行不再产出事件。
    pub last_line_offset: i64,
    /// 上次扫描消费到的字节位置（Codex/Gemini 记整文件大小；Claude 记到最后一个
    /// 完整行的末尾，下轮从这里接着读）。Codex 全程持有 rollout 追加句柄，Windows
    /// NTFS 上文件增长而 mtime 不动，只看 mtime 会把活跃会话永久跳过；0 表示旧游标
    /// 未记录（升级前写入的行），此时退回只看 mtime，避免升级后全量重扫。
    pub last_size: i64,
}

/// 文件的变更指纹：mtime 纳秒 + 字节数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FileStamp {
    pub modified_nanos: i64,
    pub size: i64,
}

impl SyncCursor {
    /// 文件自上次扫描后是否未变（可整文件跳过）。
    /// mtime>0 守卫：metadata 取不到时回落 0，此时对文件一无所知，应重扫
    ///（request_id 去重幂等）而非跳过——否则 0<=任何值 恒真会漏掉新事件。
    pub fn is_unchanged(&self, stamp: FileStamp) -> bool {
        stamp.modified_nanos > 0
            && stamp.modified_nanos <= self.last_modified_nanos
            && self.last_line_offset > 0
            && (self.last_size == 0 || stamp.size == self.last_size)
    }
}

/// 单文件一次解析的产出：新增事件 + 新游标。
#[derive(Debug)]
pub struct FileParse {
    pub events: Vec<UsageEvent>,
    pub cursor: SyncCursor,
}

#[derive(Debug, thiserror::Error)]
pub enum UsageError {
    #[error("读取会话文件失败：{0}")]
    Io(#[from] std::io::Error),
}

/// 文件 mtime 纳秒时间戳；取不到（平台不支持/文件消失）回落 0，
/// 效果是该文件每轮都会重扫，靠行 offset 与 request_id 主键防重复。
pub fn file_mtime_nanos(path: &Path) -> i64 {
    file_stamp(path).modified_nanos
}

/// 文件 mtime + 字节数；取不到 metadata 时两者都回落 0（含义同上）。
pub fn file_stamp(path: &Path) -> FileStamp {
    let Ok(meta) = std::fs::metadata(path) else {
        return FileStamp::default();
    };
    let modified_nanos = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0);
    FileStamp {
        modified_nanos,
        size: i64::try_from(meta.len()).unwrap_or(i64::MAX),
    }
}

/// RFC3339 → 秒级时间戳；解析失败返回 0（调用方回落导入时刻）。
pub fn parse_rfc3339_secs(ts: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|dt| dt.timestamp())
        .unwrap_or(0)
}

/// 归一化 model 名为定价匹配键（与 model_pricing.model_id 对齐）。
///
/// 规则按序：
/// 1. 小写：`GLM-4.6` → `glm-4.6`
/// 2. 剥 provider 前缀：`openai/gpt-5.4` → `gpt-5.4`
/// 3. gpt 系列剥 `-codex` 及其后所有内容（codex 变体与基础模型同价，
///    且 effort/日期后缀跟在 -codex 之后）：`gpt-5.2-codex-high` → `gpt-5.2`
/// 4. 剥 effort 后缀：`gpt-5.1-high` → `gpt-5.1`
/// 5. 剥 ISO 日期后缀 `-YYYY-MM-DD`：`claude-sonnet-5-2026-03-05` → `claude-sonnet-5`
/// 6. 剥紧凑日期后缀 `-YYYYMMDD`：`claude-opus-4-6-20260206` → `claude-opus-4-6`
pub fn normalize_model(raw: &str) -> String {
    let mut name = raw.to_lowercase();

    if let Some(pos) = name.rfind('/') {
        name = name[pos + 1..].to_string();
    }

    if name.starts_with("gpt-")
        && let Some(pos) = name.find("-codex")
    {
        name.truncate(pos);
    }

    for effort in ["-minimal", "-low", "-medium", "-high", "-xhigh"] {
        if let Some(stripped) = name.strip_suffix(effort) {
            name = stripped.to_string();
            break;
        }
    }

    // ISO 日期后缀 -YYYY-MM-DD（正好 11 字节，全 ASCII 才判断）
    if name.len() > 11 && name.is_char_boundary(name.len() - 11) {
        let suffix = &name[name.len() - 11..];
        let b = suffix.as_bytes();
        if suffix.is_ascii()
            && b[0] == b'-'
            && b[1..5].iter().all(u8::is_ascii_digit)
            && b[5] == b'-'
            && b[6..8].iter().all(u8::is_ascii_digit)
            && b[8] == b'-'
            && b[9..11].iter().all(u8::is_ascii_digit)
        {
            name.truncate(name.len() - 11);
        }
    }

    // 紧凑日期后缀 -YYYYMMDD（正好 9 字节）
    if name.len() > 9 && name.is_char_boundary(name.len() - 9) {
        let suffix = &name[name.len() - 9..];
        let b = suffix.as_bytes();
        if suffix.is_ascii() && b[0] == b'-' && b[1..9].iter().all(u8::is_ascii_digit) {
            name.truncate(name.len() - 9);
        }
    }

    name
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn normalize_lowercase_and_prefix() {
        assert_eq!(normalize_model("GLM-4.6"), "glm-4.6");
        assert_eq!(normalize_model("openai/GPT-5.4"), "gpt-5.4");
        assert_eq!(
            normalize_model("Anthropic/Claude-Sonnet-5"),
            "claude-sonnet-5"
        );
    }

    #[test]
    fn mtime_nanos_falls_back_to_zero_when_metadata_unavailable() {
        // 契约：取不到 metadata 回落 0。三解析器的 `mtime>0` 守卫依赖此契约把
        // "未知 mtime" 判为需重扫（0 不会被误当成"比游标旧而跳过"）。
        let missing = std::env::temp_dir().join("mcpane-usage-nonexistent-file.jsonl");
        let _ = std::fs::remove_file(&missing);
        assert_eq!(file_mtime_nanos(&missing), 0);
    }

    #[test]
    fn cursor_unchanged_requires_same_size_when_size_known() {
        let cursor = SyncCursor {
            last_modified_nanos: 100,
            last_line_offset: 3,
            last_size: 1000,
        };
        let same = FileStamp {
            modified_nanos: 100,
            size: 1000,
        };
        assert!(cursor.is_unchanged(same));
        // Windows 上 Codex 持有句柄追加：mtime 不动、size 变大 → 必须重扫
        assert!(!cursor.is_unchanged(FileStamp {
            modified_nanos: 100,
            size: 1500,
        }));
        assert!(!cursor.is_unchanged(FileStamp {
            modified_nanos: 101,
            size: 1000,
        }));
        // metadata 取不到 → 重扫
        assert!(!cursor.is_unchanged(FileStamp::default()));
        // 旧游标（升级前）没记 size：退回只看 mtime，不触发全量重扫
        let legacy = SyncCursor {
            last_size: 0,
            ..cursor
        };
        assert!(legacy.is_unchanged(FileStamp {
            modified_nanos: 100,
            size: 1500,
        }));
        // 从未扫过（行高位 0）永远不算未变
        let fresh = SyncCursor::default();
        assert!(!fresh.is_unchanged(same));
    }

    #[test]
    fn normalize_strips_gpt_codex_variants() {
        assert_eq!(normalize_model("gpt-5.2-codex"), "gpt-5.2");
        assert_eq!(normalize_model("gpt-5.6-codex-high"), "gpt-5.6");
        // 非 gpt 前缀不受 -codex 规则影响
        assert_eq!(normalize_model("codex-mini"), "codex-mini");
    }

    #[test]
    fn normalize_strips_effort_suffix() {
        assert_eq!(normalize_model("gpt-5.1-high"), "gpt-5.1");
        assert_eq!(normalize_model("gpt-5-minimal"), "gpt-5");
        assert_eq!(normalize_model("gpt-5.4-xhigh"), "gpt-5.4");
    }

    #[test]
    fn normalize_strips_date_suffixes() {
        assert_eq!(normalize_model("gpt-5.4-2026-03-05"), "gpt-5.4");
        assert_eq!(normalize_model("gpt-5.4-20260305"), "gpt-5.4");
        assert_eq!(
            normalize_model("claude-opus-4-6-20260206"),
            "claude-opus-4-6"
        );
    }

    #[test]
    fn normalize_no_change_for_clean_names() {
        assert_eq!(normalize_model("o3"), "o3");
        assert_eq!(normalize_model("deepseek-chat"), "deepseek-chat");
        assert_eq!(normalize_model("claude-sonnet-5"), "claude-sonnet-5");
    }

    #[test]
    fn normalize_combined_rules() {
        assert_eq!(
            normalize_model("openai/GPT-5.2-codex-high-2026-03-05"),
            "gpt-5.2"
        );
        assert_eq!(normalize_model("azure/gpt-5.1-2026-03-05"), "gpt-5.1");
    }

    #[test]
    fn parse_rfc3339_secs_ok_and_fallback() {
        assert_eq!(parse_rfc3339_secs("1970-01-01T00:16:45Z"), 1005);
        assert_eq!(parse_rfc3339_secs("not-a-timestamp"), 0);
    }

    #[test]
    fn billable_tokens_gate() {
        let mut ev = UsageEvent {
            request_id: "session:msg".into(),
            ts: 0,
            client_app: "claude",
            model: "m".into(),
            model_key: "m".into(),
            project: None,
            session_id: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        };
        assert!(!ev.has_billable_tokens());
        ev.cache_read_tokens = 1;
        assert!(ev.has_billable_tokens());
    }
}
