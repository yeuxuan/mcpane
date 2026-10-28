//! OpenClaw `openclaw.json`（JSON5）的最小 Patch 生成。
//!
//! 只触碰 `models.providers.<id>`。additive 下同一份配置里住着多个供应商，
//! 写一个绝不能碰其他的；`models.mode`（merge/override）是用户的全局取舍，
//! 我们同样不碰——它决定 OpenClaw 是合并还是覆盖内置模型表。

use mcpane_config::{PatchOp, PatchPlan};
use serde_json::json;

/// 供应商表的路径：`models.providers`。
pub const PROVIDERS_PATH: [&str; 2] = ["models", "providers"];

/// 默认上游协议：OpenAI Chat Completions 兼容（绝大多数中转站）。
pub const DEFAULT_API: &str = "openai-completions";
/// Anthropic Messages 形态的上游。
pub const ANTHROPIC_API: &str = "anthropic-messages";

/// 一条 provider 的字段。
#[derive(Debug, Clone)]
pub struct ProviderEntry<'a> {
    pub id: &'a str,
    pub base_url: &'a str,
    pub api_key: &'a str,
    /// 上游协议：[`DEFAULT_API`] / [`ANTHROPIC_API`]（与 cc-switch
    /// 的 openclaw 预设同值）。字段本身在 OpenClaw 侧是可选的，但显式写出来
    /// 才能接 Anthropic 系端点。
    pub api: &'a str,
    /// 模型 id 列表——OpenClaw 的 `models` 是数组，元素形如 `{ id, name }`。
    pub models: &'a [String],
    /// 每个模型的原生上下文窗口（`models[].contextWindow`，OpenClaw 官方
    /// config-tools 文档字段）。`None` 则不写该键，用 OpenClaw 自己的默认。
    ///
    /// 不是可选的锦上添花：OpenClaw 拿它判断何时压缩会话，缺省值与真实窗口
    /// 不符时用户会被提前压缩或直接溢出（同 Grok `context_window` 的坑）。
    pub context_window: Option<u64>,
    /// 自定义请求头（空表则不写这个键）。
    pub headers: &'a std::collections::BTreeMap<String, String>,
}

fn provider_path(id: &str, rest: &[&str]) -> Vec<String> {
    let mut path: Vec<String> = PROVIDERS_PATH.iter().map(|s| (*s).to_owned()).collect();
    path.push(id.to_owned());
    path.extend(rest.iter().map(|s| (*s).to_owned()));
    path
}

/// 写入/更新一条 provider。
pub fn provider_entry_patch(entry: &ProviderEntry<'_>) -> PatchPlan {
    // 模型表是数组，整体重写：编辑时删掉的模型必须跟着消失。
    let models: Vec<serde_json::Value> = entry
        .models
        .iter()
        .map(|id| match entry.context_window {
            Some(window) => json!({ "id": id, "name": id, "contextWindow": window }),
            None => json!({ "id": id, "name": id }),
        })
        .collect();
    let headers_op = if entry.headers.is_empty() {
        PatchOp::Remove {
            path: provider_path(entry.id, &["headers"]),
        }
    } else {
        PatchOp::Set {
            path: provider_path(entry.id, &["headers"]),
            value: serde_json::Value::Object(
                entry
                    .headers
                    .iter()
                    .map(|(k, v)| (k.clone(), json!(v)))
                    .collect(),
            ),
        }
    };
    PatchPlan::new(vec![
        headers_op,
        PatchOp::Set {
            path: provider_path(entry.id, &["baseUrl"]),
            value: json!(entry.base_url),
        },
        PatchOp::Set {
            path: provider_path(entry.id, &["api"]),
            value: json!(entry.api),
        },
        PatchOp::Set {
            path: provider_path(entry.id, &["apiKey"]),
            value: json!(entry.api_key),
        },
        PatchOp::Set {
            path: provider_path(entry.id, &["models"]),
            value: serde_json::Value::Array(models),
        },
    ])
}

/// 默认模型指针的路径：`agents.defaults.model.primary`。
pub const DEFAULT_MODEL_PRIMARY_PATH: [&str; 4] = ["agents", "defaults", "model", "primary"];
/// 模型目录（每模型别名等设置）的路径：`agents.defaults.models`。
pub const MODEL_CATALOG_PATH: [&str; 3] = ["agents", "defaults", "models"];

/// OpenClaw 的模型引用形态：`<provider-id>/<model-id>`。
pub fn model_ref(provider_id: &str, model_id: &str) -> String {
    format!("{provider_id}/{model_id}")
}

fn catalog_path(reference: &str) -> Vec<String> {
    let mut path: Vec<String> = MODEL_CATALOG_PATH.iter().map(|s| (*s).to_owned()).collect();
    path.push(reference.to_owned());
    path
}

/// 把声明的模型注册进模型目录，并在需要时把默认模型指针指过来。
///
/// 光写 `models.providers.<id>` 只是"这家可用"，OpenClaw 该用哪个模型看
/// `agents.defaults.model.primary`——不设的话用户启用完发现毫无变化。
///
/// 指针的三条规则（比 cc-switch 多第二条）：
/// 1. **未设** → 指向我们的首个模型（cc-switch 的
///    `if (!existingDefault?.primary)` 同语义）；
/// 2. **指向我们这家、但那个模型已不在声明列表里** → 重指到当前首个模型。
///    编辑供应商删掉原首模型后，指针就成了指向"客户端里已经选不到的模型"的
///    死引用；这一条是我们引入指针后必须自己兜住的；
/// 3. **指向我们这家且模型仍在列表里，或指向别家** → 一律不动（additive
///    语义不抢当前生效项）。
///
/// `existing_catalog` 传 live 里现有的目录键，用于清掉本家已取消声明的旧条目
/// （模型表本身是整块重写的，目录不跟着清就会残留选不到的幽灵模型）。
pub fn default_model_patch(
    provider_id: &str,
    models: &[String],
    existing_primary: Option<&str>,
    existing_catalog: &[String],
) -> PatchPlan {
    let Some(first) = models.first() else {
        return PatchPlan::new(Vec::new());
    };
    let declared: Vec<String> = models.iter().map(|m| model_ref(provider_id, m)).collect();
    let mut ops: Vec<PatchOp> = models
        .iter()
        .zip(&declared)
        .map(|(model, reference)| PatchOp::Set {
            path: catalog_path(reference),
            value: json!({ "alias": model }),
        })
        .collect();
    // 本家已取消声明的旧目录条目：清掉（别家的键一律不碰）。
    let prefix = format!("{provider_id}/");
    for key in existing_catalog {
        if key.starts_with(&prefix) && !declared.contains(key) {
            ops.push(PatchOp::Remove {
                path: catalog_path(key),
            });
        }
    }
    let claim = match existing_primary.filter(|current| !current.is_empty()) {
        // 未设（或空串）→ 认领。
        None => true,
        // 指向我们这家但模型已取消声明 → 死引用，重指。
        Some(current) => current.starts_with(&prefix) && !declared.iter().any(|d| d == current),
    };
    if claim {
        ops.push(PatchOp::Set {
            path: DEFAULT_MODEL_PRIMARY_PATH
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            value: json!(model_ref(provider_id, first)),
        });
    }
    PatchPlan::new(ops)
}

/// 从 live 配置移除一条 provider，连带清掉它在模型目录里的条目。
///
/// `existing_primary` 指向本供应商时一并清空默认模型指针——留着就是一条
/// 指向已不存在的 provider 的死引用（同 `switch_grok_official` 只在
/// `models.default` 确实是我们的表时才清的判定）。用户自己选的其他模型不动。
///
/// `existing_catalog` 传 live 里现有的**全部**目录键（不只是本家的，语义
/// 同 [`default_model_patch`]）：删完本家的条目后，如果目录里一个键都不剩
/// （没有别家还在用），直接删掉整个 `agents.defaults.models` 表，而不是留一
/// 个 `{}` 空壳——那种空壳不影响功能，但会让用户以为配置里有什么东西，
/// 手改配置时平白多一层要看懂的嵌套。
pub fn remove_provider_patch(
    id: &str,
    models: &[String],
    existing_primary: Option<&str>,
    existing_catalog: &[String],
) -> PatchPlan {
    let mut ops = vec![PatchOp::Remove {
        path: provider_path(id, &[]),
    }];
    let prefix = format!("{id}/");
    let catalog_would_be_empty = existing_catalog.iter().all(|key| key.starts_with(&prefix));
    if catalog_would_be_empty {
        ops.push(PatchOp::Remove {
            path: MODEL_CATALOG_PATH.iter().map(|s| (*s).to_owned()).collect(),
        });
    } else {
        for model in models {
            ops.push(PatchOp::Remove {
                path: catalog_path(&model_ref(id, model)),
            });
        }
    }
    if existing_primary.is_some_and(|p| p.starts_with(&prefix)) {
        ops.push(PatchOp::Remove {
            path: DEFAULT_MODEL_PRIMARY_PATH
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
        });
    }
    PatchPlan::new(ops)
}

/// live 配置里现有的 provider id 列表。传入的是已解析出的 providers 子树。
pub fn provider_ids(providers: Option<&serde_json::Value>) -> Vec<String> {
    providers
        .and_then(|v| v.as_object())
        .map(|table| table.keys().cloned().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use mcpane_config::{ConfigDocument, ConfigFormat};

    /// OpenClaw 自带的初始配置：无引号键 + 单引号 + 尾逗号 + 用户注释。
    const SOURCE: &str = "{\n  // 我的配置\n  models: {\n    mode: 'merge',\n    providers: {\n      mine: { baseUrl: 'https://x.example.com' },\n    },\n  },\n}\n";

    #[test]
    fn writes_into_json5_without_disturbing_the_rest() {
        let mut doc = ConfigDocument::parse(SOURCE, ConfigFormat::Json5).unwrap();
        let models = vec!["kimi-k2.7-code".to_owned()];
        provider_entry_patch(&ProviderEntry {
            id: "kimi",
            base_url: "https://api.moonshot.cn/v1",
            api_key: "sk-test",
            api: DEFAULT_API,
            models: &models,
            context_window: None,
            headers: &Default::default(),
        })
        .apply(&mut doc)
        .unwrap();

        let out = doc.serialize();
        assert!(out.contains("// 我的配置"), "注释保留：{out}");
        assert!(out.contains("mode: 'merge'"), "单引号与无引号键不动：{out}");
        let doc = ConfigDocument::parse(&out, ConfigFormat::Json5).unwrap();
        assert_eq!(
            doc.get(&["models", "providers", "kimi", "baseUrl"])
                .unwrap(),
            "https://api.moonshot.cn/v1"
        );
        assert_eq!(
            doc.get(&["models", "providers", "kimi", "models"]).unwrap()[0]["id"],
            "kimi-k2.7-code"
        );
        // 用户自己的 provider 与全局 mode 都在。
        assert_eq!(
            doc.get(&["models", "providers", "mine", "baseUrl"])
                .unwrap(),
            "https://x.example.com"
        );
        assert_eq!(doc.get(&["models", "mode"]).unwrap(), "merge");
        assert_eq!(
            doc.get(&["models", "providers", "kimi", "api"]).unwrap(),
            "openai-completions",
            "上游协议要显式写出来"
        );
    }

    #[test]
    fn removal_only_drops_its_own_entry() {
        let mut doc = ConfigDocument::parse(SOURCE, ConfigFormat::Json5).unwrap();
        let models = vec!["m".to_owned()];
        provider_entry_patch(&ProviderEntry {
            id: "kimi",
            base_url: "https://api.moonshot.cn/v1",
            api_key: "sk",
            api: DEFAULT_API,
            models: &models,
            context_window: None,
            headers: &Default::default(),
        })
        .apply(&mut doc)
        .unwrap();
        remove_provider_patch("kimi", &models, None, &[])
            .apply(&mut doc)
            .unwrap();

        let doc = ConfigDocument::parse(&doc.serialize(), ConfigFormat::Json5).unwrap();
        assert!(doc.get(&["models", "providers", "kimi"]).is_none());
        assert!(doc.get(&["models", "providers", "mine"]).is_some());
    }

    /// `contextWindow` 给了就写、没给就不写——OpenClaw 拿它判断何时压缩会话，
    /// 硬编一个数字就是瞎猜（同 Grok context_window 的坑）。
    #[test]
    fn context_window_written_only_when_given() {
        let models = vec!["m".to_owned()];
        let headers = std::collections::BTreeMap::new();
        let entry = |window| ProviderEntry {
            id: "k",
            base_url: "https://x",
            api_key: "sk",
            api: DEFAULT_API,
            models: &models,
            context_window: window,
            headers: &headers,
        };
        let mut with = ConfigDocument::parse(SOURCE, ConfigFormat::Json5).unwrap();
        provider_entry_patch(&entry(Some(262_144)))
            .apply(&mut with)
            .unwrap();
        assert_eq!(
            with.get(&["models", "providers", "k", "models"]).unwrap()[0]["contextWindow"],
            262_144
        );

        let mut without = ConfigDocument::parse(SOURCE, ConfigFormat::Json5).unwrap();
        provider_entry_patch(&entry(None))
            .apply(&mut without)
            .unwrap();
        assert!(
            without
                .get(&["models", "providers", "k", "models"])
                .unwrap()[0]
                .get("contextWindow")
                .is_none()
        );
    }

    /// 默认模型指针：未设过才写（不抢用户/别家供应商的选择），
    /// 模型目录条目无条件注册。
    #[test]
    fn default_model_only_claims_an_unset_pointer() {
        let models = vec!["m".to_owned()];
        let mut doc = ConfigDocument::parse(SOURCE, ConfigFormat::Json5).unwrap();
        default_model_patch("k", &models, None, &[])
            .apply(&mut doc)
            .unwrap();
        assert_eq!(doc.get(&DEFAULT_MODEL_PRIMARY_PATH).unwrap(), "k/m");
        assert_eq!(
            doc.get(&["agents", "defaults", "models", "k/m", "alias"])
                .unwrap(),
            "m"
        );

        // 已有别家的 primary：只注册目录，指针不动。
        let other_models = vec!["x".to_owned()];
        default_model_patch("other", &other_models, Some("k/m"), &["k/m".to_owned()])
            .apply(&mut doc)
            .unwrap();
        assert_eq!(
            doc.get(&DEFAULT_MODEL_PRIMARY_PATH).unwrap(),
            "k/m",
            "已设的默认模型不能被抢"
        );
        assert!(
            doc.get(&["agents", "defaults", "models", "other/x"])
                .is_some()
        );
    }

    /// 编辑供应商删掉原首模型后，指针会指向一个"客户端里已经选不到的模型"。
    /// 这是我们引入指针后必须自己兜住的死引用（cc-switch 只在添加时设一次，
    /// 没有这一条）：本家指针 + 模型已取消声明 → 重指；旧目录条目跟着清。
    #[test]
    fn default_model_repoints_when_its_own_target_is_undeclared() {
        let mut doc = ConfigDocument::parse(SOURCE, ConfigFormat::Json5).unwrap();
        let before = vec!["old".to_owned(), "keep".to_owned()];
        default_model_patch("k", &before, None, &[])
            .apply(&mut doc)
            .unwrap();
        assert_eq!(doc.get(&DEFAULT_MODEL_PRIMARY_PATH).unwrap(), "k/old");

        // 用户改成只留 keep：指针必须从 k/old 重指到 k/keep，旧目录条目清掉。
        let after = vec!["keep".to_owned()];
        default_model_patch(
            "k",
            &after,
            Some("k/old"),
            &["k/old".to_owned(), "k/keep".to_owned()],
        )
        .apply(&mut doc)
        .unwrap();
        assert_eq!(
            doc.get(&DEFAULT_MODEL_PRIMARY_PATH).unwrap(),
            "k/keep",
            "指向已取消声明的模型是死引用，必须重指"
        );
        assert!(
            doc.get(&["agents", "defaults", "models", "k/old"])
                .is_none(),
            "取消声明的模型不能在目录里留成幽灵"
        );

        // 指针仍指向在声明列表里的模型 → 不动（不抢用户的选择）。
        default_model_patch("k", &after, Some("k/keep"), &["k/keep".to_owned()])
            .apply(&mut doc)
            .unwrap();
        assert_eq!(doc.get(&DEFAULT_MODEL_PRIMARY_PATH).unwrap(), "k/keep");
    }

    /// 从配置移除时清掉自己的目录条目与指向自己的指针，别留死引用；
    /// 指针指向别家时不动。
    #[test]
    fn removal_clears_own_catalog_and_pointer() {
        let models = vec!["m".to_owned()];
        let mut doc = ConfigDocument::parse(SOURCE, ConfigFormat::Json5).unwrap();
        default_model_patch("k", &models, None, &[])
            .apply(&mut doc)
            .unwrap();
        remove_provider_patch("k", &models, Some("k/m"), &["k/m".to_owned()])
            .apply(&mut doc)
            .unwrap();
        assert!(doc.get(&["agents", "defaults", "models", "k/m"]).is_none());
        assert!(doc.get(&DEFAULT_MODEL_PRIMARY_PATH).is_none());

        let mut other = ConfigDocument::parse(SOURCE, ConfigFormat::Json5).unwrap();
        let keep_models = vec!["z".to_owned()];
        default_model_patch("keep", &keep_models, None, &[])
            .apply(&mut other)
            .unwrap();
        remove_provider_patch("k", &models, Some("keep/z"), &["keep/z".to_owned()])
            .apply(&mut other)
            .unwrap();
        assert_eq!(
            other.get(&DEFAULT_MODEL_PRIMARY_PATH).unwrap(),
            "keep/z",
            "别家的默认模型不能被顺手清掉"
        );
        assert!(
            other
                .get(&["agents", "defaults", "models", "keep/z"])
                .is_some(),
            "别家的目录条目不能被顺手清掉"
        );
    }

    /// P0-3 顺带清理：删掉唯一一条目录条目/指针后，`agents.defaults.models`
    /// 与 `agents.defaults.model` 都不该留 `{}` 空壳——那不影响功能，但会让
    /// 手改配置的用户误以为这里还设着什么。
    #[test]
    fn removal_drops_empty_catalog_shell_when_nothing_else_remains() {
        let models = vec!["m".to_owned()];
        let mut doc = ConfigDocument::parse(SOURCE, ConfigFormat::Json5).unwrap();
        default_model_patch("k", &models, None, &[])
            .apply(&mut doc)
            .unwrap();
        assert!(doc.get(&["agents", "defaults", "models"]).is_some());

        remove_provider_patch("k", &models, Some("k/m"), &["k/m".to_owned()])
            .apply(&mut doc)
            .unwrap();
        assert!(
            doc.get(&["agents", "defaults", "models"]).is_none(),
            "唯一的目录条目删完后，目录表本身也该消失：{:?}",
            doc.get(&["agents"])
        );
    }

    #[test]
    fn lists_ids_from_providers_subtree() {
        let value = serde_json::json!({ "a": {}, "b": {} });
        assert_eq!(
            provider_ids(Some(&value)),
            vec!["a".to_owned(), "b".to_owned()]
        );
        assert!(provider_ids(None).is_empty());
    }
}
