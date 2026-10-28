//! Hermes `config.yaml` 的 Patch 生成。
//!
//! Hermes 的供应商容器是**数组**，不是字典（按 Hermes 自带初始配置与实测，
//! 核对 2026-07-25）：
//!
//! ```yaml
//! custom_providers:
//!   - name: openrouter
//!     base_url: https://openrouter.ai/api/v1
//!     api_key: sk-...
//!     api_mode: chat_completions
//!     model: anthropic/claude-opus-4-8
//!     models:
//!       anthropic/claude-opus-4-8:
//!         context_length: 200000
//! ```
//!
//! 引擎的路径模型是纯 key 寻址，定位不到"数组里 name 等于 X 的那一项"。
//! 这里的解法是**把整个数组当一个值改写**：读出现有数组 → 在这里按 name
//! 增/改/删 → 整体 Set 回去。代价只是历史 diff 的粒度是整块数组，换来的是
//! 不必给引擎引入数组寻址这套地基。
//!
//! 整块改写有个陷阱：**我们不管的字段必须原样带回去**。Hermes 的 provider
//! 条目还认 `max_tokens` / `rate_limit_delay` / `request_timeout_seconds` /
//! `key_env` 等键，用户可能在 Hermes Web UI 里设过；直接用新值替换整个数组
//! 元素会把它们静默抹掉。cc-switch `set_provider` 为此专门做了 on-disk 字段
//! 回补（并有 `set_provider_preserves_unknown_fields_on_update` 回归测试），
//! 我们同样只覆盖自己写的键（见 [`upsert_patch`]）。

use mcpane_config::{PatchOp, PatchPlan};
use serde_json::{Value, json};

/// 供应商数组的键。
pub const CUSTOM_PROVIDERS_KEY: &str = "custom_providers";

/// Hermes v12+ 的供应商**字典**容器（`providers: {name: {...}}`）。
/// 该容器由 Hermes 自身（Web UI）管理，MCPane 只读呈现、永不写入
///（对齐 cc-switch：字典源条目 edit/remove 一律拦截指去 Hermes Web UI）。
pub const PROVIDERS_DICT_KEY: &str = "providers";

/// `api_mode` 默认值：OpenAI Chat Completions 兼容（绝大多数中转站）。
/// 与 cc-switch `HERMES_DEFAULT_API_MODE` 同值。
pub const DEFAULT_API_MODE: &str = "chat_completions";
/// Anthropic Messages 形态的上游（Claude 系端点）。
pub const ANTHROPIC_API_MODE: &str = "anthropic_messages";

/// 一条 provider 的字段。
#[derive(Debug, Clone)]
pub struct ProviderEntry<'a> {
    /// 数组元素的 `name`，也是 Hermes 里引用这家供应商的标识。
    pub name: &'a str,
    pub base_url: &'a str,
    pub api_key: &'a str,
    /// 上游协议形态：[`DEFAULT_API_MODE`] / [`ANTHROPIC_API_MODE`]。
    ///
    /// Hermes 自己的缺省值就是 `chat_completions`，但**必须显式写出来**：
    /// 同一条目上一次可能是 anthropic 形态，不写就留着旧值，请求会以错误的
    /// 报文格式打上游（cc-switch 预设注释同样是「Always written explicitly」）。
    pub api_mode: &'a str,
    /// 模型 id 列表；首个作为该供应商的默认 `model`。
    pub models: &'a [String],
    /// 每个模型的上下文长度；`None` 则不写该键（Hermes 有自己的默认值）。
    pub context_length: Option<u64>,
}

/// 现有数组里的全部 provider name。
pub fn provider_names(providers: Option<&Value>) -> Vec<String> {
    providers
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("name").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// v12+ `providers:` 字典里的一条外部供应商（只读呈现用）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DictProvider {
    /// 字典键即供应商名。
    pub name: String,
    pub base_url: Option<String>,
}

/// 读取 v12+ `providers:` 字典条目（Hermes 托管的外部供应商）。
///
/// 同名遮蔽语义：
/// `custom_providers` 数组条目赢——用户在 MCPane 里的改动权优先，被数组
/// 同名遮蔽的字典键直接不出现（Hermes 运行时同样以数组为准）。
/// 非对象形态（手改坏的配置）容错为空表。
pub fn dict_provider_entries(dict: Option<&Value>, custom: Option<&Value>) -> Vec<DictProvider> {
    let shadowed: Vec<String> = provider_names(custom);
    dict.and_then(Value::as_object)
        .map(|entries| {
            entries
                .iter()
                .filter(|(name, _)| !shadowed.iter().any(|s| s == *name))
                .map(|(name, body)| DictProvider {
                    name: name.clone(),
                    base_url: body
                        .get("base_url")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 写入/更新一条 provider（按 name 匹配就地更新，没有则追加）。
/// `existing` 是 live 配置里读出来的 `custom_providers` 当前值。
///
/// 就地更新时**只覆盖我们自己写的键**，条目上其余键（用户经 Hermes Web UI
/// 设的 `request_timeout_seconds`、`key_env`、`max_tokens` 等）原样保留。
pub fn upsert_patch(existing: Option<&Value>, entry: &ProviderEntry<'_>) -> PatchPlan {
    let mut items: Vec<Value> = existing
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let next = entry_value(entry);
    match items
        .iter()
        .position(|item| item.get("name").and_then(Value::as_str) == Some(entry.name))
    {
        Some(index) => items[index] = merge_over(&items[index], next),
        None => items.push(next),
    }
    PatchPlan::new(vec![PatchOp::Set {
        path: vec![CUSTOM_PROVIDERS_KEY.to_owned()],
        value: Value::Array(items),
    }])
}

/// 移除一条 provider；数组因此变空时把整个键删掉，不留 `custom_providers: []`。
pub fn remove_patch(existing: Option<&Value>, name: &str) -> PatchPlan {
    let items: Vec<Value> = existing
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|item| item.get("name").and_then(Value::as_str) != Some(name))
        .collect();
    let path = vec![CUSTOM_PROVIDERS_KEY.to_owned()];
    PatchPlan::new(vec![if items.is_empty() {
        PatchOp::Remove { path }
    } else {
        PatchOp::Set {
            path,
            value: Value::Array(items),
        }
    }])
}

/// 把 `ours` 覆盖到 `theirs` 上：我们写的键取新值，其余键保留磁盘上的原值。
///
/// 只做**顶层**合并——`models` 是我们完整托管的子树（编辑时删掉的模型必须
/// 跟着消失），逐键合并反而会让旧模型永远清不掉。
fn merge_over(theirs: &Value, ours: Value) -> Value {
    match (theirs.as_object(), ours) {
        (Some(base), Value::Object(overlay)) => {
            let mut merged = base.clone();
            merged.extend(overlay);
            Value::Object(merged)
        }
        // 磁盘上那一项不是映射（配置被手改坏了）：直接用我们的新值，
        // 保底让切换能成功。
        (_, ours) => ours,
    }
}

fn entry_value(entry: &ProviderEntry<'_>) -> Value {
    // models 是 id → 元数据的映射；我们只声明存在性，具体参数交给 Hermes 默认值。
    let models: serde_json::Map<String, Value> = entry
        .models
        .iter()
        .map(|id| {
            let meta = match entry.context_length {
                Some(len) => json!({ "context_length": len }),
                // 不猜数值：留空对象，Hermes 用它自己的默认。
                None => json!({}),
            };
            (id.clone(), meta)
        })
        .collect();
    let mut value = json!({
        "name": entry.name,
        "base_url": entry.base_url,
        "api_key": entry.api_key,
        "api_mode": entry.api_mode,
        "models": models,
    });
    // 默认模型取列表第一个：Hermes 需要一个 `model` 才知道默认用哪个。
    if let Some(first) = entry.models.first()
        && let Some(obj) = value.as_object_mut()
    {
        obj.insert("model".to_owned(), json!(first));
    }
    value
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use mcpane_config::{ConfigDocument, ConfigFormat};

    const SOURCE: &str = "agent:\n  max_turns: 50\ncustom_providers:\n  - name: mine\n    base_url: https://x.example.com\n";

    fn apply(source: &str, plan: PatchPlan) -> ConfigDocument {
        let mut doc = ConfigDocument::parse(source, ConfigFormat::Yaml).unwrap();
        plan.apply(&mut doc).unwrap();
        ConfigDocument::parse(&doc.serialize(), ConfigFormat::Yaml).unwrap()
    }

    #[test]
    fn appends_without_touching_other_entries_or_keys() {
        let doc = ConfigDocument::parse(SOURCE, ConfigFormat::Yaml).unwrap();
        let existing = doc.get(&[CUSTOM_PROVIDERS_KEY]);
        let models = vec!["kimi-k2.7-code".to_owned()];
        let doc = apply(
            SOURCE,
            upsert_patch(
                existing.as_ref(),
                &ProviderEntry {
                    name: "kimi",
                    base_url: "https://api.moonshot.cn/v1",
                    api_key: "sk-test",
                    api_mode: DEFAULT_API_MODE,
                    models: &models,
                    context_length: None,
                },
            ),
        );
        let list = doc.get(&[CUSTOM_PROVIDERS_KEY]).unwrap();
        assert_eq!(list[0]["name"], "mine", "用户自建条目原位保留");
        assert_eq!(list[1]["name"], "kimi");
        assert_eq!(list[1]["model"], "kimi-k2.7-code", "默认模型取列表首项");
        assert!(list[1]["models"]["kimi-k2.7-code"].is_object());
        assert_eq!(
            doc.get(&["agent", "max_turns"]).unwrap(),
            50,
            "其他配置不动"
        );
    }

    #[test]
    fn upsert_replaces_in_place_by_name() {
        let doc = ConfigDocument::parse(SOURCE, ConfigFormat::Yaml).unwrap();
        let models = vec!["m2".to_owned()];
        let doc = apply(
            SOURCE,
            upsert_patch(
                doc.get(&[CUSTOM_PROVIDERS_KEY]).as_ref(),
                &ProviderEntry {
                    name: "mine",
                    base_url: "https://updated.example.com",
                    api_key: "sk",
                    api_mode: DEFAULT_API_MODE,
                    models: &models,
                    context_length: None,
                },
            ),
        );
        let list = doc.get(&[CUSTOM_PROVIDERS_KEY]).unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1, "同名不新增，原位更新");
        assert_eq!(list[0]["base_url"], "https://updated.example.com");
    }

    #[test]
    fn removal_drops_key_when_list_becomes_empty() {
        let doc = ConfigDocument::parse(SOURCE, ConfigFormat::Yaml).unwrap();
        let doc = apply(
            SOURCE,
            remove_patch(doc.get(&[CUSTOM_PROVIDERS_KEY]).as_ref(), "mine"),
        );
        assert!(
            doc.get(&[CUSTOM_PROVIDERS_KEY]).is_none(),
            "空数组不该留成 custom_providers: []"
        );
        assert_eq!(doc.get(&["agent", "max_turns"]).unwrap(), 50);
    }

    #[test]
    fn context_length_written_only_when_given() {
        let models = vec!["m".to_owned()];
        let with = apply(
            SOURCE,
            upsert_patch(
                None,
                &ProviderEntry {
                    name: "k",
                    base_url: "https://x",
                    api_key: "sk",
                    api_mode: DEFAULT_API_MODE,
                    models: &models,
                    context_length: Some(200_000),
                },
            ),
        );
        assert_eq!(
            with.get(&[CUSTOM_PROVIDERS_KEY]).unwrap()[0]["models"]["m"]["context_length"],
            200_000
        );
        // 没给就不写这个键——硬编一个数字就是瞎猜。
        let without = apply(
            SOURCE,
            upsert_patch(
                None,
                &ProviderEntry {
                    name: "k",
                    base_url: "https://x",
                    api_key: "sk",
                    api_mode: DEFAULT_API_MODE,
                    models: &models,
                    context_length: None,
                },
            ),
        );
        assert!(
            without.get(&[CUSTOM_PROVIDERS_KEY]).unwrap()[0]["models"]["m"]
                .get("context_length")
                .is_none()
        );
    }

    /// Hermes 的 provider 条目还有一批我们不管的键（`request_timeout_seconds`、
    /// `key_env`、`max_tokens`、`rate_limit_delay`），用户可能在 Hermes Web UI
    /// 里设过。整块改写数组时必须把它们带回去——对齐 cc-switch
    /// `set_provider_preserves_unknown_fields_on_update`。
    #[test]
    fn upsert_preserves_fields_we_do_not_manage() {
        const WITH_EXTRAS: &str = "custom_providers:\n  - name: acme\n    base_url: https://old.example.com\n    api_key: sk-old\n    request_timeout_seconds: 300\n    key_env: ACME_API_KEY\n    rate_limit_delay: 2\n";
        let doc = ConfigDocument::parse(WITH_EXTRAS, ConfigFormat::Yaml).unwrap();
        let models = vec!["m".to_owned()];
        let doc = apply(
            WITH_EXTRAS,
            upsert_patch(
                doc.get(&[CUSTOM_PROVIDERS_KEY]).as_ref(),
                &ProviderEntry {
                    name: "acme",
                    base_url: "https://new.example.com",
                    api_key: "sk-new",
                    api_mode: DEFAULT_API_MODE,
                    models: &models,
                    context_length: None,
                },
            ),
        );
        let entry = &doc.get(&[CUSTOM_PROVIDERS_KEY]).unwrap()[0];
        assert_eq!(entry["base_url"], "https://new.example.com", "我们的键更新");
        assert_eq!(entry["api_key"], "sk-new");
        assert_eq!(entry["request_timeout_seconds"], 300, "用户设的键必须保留");
        assert_eq!(entry["key_env"], "ACME_API_KEY");
        assert_eq!(entry["rate_limit_delay"], 2);
    }

    /// `api_mode` 恒显式写出：同一条目从 anthropic 形态改回 chat 形态时，
    /// 不写就会留着旧值，报文格式与上游不符。
    #[test]
    fn api_mode_is_always_written_and_switchable() {
        let models = vec!["claude-opus-4-8".to_owned()];
        let anthropic = apply(
            SOURCE,
            upsert_patch(
                None,
                &ProviderEntry {
                    name: "relay",
                    base_url: "https://x",
                    api_key: "sk",
                    api_mode: ANTHROPIC_API_MODE,
                    models: &models,
                    context_length: None,
                },
            ),
        );
        assert_eq!(
            anthropic.get(&[CUSTOM_PROVIDERS_KEY]).unwrap()[0]["api_mode"],
            "anthropic_messages"
        );
        // 改回 chat 形态：合并保留其他键，但 api_mode 必须被覆盖。
        let existing = anthropic.get(&[CUSTOM_PROVIDERS_KEY]);
        let back = apply(
            &anthropic.serialize(),
            upsert_patch(
                existing.as_ref(),
                &ProviderEntry {
                    name: "relay",
                    base_url: "https://x",
                    api_key: "sk",
                    api_mode: DEFAULT_API_MODE,
                    models: &models,
                    context_length: None,
                },
            ),
        );
        assert_eq!(
            back.get(&[CUSTOM_PROVIDERS_KEY]).unwrap()[0]["api_mode"],
            "chat_completions",
            "旧的 anthropic_messages 不能残留"
        );
    }

    #[test]
    fn lists_names() {
        let doc = ConfigDocument::parse(SOURCE, ConfigFormat::Yaml).unwrap();
        assert_eq!(
            provider_names(doc.get(&[CUSTOM_PROVIDERS_KEY]).as_ref()),
            vec!["mine".to_owned()]
        );
        assert!(provider_names(None).is_empty());
    }

    /// v12+ 字典读取：数组同名遮蔽（用户改动权优先）、坏形态容错为空。
    #[test]
    fn dict_entries_respect_array_shadowing_and_tolerate_garbage() {
        const V12: &str = "custom_providers:\n  - name: mine\n    base_url: https://x\nproviders:\n  mine:\n    base_url: https://dict-shadowed\n  webui-one:\n    base_url: https://from-webui\n  keyless: {}\n";
        let doc = ConfigDocument::parse(V12, ConfigFormat::Yaml).unwrap();
        let entries = dict_provider_entries(
            doc.get(&[PROVIDERS_DICT_KEY]).as_ref(),
            doc.get(&[CUSTOM_PROVIDERS_KEY]).as_ref(),
        );
        // "mine" 被数组遮蔽不出现；其余两条按字典序呈现。
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.name != "mine"), "被遮蔽键不得出现");
        let webui = entries.iter().find(|e| e.name == "webui-one").unwrap();
        assert_eq!(webui.base_url.as_deref(), Some("https://from-webui"));
        // base_url 缺失容错为 None，不整条丢弃（名字本身就有呈现价值）。
        assert!(
            entries
                .iter()
                .any(|e| e.name == "keyless" && e.base_url.is_none())
        );

        // 字典形态坏掉（标量/数组）→ 空表，不报错。
        let garbage = serde_json::json!("not-a-map");
        assert!(dict_provider_entries(Some(&garbage), None).is_empty());
        assert!(dict_provider_entries(None, None).is_empty());
    }

    /// upsert/remove 只动数组容器，字典容器一个字节都不碰。
    #[test]
    fn upsert_and_remove_never_touch_dict_container() {
        let models = vec!["m1".to_owned()];
        let plan = upsert_patch(
            None,
            &ProviderEntry {
                name: "new-one",
                base_url: "https://x",
                api_key: "sk",
                api_mode: DEFAULT_API_MODE,
                models: &models,
                context_length: None,
            },
        );
        let touches_dict = |plan: &PatchPlan| {
            plan.ops.iter().any(|op| match op {
                PatchOp::Set { path, .. } | PatchOp::Remove { path } => {
                    path.first().map(String::as_str) == Some(PROVIDERS_DICT_KEY)
                }
            })
        };
        assert!(!touches_dict(&plan));
        assert!(!touches_dict(&remove_patch(None, "any")));
    }
}
