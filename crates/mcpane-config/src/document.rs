//! 格式感知的配置文档：JSON（serde_json，字段级保留）、
//! TOML（toml_edit 语法树，注释/未知表/格式保留）与
//! Env（dotenv 行模型，注释/键序/未知键逐字保留——Gemini CLI 的 ~/.gemini/.env）。

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::ConfigError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    Json,
    Toml,
    /// JSON5（无引号键 / 单引号 / 注释 / 尾逗号）。OpenClaw 的 `openclaw.json`
    /// 名为 .json 实为 JSON5——用严格 JSON 解析器读它会直接失败。
    Json5,
    /// YAML（Hermes 的 `~/.hermes/config.yaml`）。
    ///
    /// **与其他格式的重要差别：这一路不保注释**。Rust 生态没有 toml_edit /
    /// json-five 那种成熟的 YAML round-trip 编辑器，只能整份读写。写入仍走引擎，
    /// 写前快照与历史回滚照旧——格式被规整时用户
    /// 能从历史里找回原文。
    Yaml,
    /// dotenv 风格 `KEY=VALUE`（Gemini CLI 读 `~/.gemini/.env`）。
    /// 路径恒为单段键；值恒为字符串。
    Env,
}

impl ConfigFormat {
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension().and_then(|e| e.to_str()) {
            Some("json") => Some(Self::Json),
            Some("json5") => Some(Self::Json5),
            Some("yaml") | Some("yml") => Some(Self::Yaml),
            Some("toml") => Some(Self::Toml),
            Some("env") => Some(Self::Env),
            None if path.file_name().is_some_and(|n| n == ".claude.json") => Some(Self::Json),
            None if path.file_name().is_some_and(|n| n == ".env") => Some(Self::Env),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Json5 => "json5",
            Self::Yaml => "yaml",
            Self::Toml => "toml",
            Self::Env => "env",
        }
    }
}

/// dotenv 文档的一行：`KEY=VALUE` 结构化；注释/空行/无法解析行原样保留。
#[derive(Debug, Clone)]
pub(crate) enum EnvLine {
    /// 注释、空行或非 KEY=VALUE 行（逐字保留，绝不改写）。
    Raw(String),
    Pair {
        key: String,
        /// 语义值（读取时剥去成对引号）。
        value: String,
        /// 原始行文本：未被 patch 修改时序列化用它，保证用户手写格式不变。
        raw: String,
    },
}

/// dotenv 文档：行模型保注释保键序。只有被 patch 修改的行才会重新生成文本。
#[derive(Debug, Clone, Default)]
pub struct EnvDocument {
    pub(crate) lines: Vec<EnvLine>,
}

impl EnvDocument {
    fn parse(content: &str) -> Self {
        let lines = content
            .lines()
            .map(|line| {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    return EnvLine::Raw(line.to_owned());
                }
                match trimmed.split_once('=') {
                    Some((key, value)) if is_env_key(key.trim()) => EnvLine::Pair {
                        key: key.trim().to_owned(),
                        value: unquote_env(value.trim()).into_owned(),
                        raw: line.to_owned(),
                    },
                    _ => EnvLine::Raw(line.to_owned()),
                }
            })
            .collect();
        Self { lines }
    }

    fn serialize(&self) -> String {
        let mut out = String::new();
        for line in &self.lines {
            match line {
                EnvLine::Raw(raw) | EnvLine::Pair { raw, .. } => out.push_str(raw),
            }
            out.push('\n');
        }
        out
    }

    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        // dotenv（node 实现，Gemini CLI 所用）语义：先到先得。
        self.lines.iter().find_map(|line| match line {
            EnvLine::Pair { key: k, value, .. } if k == key => Some(value.as_str()),
            _ => None,
        })
    }
}

/// dotenv 键：`[A-Za-z_][A-Za-z0-9_]*`。
fn is_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 读取语义：与真实消费方（Gemini CLI 用的 node dotenv）逐条对齐——
/// 剥去成对的外层引号，且**只有双引号形态**会把 `\n` / `\r` 展开成真换行。
///
/// 那处展开不是我们的选择，是 dotenv 的既定行为（node 内置 `--env-file`
/// 与 dotenv npm 包一致，2026-08-06 实测）。不跟着展开，我们读到的值就与
/// Gemini CLI 实际吃到的值不同——回填会把一个 CLI 从没见过的值写回配置。
/// 单引号与裸值形态 dotenv 一律逐字取用，故这里也逐字返回。
fn unquote_env(value: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[bytes.len() - 1] == bytes[0]
    {
        let inner = &value[1..value.len() - 1];
        if bytes[0] == b'"' && (inner.contains("\\n") || inner.contains("\\r")) {
            return Cow::Owned(inner.replace("\\n", "\n").replace("\\r", "\r"));
        }
        return Cow::Borrowed(inner);
    }
    Cow::Borrowed(value)
}

/// 写入语义：挑一种能被 dotenv **无损读回**的形态；挑不出来返回 `None`
/// （由调用方报错，绝不静默写出会被消费方改写的内容）。
///
/// 三种形态的 dotenv 语义（node 内置 `--env-file` 实测，2026-08-06）：
/// - 裸值 `K=v`：逐字取用，但空格会截断、`#` 起注释、首尾引号会被当成引号形态；
/// - 单引号 `K='v'`：**内部不做任何转义处理，逐字取用**——首选；
/// - 双引号 `K="v"`：会把 `\n` / `\r` 展开成真换行，且**不认 `\"` 转义**
///   （`"sk-a\"b"` 被读成 `sk-a\`，值直接截断）。
///
/// 早期实现无条件用双引号 + `\"` 转义，正好踩中最后一条：值里带双引号会被
/// 消费方截断，带 `\n` 字面量会被展开成换行。两种都是静默的凭据损坏。
pub(crate) fn quote_env(value: &str) -> Option<String> {
    // 裸值够用就用裸值（保持用户配置最朴素的样子）。
    let bare_ok = !value.is_empty()
        && !value
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '#' | '"' | '\''));
    if bare_ok {
        return Some(value.to_owned());
    }
    // 行模型一行一条，任何引号形态都表达不了真换行。
    if value.contains('\n') || value.contains('\r') {
        return None;
    }
    // 单引号：dotenv 逐字取用，能无损装下 `"`、`\`、空格、`#`。
    if !value.contains('\'') {
        return Some(format!("'{value}'"));
    }
    // 值里有单引号只能退到双引号，此时 `"` 与 `\` 都无法无损表达。
    if !value.contains('"') && !value.contains('\\') {
        return Some(format!("\"{value}\""));
    }
    None
}

/// 已解析的配置文档。JSON 部分保留全部字段；TOML/JSON5 部分保留语法树；
/// Env 部分保留行结构。
#[derive(Debug)]
pub enum ConfigDocument {
    Json(serde_json::Value),
    Json5(json_five::rt::parser::JSONText),
    /// YAML 文档在内部就用 JSON 值模型——两者的数据模型对我们要碰的配置
    /// 完全同构，路径读写因此与 JSON 共用一套实现，只在解析/序列化边界转换。
    Yaml(serde_json::Value),
    Toml(toml_edit::DocumentMut),
    Env(EnvDocument),
}

impl ConfigDocument {
    /// 可失败的克隆。json-five 的语法树没实现 `Clone`，只能绕一次
    /// 「序列化 → 重解析」——而这一步**真的会失败**，所以它必须是 `Result`，
    /// 不能是 `Clone`。
    ///
    /// 这里曾是一个 `impl Clone`，往返失败时静默退化成 `Json(Null)`。那条兜底
    /// 看着保守，实际是一条数据全丢的路径：退化后的空文档上，纯 `Remove` 计划
    /// （OpenClaw 的「移除供应商」正是）每一条都幂等成功，序列化出字面量
    /// `null`，被引擎当成正常结果原子写进用户的 `openclaw.json`——整份配置没了，
    /// 还报成功。触发条件只是配置里有一个块注释（见 [`crate::json5`] 的上游缺陷
    /// 说明）。宁可报错也不能猜。
    pub fn try_clone(&self) -> Result<Self, ConfigError> {
        Ok(match self {
            Self::Json(value) => Self::Json(value.clone()),
            Self::Yaml(value) => Self::Yaml(value.clone()),
            Self::Json5(doc) => Self::Json5(crate::json5::reparse_own_output(doc)?),
            Self::Toml(doc) => Self::Toml(doc.clone()),
            Self::Env(doc) => Self::Env(doc.clone()),
        })
    }
}

impl ConfigDocument {
    pub fn parse(content: &str, format: ConfigFormat) -> Result<Self, ConfigError> {
        // Env 的行模型同样要剥：带 BOM 的首行认不出是键，更新时会在末尾追加重复定义。
        let content = strip_bom(content);
        match format {
            ConfigFormat::Json => {
                // 空文件视为空对象（Claude Code 初次使用前 .claude.json 可能为空）。
                if content.trim().is_empty() {
                    return Ok(Self::Json(serde_json::Value::Object(Default::default())));
                }
                serde_json::from_str(content)
                    .map(Self::Json)
                    .map_err(|e| ConfigError::Parse {
                        format: "json",
                        message: e.to_string(),
                    })
            }
            ConfigFormat::Json5 => crate::json5::parse(content).map(Self::Json5),
            ConfigFormat::Yaml => {
                if content.trim().is_empty() {
                    return Ok(Self::Yaml(serde_json::Value::Object(Default::default())));
                }
                serde_yaml_ng::from_str::<serde_json::Value>(content)
                    .map(Self::Yaml)
                    .map_err(|e| ConfigError::Parse {
                        format: "yaml",
                        message: e.to_string(),
                    })
            }
            ConfigFormat::Toml => content
                .parse::<toml_edit::DocumentMut>()
                .map(Self::Toml)
                .map_err(|e| ConfigError::Parse {
                    format: "toml",
                    message: e.to_string(),
                }),
            // 行模型对任何文本都能解析（无法识别的行原样保留），不产生解析错误。
            ConfigFormat::Env => Ok(Self::Env(EnvDocument::parse(content))),
        }
    }

    pub fn format(&self) -> ConfigFormat {
        match self {
            Self::Json(_) => ConfigFormat::Json,
            Self::Json5(_) => ConfigFormat::Json5,
            Self::Yaml(_) => ConfigFormat::Yaml,
            Self::Toml(_) => ConfigFormat::Toml,
            Self::Env(_) => ConfigFormat::Env,
        }
    }

    /// 序列化回文本。JSON 统一 2 空格缩进；TOML/Env 保留原始格式。
    pub fn serialize(&self) -> String {
        match self {
            Self::Json(value) => {
                let mut text =
                    serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_owned());
                text.push('\n');
                text
            }
            Self::Json5(doc) => crate::json5::serialize(doc),
            Self::Yaml(value) => {
                serde_yaml_ng::to_string(value).unwrap_or_else(|_| "{}\n".to_owned())
            }
            Self::Toml(doc) => doc.to_string(),
            Self::Env(doc) => doc.serialize(),
        }
    }

    /// 按路径读取值（返回 JSON 表示，便于统一 Diff）。
    pub fn get(&self, path: &[&str]) -> Option<serde_json::Value> {
        match self {
            Self::Json(value) => {
                let mut cur = value;
                for seg in path {
                    cur = cur.get(seg)?;
                }
                Some(cur.clone())
            }
            Self::Json5(doc) => crate::json5::get(doc, path),
            Self::Yaml(value) => {
                let mut cur = value;
                for seg in path {
                    cur = cur.get(seg)?;
                }
                Some(cur.clone())
            }
            Self::Toml(doc) => {
                let mut cur: &toml_edit::Item = doc.as_item();
                for seg in path {
                    cur = cur.get(seg)?;
                }
                toml_item_to_json(cur)
            }
            Self::Env(doc) => match path {
                [key] => doc
                    .get(key)
                    .map(|v| serde_json::Value::String(v.to_owned())),
                _ => None,
            },
        }
    }
}

fn toml_item_to_json(item: &toml_edit::Item) -> Option<serde_json::Value> {
    // 直接递归转换（仅用于 Diff 展示，不用于写回）。曾借道"克隆 → 序列化 →
    // 重解析"的字符串往返，对每个叶子读取是 O(子树) 的深拷贝，纯浪费。
    match item {
        toml_edit::Item::None => None,
        other => Some(toml_value_to_json(other)),
    }
}

fn toml_value_to_json(item: &toml_edit::Item) -> serde_json::Value {
    use serde_json::Value;
    match item {
        toml_edit::Item::None => Value::Null,
        toml_edit::Item::Value(v) => toml_scalar_to_json(v),
        toml_edit::Item::Table(t) => Value::Object(
            t.iter()
                .map(|(k, v)| (k.to_owned(), toml_value_to_json(v)))
                .collect(),
        ),
        toml_edit::Item::ArrayOfTables(arr) => Value::Array(
            arr.iter()
                .map(|t| {
                    Value::Object(
                        t.iter()
                            .map(|(k, v)| (k.to_owned(), toml_value_to_json(v)))
                            .collect(),
                    )
                })
                .collect(),
        ),
    }
}

fn toml_scalar_to_json(value: &toml_edit::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        toml_edit::Value::String(s) => Value::String(s.value().clone()),
        toml_edit::Value::Integer(i) => Value::from(*i.value()),
        toml_edit::Value::Float(f) => {
            serde_json::Number::from_f64(*f.value()).map_or(Value::Null, Value::Number)
        }
        toml_edit::Value::Boolean(b) => Value::Bool(*b.value()),
        toml_edit::Value::Datetime(d) => Value::String(d.to_string()),
        toml_edit::Value::Array(arr) => Value::Array(arr.iter().map(toml_scalar_to_json).collect()),
        toml_edit::Value::InlineTable(t) => Value::Object(
            t.iter()
                .map(|(k, v)| (k.to_owned(), toml_scalar_to_json(v)))
                .collect(),
        ),
    }
}

/// UTF-8 BOM（Windows 上 PowerShell 的 `Out-File` 与老记事本默认就写它）。
pub const BOM: &str = "\u{feff}";

/// 磁盘原文的行尾与 BOM。解析器一律吐 LF、不带 BOM，直接落盘会把 Windows 用户的
/// CRLF 文件整份改写（AGENTS.md 规则 7），故读入时记下、写回时还原。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileFormatting {
    pub newline: NewlineStyle,
    pub has_bom: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewlineStyle {
    Lf,
    Crlf,
}

impl FileFormatting {
    pub fn detect(raw: &str) -> Self {
        Self {
            newline: NewlineStyle::detect(raw),
            has_bom: raw.starts_with(BOM),
        }
    }

    /// 把序列化结果还原成原文外壳。序列化器恒吐 LF、不带 BOM，所以只需单向补。
    pub fn restore(&self, text: &mut String) {
        if self.newline == NewlineStyle::Crlf {
            *text = to_crlf(text);
        }
        if self.has_bom && !text.starts_with(BOM) {
            text.insert_str(0, BOM);
        }
    }
}

impl NewlineStyle {
    /// 同 rustfmt 的 `auto_detect_newline_style`：看第一个 `\n` 前是不是 `\r`。
    /// 混合行尾统一成检测到的那种（rustfmt / dprint / VS Code 同此）。
    fn detect(raw: &str) -> Self {
        match raw.chars().position(|c| c == '\n') {
            Some(pos) => match raw.chars().nth(pos.saturating_sub(1)) {
                Some('\r') => Self::Crlf,
                _ => Self::Lf,
            },
            // 空文件/首次写入恒用 LF，不跟随本机风格（与 rustfmt 有意分歧）：
            // 另一个写者是 Claude Code、Codex 等跨平台 CLI，它们恒写 LF，
            // 跟随本机会让行尾在两个写者之间来回抖。
            None => Self::Lf,
        }
    }
}

/// LF → CRLF。已是 `\r\n` 的不重复加 `\r`；孤立的 `\r`（不跟 `\n`）原样保留。
fn to_crlf(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => out.push_str("\r\n"),
            other => out.push(other),
        }
    }
    out
}

/// 解析前剥 BOM：serde_json 遇 BOM 报 `expected value at line 1 column 1`，
/// serde_yaml_ng 报得更误导（`more than one document`）。
pub(crate) fn strip_bom(content: &str) -> &str {
    content.strip_prefix(BOM).unwrap_or(content)
}

/// 从磁盘读取的配置及其身份信息（哈希 + mtime，用于冲突检测）。
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub path: PathBuf,
    pub format: ConfigFormat,
    pub content: String,
    pub content_hash: String,
    pub mtime_unix_nanos: Option<u128>,
    /// 文件不存在时为 true（首次写入场景）。
    pub missing: bool,
    /// 磁盘原文的行尾与 BOM，写回时按它还原。
    pub formatting: FileFormatting,
}

pub fn content_hash(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    #[test]
    fn yaml_round_trips_through_json_value_model() {
        let source = "custom_providers:\n  - name: openrouter\n    base_url: https://openrouter.ai/api/v1\nagent:\n  max_turns: 50\n";
        let mut doc = ConfigDocument::parse(source, ConfigFormat::Yaml).unwrap();
        assert_eq!(
            doc.get(&["custom_providers"]).unwrap()[0]["name"],
            "openrouter"
        );
        assert_eq!(doc.get(&["agent", "max_turns"]).unwrap(), 50);

        // 整个数组当一个值写回——这样路径模型不需要"按字段定位数组元素"。
        crate::patch::PatchPlan::new(vec![crate::patch::PatchOp::Set {
            path: vec!["custom_providers".into()],
            value: serde_json::json!([{ "name": "kimi", "base_url": "https://api.moonshot.cn/v1" }]),
        }])
        .apply(&mut doc)
        .unwrap();

        let out = doc.serialize();
        let doc = ConfigDocument::parse(&out, ConfigFormat::Yaml).unwrap();
        assert_eq!(doc.get(&["custom_providers"]).unwrap()[0]["name"], "kimi");
        // 同一份里的其他键不受影响。
        assert_eq!(doc.get(&["agent", "max_turns"]).unwrap(), 50);
    }

    #[test]
    fn yaml_empty_file_is_an_empty_mapping() {
        let doc = ConfigDocument::parse("", ConfigFormat::Yaml).unwrap();
        assert_eq!(
            doc.get(&[]).unwrap_or(serde_json::json!({})),
            serde_json::json!({})
        );
    }

    use super::*;

    #[test]
    fn json_roundtrip_preserves_unknown_fields() {
        let raw = r#"{"known": 1, "unknownObj": {"a": [1, 2, {"deep": true}]}, "unknownStr": "x"}"#;
        let doc = ConfigDocument::parse(raw, ConfigFormat::Json).unwrap();
        let out = doc.serialize();
        let reparsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        let original: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(reparsed, original, "JSON 未知字段必须 100% 保留");
    }

    #[test]
    fn toml_roundtrip_preserves_comments_and_unknown_tables() {
        let raw = "# 顶部注释\n[known]\nkey = \"v\" # 行内注释\n\n[unknown_table]\nfoo = 1\n";
        let doc = ConfigDocument::parse(raw, ConfigFormat::Toml).unwrap();
        assert_eq!(doc.serialize(), raw, "TOML 注释与未知表必须逐字保留");
    }

    #[test]
    fn corrupt_inputs_yield_parse_errors() {
        assert!(matches!(
            ConfigDocument::parse("{not json", ConfigFormat::Json),
            Err(ConfigError::Parse { format: "json", .. })
        ));
        assert!(matches!(
            ConfigDocument::parse("[table\nbroken", ConfigFormat::Toml),
            Err(ConfigError::Parse { format: "toml", .. })
        ));
    }

    #[test]
    fn empty_json_is_treated_as_empty_object() {
        let doc = ConfigDocument::parse("", ConfigFormat::Json).unwrap();
        assert_eq!(doc.get(&[]).unwrap(), serde_json::json!({}));
    }

    #[test]
    fn env_roundtrip_preserves_comments_order_and_unknown_keys() {
        let raw = "# Gemini CLI 环境\nGOOGLE_CLOUD_PROJECT=my-proj\n\n# key 区\nGEMINI_API_KEY=\"sk-abc\"\nbroken line without eq\n";
        let doc = ConfigDocument::parse(raw, ConfigFormat::Env).unwrap();
        assert_eq!(doc.serialize(), raw, "未修改时必须逐字保留（含注释/坏行）");
        // 读取剥引号
        assert_eq!(
            doc.get(&["GEMINI_API_KEY"]).unwrap(),
            serde_json::json!("sk-abc")
        );
        assert_eq!(
            doc.get(&["GOOGLE_CLOUD_PROJECT"]).unwrap(),
            serde_json::json!("my-proj")
        );
        // 多段路径无意义
        assert!(doc.get(&["a", "b"]).is_none());
    }

    #[test]
    fn env_duplicate_keys_first_wins() {
        // dotenv（node 实现）语义：先到先得。
        let doc = ConfigDocument::parse("K=first\nK=second\n", ConfigFormat::Env).unwrap();
        assert_eq!(doc.get(&["K"]).unwrap(), serde_json::json!("first"));
    }

    #[test]
    fn env_from_path_recognizes_dotenv() {
        assert_eq!(
            ConfigFormat::from_path(Path::new("/home/u/.gemini/.env")),
            Some(ConfigFormat::Env)
        );
    }

    #[test]
    fn get_navigates_both_formats() {
        let json = ConfigDocument::parse(r#"{"a":{"b":42}}"#, ConfigFormat::Json).unwrap();
        assert_eq!(json.get(&["a", "b"]).unwrap(), serde_json::json!(42));

        let toml = ConfigDocument::parse(
            "[mcp_servers.mcpane]\nurl = \"http://x\"\n",
            ConfigFormat::Toml,
        )
        .unwrap();
        assert_eq!(
            toml.get(&["mcp_servers", "mcpane", "url"]).unwrap(),
            serde_json::json!("http://x")
        );
    }
}
