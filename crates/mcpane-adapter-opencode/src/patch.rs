//! OpenCode `opencode.json` 的最小 Patch 生成。
//!
//! 只触碰 `provider.<id>` 这一棵子树。additive 语义下同一份配置里住着多个
//! 供应商，写一个绝不能碰其他的——包括用户手写的、以及 `mcp` 等其他顶层键。

use mcpane_config::{PatchOp, PatchPlan};
use serde_json::json;

/// provider 表的顶层键。
pub const PROVIDER_TABLE_KEY: &str = "provider";

/// AI SDK 包名：OpenAI 兼容端点（绝大多数中转站）与 Anthropic 系端点各一个。
/// 与同类工具（cc-switch）的取值一致。
pub const DEFAULT_NPM: &str = "@ai-sdk/openai-compatible";
pub const ANTHROPIC_NPM: &str = "@ai-sdk/anthropic";

/// 一条 provider 的字段。
#[derive(Debug, Clone)]
pub struct ProviderEntry<'a> {
    /// provider 表里的键（也是 OpenCode 里的 provider id）。
    pub id: &'a str,
    /// 显示名。
    pub name: &'a str,
    /// AI SDK 包名。
    pub npm: &'a str,
    pub base_url: &'a str,
    pub api_key: &'a str,
    /// 模型 id 列表——OpenCode 只认显式声明过的模型，留空它就一个都选不出来。
    pub models: &'a [String],
    /// 自定义请求头（空表则不写这个键，不留 `"headers": {}` 的噪声）。
    pub headers: &'a std::collections::BTreeMap<String, String>,
}

/// 写入/更新一条 provider。
pub fn provider_entry_patch(entry: &ProviderEntry<'_>) -> PatchPlan {
    let at = |rest: &[&str]| {
        let mut path = vec![PROVIDER_TABLE_KEY.to_owned(), entry.id.to_owned()];
        path.extend(rest.iter().map(|s| (*s).to_owned()));
        path
    };
    let mut ops = vec![
        PatchOp::Set {
            path: at(&["npm"]),
            value: json!(entry.npm),
        },
        PatchOp::Set {
            path: at(&["name"]),
            value: json!(entry.name),
        },
        PatchOp::Set {
            path: at(&["options", "baseURL"]),
            value: json!(entry.base_url),
        },
        PatchOp::Set {
            path: at(&["options", "apiKey"]),
            value: json!(entry.api_key),
        },
        // cc-switch 的 opencode 预设普遍带这个：让 OpenCode 对上游启用
        // 提示缓存键，省 token 也更快。
        PatchOp::Set {
            path: at(&["options", "setCacheKey"]),
            value: json!(true),
        },
    ];
    // 模型表整体重写：编辑时删掉的模型必须跟着消失，逐个 Set 会留下旧条目。
    let models: serde_json::Map<String, serde_json::Value> = entry
        .models
        .iter()
        .map(|id| (id.clone(), json!({ "name": id })))
        .collect();
    ops.push(PatchOp::Set {
        path: at(&["models"]),
        value: serde_json::Value::Object(models),
    });
    // 头表整体重写；空表则移除该键——留个空对象只会让用户以为设过什么。
    ops.push(if entry.headers.is_empty() {
        PatchOp::Remove {
            path: at(&["options", "headers"]),
        }
    } else {
        PatchOp::Set {
            path: at(&["options", "headers"]),
            value: serde_json::Value::Object(
                entry
                    .headers
                    .iter()
                    .map(|(k, v)| (k.clone(), json!(v)))
                    .collect(),
            ),
        }
    });
    PatchPlan::new(ops)
}

/// 从 live 配置移除一条 provider（"从配置移除"，不是删除供应商记录）。
pub fn remove_provider_patch(id: &str) -> PatchPlan {
    PatchPlan::new(vec![PatchOp::Remove {
        path: vec![PROVIDER_TABLE_KEY.to_owned(), id.to_owned()],
    }])
}

/// live 配置里现有的 provider id 列表（判断"这条启用了没有"）。
///
/// 传入的是已解析出的 `provider` 子树——解析交给调用方按
/// [`crate::PROVIDER_TABLE_KEY`] 绑定的格式做（opencode.json 是 JSONC，
/// 这里自己 `serde_json::from_str` 过一遍就会在带注释的配置上静默返回空表，
/// 把已启用的供应商全显示成未启用）。与 openclaw / hermes 同构。
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

    fn entry<'a>(id: &'a str, models: &'a [String]) -> ProviderEntry<'a> {
        ProviderEntry {
            id,
            name: "Kimi",
            npm: DEFAULT_NPM,
            base_url: "https://api.moonshot.cn/v1",
            api_key: "sk-test",
            models,
            headers: &EMPTY_HEADERS,
        }
    }

    static EMPTY_HEADERS: std::sync::LazyLock<std::collections::BTreeMap<String, String>> =
        std::sync::LazyLock::new(std::collections::BTreeMap::new);

    #[test]
    fn writes_entry_without_touching_siblings() {
        let source = r#"{"provider":{"mine":{"npm":"@ai-sdk/anthropic"}},"mcp":{"x":{}}}"#;
        let mut doc = ConfigDocument::parse(source, ConfigFormat::Json5).unwrap();
        let models = vec!["kimi-k2.7-code".to_owned()];
        provider_entry_patch(&entry("kimi", &models))
            .apply(&mut doc)
            .unwrap();

        assert_eq!(
            doc.get(&["provider", "kimi", "options", "baseURL"])
                .unwrap(),
            "https://api.moonshot.cn/v1"
        );
        assert_eq!(
            doc.get(&["provider", "kimi", "models", "kimi-k2.7-code", "name"])
                .unwrap(),
            "kimi-k2.7-code"
        );
        // 别人的 provider 与 mcp 表纹丝不动。
        assert_eq!(
            doc.get(&["provider", "mine", "npm"]).unwrap(),
            "@ai-sdk/anthropic"
        );
        assert!(doc.get(&["mcp", "x"]).is_some());
    }

    #[test]
    fn model_table_is_replaced_not_merged() {
        let mut doc = ConfigDocument::parse("{}", ConfigFormat::Json5).unwrap();
        let first = vec!["a".to_owned(), "b".to_owned()];
        provider_entry_patch(&entry("kimi", &first))
            .apply(&mut doc)
            .unwrap();
        let second = vec!["a".to_owned()];
        provider_entry_patch(&entry("kimi", &second))
            .apply(&mut doc)
            .unwrap();
        assert!(doc.get(&["provider", "kimi", "models", "a"]).is_some());
        assert!(
            doc.get(&["provider", "kimi", "models", "b"]).is_none(),
            "编辑时删掉的模型必须跟着消失"
        );
    }

    #[test]
    fn removal_only_drops_its_own_entry() {
        let source = r#"{"provider":{"kimi":{"npm":"x"},"mine":{"npm":"y"}}}"#;
        let mut doc = ConfigDocument::parse(source, ConfigFormat::Json5).unwrap();
        remove_provider_patch("kimi").apply(&mut doc).unwrap();
        assert!(doc.get(&["provider", "kimi"]).is_none());
        assert!(doc.get(&["provider", "mine"]).is_some());
    }

    #[test]
    fn headers_written_only_when_present() {
        let mut doc = ConfigDocument::parse("{}", ConfigFormat::Json5).unwrap();
        let models = vec!["m".to_owned()];
        let headers: std::collections::BTreeMap<String, String> =
            [("X-Route".to_owned(), "fast".to_owned())]
                .into_iter()
                .collect();
        let mut e = entry("kimi", &models);
        e.headers = &headers;
        provider_entry_patch(&e).apply(&mut doc).unwrap();
        assert_eq!(
            doc.get(&["provider", "kimi", "options", "headers", "X-Route"])
                .unwrap(),
            "fast"
        );
        // 清空后该键要消失，不留 `"headers": {}`。
        provider_entry_patch(&entry("kimi", &models))
            .apply(&mut doc)
            .unwrap();
        assert!(
            doc.get(&["provider", "kimi", "options", "headers"])
                .is_none()
        );
    }

    #[test]
    fn lists_live_provider_ids() {
        let value = serde_json::json!({ "a": {}, "b": {} });
        assert_eq!(
            provider_ids(Some(&value)),
            vec!["a".to_owned(), "b".to_owned()]
        );
        assert!(provider_ids(None).is_empty());
    }

    /// OpenCode 官方文档声明配置支持 JSONC（示例本身就带 `//` 注释与尾逗号）。
    /// 严格 JSON 解析会在这种文件上直接失败——供应商页打不开、启用态全丢。
    #[test]
    fn reads_and_preserves_official_jsonc_style() {
        let source = "{\n  \"$schema\": \"https://opencode.ai/config.json\",\n  // 我挑的主题\n  \"theme\": \"opencode\",\n  \"provider\": {\n    \"mine\": { \"npm\": \"@ai-sdk/anthropic\" },\n  },\n}\n";
        let mut doc = ConfigDocument::parse(source, ConfigFormat::Json5).unwrap();
        let models = vec!["kimi-k2.7-code".to_owned()];
        provider_entry_patch(&entry("kimi", &models))
            .apply(&mut doc)
            .unwrap();

        let out = doc.serialize();
        assert!(out.contains("// 我挑的主题"), "注释要留住：{out}");
        let doc = ConfigDocument::parse(&out, ConfigFormat::Json5).unwrap();
        assert_eq!(
            doc.get(&["provider", "kimi", "options", "baseURL"])
                .unwrap(),
            "https://api.moonshot.cn/v1"
        );
        assert_eq!(doc.get(&["theme"]).unwrap(), "opencode", "别人的键不动");
        // 启用态判定也得认得出这份文件里的两条 provider。
        assert_eq!(
            provider_ids(doc.get(&[PROVIDER_TABLE_KEY]).as_ref()),
            vec!["mine".to_owned(), "kimi".to_owned()]
        );
    }
}
