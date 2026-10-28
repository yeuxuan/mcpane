//! Claude Code 的思考强度 → OpenAI `reasoning_effort`（Chat / Responses 转换共用）。
//! 解析顺序：显式 `output_config.effort`（`/effort`）优先，
//! 其次 `thinking`；`adaptive` 取 high 而非 xhigh——xhigh 只有少数模型支持，默认给了会 400。

use serde_json::Value;

pub(crate) fn from_anthropic(body: &Value) -> Option<&'static str> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if let Some(effort) = body
        .pointer("/output_config/effort")
        .and_then(Value::as_str)
    {
        return match effort {
            "low" => Some("low"),
            "medium" => Some("medium"),
            "high" => Some("high"),
            "xhigh" => Some("xhigh"),
            "max" if supports_max_effort(model) => Some("max"),
            "max" => Some("xhigh"),
            _ => None,
        };
    }
    let thinking = body.get("thinking")?;
    match thinking.get("type").and_then(Value::as_str) {
        Some("adaptive") => Some("high"),
        Some("enabled") => Some(
            match thinking.get("budget_tokens").and_then(Value::as_u64) {
                Some(budget) if budget < 4_000 => "low",
                Some(budget) if budget < 16_000 => "medium",
                _ => "high",
            },
        ),
        _ => None,
    }
}

/// 支持 reasoning effort 的 OpenAI 系模型：o 系列、GPT-5 及以后、Grok 4.5 及以后。
/// Chat 上游多为 DeepSeek / Kimi 等，发未知参数可能被严格网关拒收，故按模型门控。
pub(crate) fn supports_reasoning_effort(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    let model = model.rsplit('/').next().unwrap_or(&model);
    let o_series =
        model.starts_with('o') && model.as_bytes().get(1).is_some_and(u8::is_ascii_digit);
    let gpt5_plus = model
        .strip_prefix("gpt-")
        .and_then(|rest| rest.chars().next())
        .is_some_and(|c| c.is_ascii_digit() && c >= '5');
    let grok = model
        .strip_prefix("grok-4.")
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|minor| minor.parse::<u32>().ok())
        .is_some_and(|minor| minor >= 5);
    o_series || gpt5_plus || grok
}

/// OpenAI 推理模型（o 系列 / GPT-5+）的 Chat 接口拒收 `max_tokens`，只认
/// `max_completion_tokens`；Grok 虽支持 effort，仍用 `max_tokens`。
pub(crate) fn uses_max_completion_tokens(model: &str) -> bool {
    supports_reasoning_effort(model)
        && !model
            .to_ascii_lowercase()
            .rsplit('/')
            .next()
            .is_some_and(|base| base.starts_with("grok"))
}

/// 有独立 max 档的模型（与 cc-switch 白名单一致）；其余 max 降为 xhigh。
fn supports_max_effort(model: &str) -> bool {
    matches!(
        model.to_ascii_lowercase().as_str(),
        "gpt-5.6"
            | "gpt-5.6-sol"
            | "gpt-5.6-terra"
            | "gpt-5.6-luna"
            | "gpt-6-astra"
            | "gpt-6-sol"
            | "gpt-6-luna"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn explicit_effort_wins_over_thinking() {
        let body = json!({ "model": "gpt-5.2", "output_config": { "effort": "low" },
                           "thinking": { "type": "enabled", "budget_tokens": 30000 } });
        assert_eq!(from_anthropic(&body), Some("low"));
    }

    #[test]
    fn max_only_kept_for_max_capable_models() {
        let max = |model: &str| {
            from_anthropic(&json!({ "model": model, "output_config": { "effort": "max" } }))
        };
        assert_eq!(max("gpt-6-sol"), Some("max"));
        assert_eq!(max("gpt-5.2"), Some("xhigh"));
    }

    #[test]
    fn thinking_fallbacks() {
        let of = |thinking: Value| from_anthropic(&json!({ "thinking": thinking }));
        assert_eq!(of(json!({ "type": "adaptive" })), Some("high"));
        assert_eq!(
            of(json!({ "type": "enabled", "budget_tokens": 2000 })),
            Some("low")
        );
        assert_eq!(
            of(json!({ "type": "enabled", "budget_tokens": 8000 })),
            Some("medium")
        );
        assert_eq!(of(json!({ "type": "enabled" })), Some("high"));
        assert_eq!(of(json!({ "type": "disabled" })), None);
        assert_eq!(from_anthropic(&json!({})), None);
    }

    #[test]
    fn reasoning_model_detection() {
        for model in [
            "o3",
            "o4-mini",
            "gpt-5",
            "gpt-5.2-codex",
            "openai/gpt-5.1",
            "grok-4.5",
            "grok-4.10",
        ] {
            assert!(supports_reasoning_effort(model), "{model}");
        }
        for model in [
            "gpt-4o",
            "gpt-4.1",
            "deepseek-chat",
            "kimi-k2",
            "grok-4",
            "grok-4.1",
            "omni",
        ] {
            assert!(!supports_reasoning_effort(model), "{model}");
        }
    }
}
