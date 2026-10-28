//! Grok CLI `config.toml` 的最小 Patch 生成。
//!
//! 只触碰两处：`[model."<slug>"]` 这张表，以及 `[models].default` 指针。
//! 用户自建的其他模型表原样保留——切走时只清我们那张。

use mcpane_config::{PatchOp, PatchPlan};
use serde_json::json;

/// 模型表所在的顶层键：`[model.<slug>]`。
pub const MODEL_TABLE_KEY: &str = "model";
/// 默认模型指针：`[models].default`。
pub const MODELS_DEFAULT_PATH: [&str; 2] = ["models", "default"];

/// `api_backend` 默认值：中转站几乎都是 OpenAI 兼容的 chat completions。
pub const DEFAULT_API_BACKEND: &str = "chat";
/// `context_window` 默认值（Grok CLI 要求正整数，缺省会被它判为非法配置）。
///
/// 500k = grok-4.5 的实际上下文窗口（xAI 官方 docs.x.ai 模型页标注，2026-07-25
/// 核对）。Grok CLI 拿这个数判断何时压缩会话
/// 历史，填小了不会报错、只会让用户在还剩一半窗口时就被提前压缩——预设里
/// 每条 grok 供应商都不带该字段，一律吃这个默认值，所以它必须是真实值。
pub const DEFAULT_CONTEXT_WINDOW: i64 = 500_000;

/// 由供应商名派生模型表名：`mcpane-<sanitized>`。带前缀是为了切走时能只清
/// 我们写的表，用户手写的表一律不碰。
pub fn table_slug(name: &str) -> String {
    let sanitized: String = name
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned();
    if sanitized.is_empty() {
        format!("{SLUG_PREFIX}relay")
    } else {
        format!("{SLUG_PREFIX}{sanitized}")
    }
}

/// 我们写的模型表名前缀。
pub const SLUG_PREFIX: &str = "mcpane-";

/// 一张模型表的字段。`api_key` 与 `env_key` 至少有一个非空，否则 Grok CLI
/// 拒收整份配置。
#[derive(Debug, Clone)]
pub struct GrokModelTable<'a> {
    /// 表名（我们统一用 `mcpane-<slug>`，便于识别与清理）。
    pub slug: &'a str,
    /// 卡片上的显示名。
    pub name: &'a str,
    /// 上游真实模型名。
    pub model: &'a str,
    pub base_url: &'a str,
    /// 明文 Key（写入 config.toml；Grok CLI 无独立凭据文件）。
    pub api_key: Option<&'a str>,
    /// 改从环境变量取 Key 时的变量名（与 api_key 二选一）。
    pub env_key: Option<&'a str>,
    pub api_backend: &'a str,
    pub context_window: i64,
}

/// 写入一张模型表并把默认指针指向它。
pub fn model_table_patch(table: &GrokModelTable<'_>) -> PatchPlan {
    let base = |field: &str| {
        vec![
            MODEL_TABLE_KEY.to_owned(),
            table.slug.to_owned(),
            field.to_owned(),
        ]
    };
    let mut ops = vec![
        PatchOp::Set {
            path: base("model"),
            value: json!(table.model),
        },
        PatchOp::Set {
            path: base("base_url"),
            value: json!(table.base_url),
        },
        PatchOp::Set {
            path: base("name"),
            value: json!(table.name),
        },
        PatchOp::Set {
            path: base("api_backend"),
            value: json!(table.api_backend),
        },
        PatchOp::Set {
            path: base("context_window"),
            value: json!(table.context_window),
        },
    ];
    // 两种凭据形态互斥：写哪个就删掉另一个，避免切换后残留失效的旧形态。
    match (table.api_key, table.env_key) {
        (Some(key), _) => {
            ops.push(PatchOp::Set {
                path: base("api_key"),
                value: json!(key),
            });
            ops.push(PatchOp::Remove {
                path: base("env_key"),
            });
        }
        (None, Some(env)) => {
            ops.push(PatchOp::Set {
                path: base("env_key"),
                value: json!(env),
            });
            ops.push(PatchOp::Remove {
                path: base("api_key"),
            });
        }
        (None, None) => {}
    }
    ops.push(PatchOp::Set {
        path: MODELS_DEFAULT_PATH
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
        value: json!(table.slug),
    });
    PatchPlan::new(ops)
}

/// 断开连接时 `[models].default` 指针的处理方式。
///
/// 指针是否该碰由调用方判断——它得先确认 default 确实指向这张表，否则
/// 会把用户自己设的默认模型一起抹掉。**碰了之后指针该变成什么**是这个
/// 类型要回答的：早期实现只有"清不清空"一个开关，切回官方=无条件删掉
/// 整个指针，哪怕它在 MCPane 接管之前指向的是用户自己配的另一张表——
/// 那张表本身没被删（见 [`remove_model_table_patch`] 只删传入的 `slug`），
/// 但指向它的路标永久丢失，等价于弄丢了用户的选择。
#[derive(Debug, Clone, Copy)]
pub enum DefaultPointerDisposition<'a> {
    /// 指针不指向我们管理的表：不碰（调用方不应该走到这个分支）。
    Untouched,
    /// 找回了 MCPane 接管这个指针之前的原值：还原成它。
    Restore(&'a str),
    /// 指针确实是我们写的，但找不到（或找到的表已被用户删除）接管前的
    /// 原值：只能清空，退回早期"无条件删除"的语义。
    Clear,
}

/// 移除某张模型表；`default` 决定 `[models].default` 指针怎么处理
/// （见 [`DefaultPointerDisposition`]）。
pub fn remove_model_table_patch(slug: &str, default: DefaultPointerDisposition<'_>) -> PatchPlan {
    let mut ops = vec![PatchOp::Remove {
        path: vec![MODEL_TABLE_KEY.to_owned(), slug.to_owned()],
    }];
    let default_path = || {
        MODELS_DEFAULT_PATH
            .iter()
            .map(|s| (*s).to_owned())
            .collect()
    };
    match default {
        DefaultPointerDisposition::Untouched => {}
        DefaultPointerDisposition::Restore(previous) => ops.push(PatchOp::Set {
            path: default_path(),
            value: serde_json::Value::String(previous.to_owned()),
        }),
        DefaultPointerDisposition::Clear => ops.push(PatchOp::Remove {
            path: default_path(),
        }),
    }
    PatchPlan::new(ops)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use mcpane_config::{ConfigDocument, ConfigFormat};

    fn table<'a>(api_key: Option<&'a str>, env_key: Option<&'a str>) -> GrokModelTable<'a> {
        GrokModelTable {
            slug: "mcpane-relay",
            name: "My Relay",
            model: "grok-4.5",
            base_url: "https://api.example.com/v1",
            api_key,
            env_key,
            api_backend: DEFAULT_API_BACKEND,
            context_window: DEFAULT_CONTEXT_WINDOW,
        }
    }

    #[test]
    fn slug_is_sanitized_and_prefixed() {
        assert_eq!(table_slug("My Relay"), "mcpane-my-relay");
        assert_eq!(table_slug("智谱 GLM"), "mcpane-glm");
        assert_eq!(table_slug("***"), "mcpane-relay");
    }

    #[test]
    fn writes_table_and_default_pointer() {
        let mut doc = ConfigDocument::parse("", ConfigFormat::Toml).unwrap();
        model_table_patch(&table(Some("sk-test"), None))
            .apply(&mut doc)
            .unwrap();
        assert_eq!(doc.get(&["models", "default"]).unwrap(), "mcpane-relay");
        assert_eq!(
            doc.get(&["model", "mcpane-relay", "base_url"]).unwrap(),
            "https://api.example.com/v1"
        );
        assert_eq!(
            doc.get(&["model", "mcpane-relay", "api_key"]).unwrap(),
            "sk-test"
        );
        // Grok CLI 要求 context_window 是正整数，不能序列化成字符串。
        assert!(
            doc.get(&["model", "mcpane-relay", "context_window"])
                .unwrap()
                .is_i64()
        );
    }

    #[test]
    fn env_key_form_clears_inline_key() {
        let mut doc = ConfigDocument::parse("", ConfigFormat::Toml).unwrap();
        model_table_patch(&table(Some("sk-test"), None))
            .apply(&mut doc)
            .unwrap();
        model_table_patch(&table(None, Some("XAI_API_KEY")))
            .apply(&mut doc)
            .unwrap();
        assert!(doc.get(&["model", "mcpane-relay", "api_key"]).is_none());
        assert_eq!(
            doc.get(&["model", "mcpane-relay", "env_key"]).unwrap(),
            "XAI_API_KEY"
        );
    }

    #[test]
    fn removal_keeps_user_tables_and_can_clear_default_as_fallback() {
        let source = r#"
[models]
default = "my-own"

[model.my-own]
model = "grok-4"
"#;
        let mut doc = ConfigDocument::parse(source, ConfigFormat::Toml).unwrap();
        model_table_patch(&table(Some("sk"), None))
            .apply(&mut doc)
            .unwrap();
        // 找不到接入前原值（比如历史被轮转掉）时的兜底：清空指针。
        remove_model_table_patch("mcpane-relay", DefaultPointerDisposition::Clear)
            .apply(&mut doc)
            .unwrap();
        assert!(doc.get(&["model", "mcpane-relay"]).is_none());
        assert!(doc.get(&["models", "default"]).is_none());
        // 用户自己的表纹丝不动。
        assert_eq!(doc.get(&["model", "my-own", "model"]).unwrap(), "grok-4");
    }

    /// 断开连接的正常路径：找到了接入前的原值，指针应该被还原而不是清空——
    /// 这是本次要修的缺陷本身（早期实现只会 Clear，永久丢失用户原有默认）。
    #[test]
    fn removal_restores_prior_default_when_known() {
        let source = r#"
[models]
default = "my-own"

[model.my-own]
model = "grok-4"
"#;
        let mut doc = ConfigDocument::parse(source, ConfigFormat::Toml).unwrap();
        model_table_patch(&table(Some("sk"), None))
            .apply(&mut doc)
            .unwrap();
        remove_model_table_patch("mcpane-relay", DefaultPointerDisposition::Restore("my-own"))
            .apply(&mut doc)
            .unwrap();
        assert!(doc.get(&["model", "mcpane-relay"]).is_none());
        assert_eq!(
            doc.get(&["models", "default"]).unwrap(),
            "my-own",
            "断开后必须还原成接入前用户自己的默认模型，而不是清空"
        );
        assert_eq!(doc.get(&["model", "my-own", "model"]).unwrap(), "grok-4");
    }

    #[test]
    fn removal_leaves_default_untouched_when_it_was_never_ours() {
        let source = r#"
[models]
default = "someone-elses-table"

[model.someone-elses-table]
model = "grok-4"

[model.mcpane-relay]
model = "grok-4.5"
"#;
        let mut doc = ConfigDocument::parse(source, ConfigFormat::Toml).unwrap();
        // 调用方已判断 default 不是我们的（不该走 Restore/Clear），只删表本身。
        remove_model_table_patch("mcpane-relay", DefaultPointerDisposition::Untouched)
            .apply(&mut doc)
            .unwrap();
        assert!(doc.get(&["model", "mcpane-relay"]).is_none());
        assert_eq!(
            doc.get(&["models", "default"]).unwrap(),
            "someone-elses-table",
            "指针本就不是我们写的，不该被碰"
        );
    }
}
