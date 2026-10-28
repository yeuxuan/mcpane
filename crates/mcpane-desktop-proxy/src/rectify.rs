//! 请求整流（ADR-022 二期）：上游因 thinking 签名 / budget 约束 / 输出上限 / 思考强度
//! 拒收时，就地修正客户端请求体后对同一候选重试一次。识别规则对照 cc-switch
//! `thinking_rectifier.rs` / `thinking_budget_rectifier.rs`，但刻意更保守：
//! 不以泛化的 "invalid request" 触发；budget 只做满足约束的最小修正。

use serde_json::Value;

/// thinking 签名类错误：故障转移到另一家后，历史里前一家签名的 thinking 块被拒。
pub(crate) fn is_signature_error(body: &str) -> bool {
    let lower = body.to_lowercase();
    let has = |needle: &str| lower.contains(needle);
    (has("signature") && has("thinking") && (has("invalid") || has("not valid")))
        || (has("thought signature") && (has("invalid") || has("not valid")))
        || has("must start with a thinking block")
        || (has("expected") && has("thinking") && has("found") && has("tool_use"))
        || (has("signature") && (has("field required") || has("extra inputs are not permitted")))
        || (has("thinking") && has("cannot be modified"))
        || has("非法请求")
        || has("illegal request")
}

/// 剥离历史里的 thinking / redacted_thinking 块与残留 signature 字段；
/// thinking 仍开启而最后一条带工具调用的 assistant 已无 thinking 开头时关掉 thinking
///（否则上游会以 "must start with a thinking block" 再拒一次）。返回是否有改动。
pub(crate) fn strip_thinking(body: &mut Value) -> bool {
    let mut changed = false;
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return false;
    };
    for msg in messages.iter_mut() {
        let Some(blocks) = msg.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        let before = blocks.len();
        blocks.retain(|b| {
            !matches!(
                b.get("type").and_then(Value::as_str),
                Some("thinking" | "redacted_thinking")
            )
        });
        changed |= blocks.len() != before;
        for block in blocks.iter_mut() {
            if let Some(obj) = block.as_object_mut() {
                changed |= obj.remove("signature").is_some();
            }
        }
    }
    let last_assistant_uses_tools = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .and_then(|m| m.get("content").and_then(Value::as_array))
        .is_some_and(|blocks| {
            blocks
                .iter()
                .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
        });
    let thinking_enabled =
        body.pointer("/thinking/type").and_then(Value::as_str) == Some("enabled");
    if thinking_enabled
        && last_assistant_uses_tools
        && let Some(obj) = body.as_object_mut()
    {
        obj.remove("thinking");
        changed = true;
    }
    changed
}

const MIN_BUDGET: u64 = 1024;
/// 修正后给正文留的余量（max_tokens 必须严格大于 budget_tokens）。
const ANSWER_HEADROOM: u64 = 8192;

/// thinking budget 约束类错误（budget 过小，或 max_tokens 不大于 budget）。
pub(crate) fn is_budget_error(body: &str) -> bool {
    let lower = body.to_lowercase();
    (lower.contains("budget_tokens") || lower.contains("budget tokens"))
        && (lower.contains("thinking") || lower.contains("max_tokens"))
}

/// 最小修正：budget 抬到 1024，max_tokens 不足时抬到 budget + 余量。返回是否有改动。
pub(crate) fn fix_budget(body: &mut Value) -> bool {
    if body.pointer("/thinking/type").and_then(Value::as_str) != Some("enabled") {
        return false;
    }
    let budget = body
        .pointer("/thinking/budget_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let fixed_budget = budget.max(MIN_BUDGET);
    let max_tokens = body.get("max_tokens").and_then(Value::as_u64).unwrap_or(0);
    let mut changed = false;
    if fixed_budget != budget {
        body["thinking"]["budget_tokens"] = Value::from(fixed_budget);
        changed = true;
    }
    if max_tokens <= fixed_budget {
        body["max_tokens"] = Value::from(fixed_budget + ANSWER_HEADROOM);
        changed = true;
    }
    changed
}

/// 输出上限类错误里的上限值：各家措辞不一（`64000 > 32000, which is the maximum`、
/// `valid range of max_tokens is [1, 8192]`、`supports at most 32768`），取小于本次
/// 请求值的最大整数；认不出返回 None。
pub(crate) fn max_tokens_limit(body: &str, requested: u64) -> Option<u64> {
    let lower = body.to_lowercase();
    let about_output = [
        "max_tokens",
        "max_output_tokens",
        "max_completion_tokens",
        "output tokens",
    ]
    .iter()
    .any(|k| lower.contains(k));
    let says_limit = [
        "maximum",
        "at most",
        "valid range",
        "too large",
        "exceed",
        "<=",
        "less than or equal",
    ]
    .iter()
    .any(|k| lower.contains(k));
    if !about_output || !says_limit || lower.contains("budget") {
        return None;
    }
    lower
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|n| n.parse::<u64>().ok())
        .filter(|n| (MIN_OUTPUT..requested).contains(n))
        .max()
}

/// 输出上限不低于它才值得重试（再小基本是误读了消息里的其他数字）。
const MIN_OUTPUT: u64 = 256;

/// 把输出上限降到 `limit`：Claude 写 `max_tokens` 并把 thinking 预算压到其下
///（压不到 1024 以上就关思考），Codex 写 `max_output_tokens`。返回是否有改动。
pub(crate) fn fix_max_tokens(body: &mut Value, codex: bool, limit: u64) -> bool {
    if codex {
        body["max_output_tokens"] = Value::from(limit);
        return true;
    }
    body["max_tokens"] = Value::from(limit);
    let budget = body
        .pointer("/thinking/budget_tokens")
        .and_then(Value::as_u64);
    // 预算至少给正文留出余量（上限的一半封顶），否则思考吃满上限、正文被截断。
    let target = limit.saturating_sub(ANSWER_HEADROOM.min(limit / 2));
    if budget.is_some_and(|b| b > target) {
        if target >= MIN_BUDGET {
            body["thinking"]["budget_tokens"] = Value::from(target);
        } else if let Some(obj) = body.as_object_mut() {
            obj.remove("thinking");
        }
    }
    true
}

/// 思考强度取值被拒（多为模型不支持 xhigh / max，或根本不支持 effort）。
pub(crate) fn is_effort_error(body: &str) -> bool {
    let lower = body.to_lowercase();
    lower.contains("effort")
        && [
            "unsupported",
            "not supported",
            "invalid",
            "unknown",
            "must be one of",
        ]
        .iter()
        .any(|k| lower.contains(k))
}

/// xhigh / max 降为 high；已是常规档仍被拒则去掉强度（交给上游默认）。返回是否有改动。
pub(crate) fn fix_effort(body: &mut Value, codex: bool) -> bool {
    let (parent, field) = if codex {
        ("/reasoning", "effort")
    } else {
        ("/output_config", "effort")
    };
    let current = body
        .pointer(&format!("{parent}/{field}"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    match current.as_deref() {
        Some("xhigh" | "max") => body
            .pointer_mut(parent)
            .and_then(Value::as_object_mut)
            .map(|obj| obj.insert(field.into(), Value::from("high")))
            .is_some(),
        Some(_) => body
            .pointer_mut(parent)
            .and_then(Value::as_object_mut)
            .is_some_and(|obj| obj.remove(field).is_some()),
        // Claude 未显式给 effort：强度来自 thinking，关掉它。
        None if !codex => body
            .as_object_mut()
            .is_some_and(|obj| obj.remove("thinking").is_some()),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use serde_json::json;

    #[test]
    fn signature_errors_detected_conservatively() {
        assert!(is_signature_error(
            r#"{"error":{"message":"messages.1.content.0: Invalid `signature` in `thinking` block"}}"#
        ));
        assert!(is_signature_error(
            "Expected `thinking` or `redacted_thinking`, but found `tool_use`"
        ));
        assert!(is_signature_error(
            "messages.1.content.0.signature: Field required"
        ));
        assert!(!is_signature_error(
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: must be positive"}}"#
        ));
        assert!(!is_signature_error("Invalid request: prompt is too long"));
    }

    #[test]
    fn strip_thinking_removes_blocks_signatures_and_disables_thinking_when_needed() {
        let mut body = json!({
            "thinking": {"type": "enabled", "budget_tokens": 2048},
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "x", "signature": "sig-a"},
                    {"type": "redacted_thinking", "data": "..."},
                    {"type": "text", "text": "ok", "signature": "stray"},
                    {"type": "tool_use", "id": "t", "name": "Bash", "input": {}}
                ]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t", "content": "a"}]}
            ]
        });
        assert!(strip_thinking(&mut body));
        let blocks = body["messages"][1]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert!(blocks[0].get("signature").is_none());
        assert!(
            body.get("thinking").is_none(),
            "工具链路缺 thinking 开头时须关闭 thinking"
        );

        let mut plain = json!({"messages": [{"role": "user", "content": "hi"}]});
        assert!(!strip_thinking(&mut plain));
    }

    #[test]
    fn budget_fix_is_minimal() {
        assert!(is_budget_error(
            "thinking.budget_tokens: Input should be greater than or equal to 1024"
        ));
        assert!(is_budget_error(
            "`max_tokens` must be greater than `thinking.budget_tokens`"
        ));
        assert!(!is_budget_error("rate limit exceeded"));

        let mut small =
            json!({"max_tokens": 4000, "thinking": {"type": "enabled", "budget_tokens": 500}});
        assert!(fix_budget(&mut small));
        assert_eq!(small["thinking"]["budget_tokens"], 1024);
        assert_eq!(small["max_tokens"], 4000, "已满足约束的 max_tokens 不动");

        let mut tight =
            json!({"max_tokens": 2000, "thinking": {"type": "enabled", "budget_tokens": 2000}});
        assert!(fix_budget(&mut tight));
        assert_eq!(tight["max_tokens"], 2000 + ANSWER_HEADROOM);

        let mut off = json!({"max_tokens": 10, "thinking": {"type": "disabled"}});
        assert!(!fix_budget(&mut off));
    }

    #[test]
    fn max_tokens_limit_parses_common_wordings() {
        let anthropic = "max_tokens: 64000 > 32000, which is the maximum allowed number of output tokens for claude-opus-4-1-20250805";
        assert_eq!(max_tokens_limit(anthropic, 64000), Some(32000));
        let deepseek = "Invalid max_tokens value, the valid range of max_tokens is [1, 8192]";
        assert_eq!(max_tokens_limit(deepseek, 32000), Some(8192));
        let openai = "max_tokens is too large: 64000. This model supports at most 16384 completion tokens, whereas you provided 64000.";
        assert_eq!(max_tokens_limit(openai, 64000), Some(16384));
        assert_eq!(
            max_tokens_limit(
                "max_tokens must be greater than thinking.budget_tokens",
                8000
            ),
            None
        );
        assert_eq!(max_tokens_limit("context length exceeded", 8000), None);
        assert_eq!(
            max_tokens_limit(deepseek, 4096),
            None,
            "已低于上限就不是这个原因"
        );
    }

    #[test]
    fn fix_max_tokens_clamps_thinking_budget() {
        let mut body = json!({ "max_tokens": 64000, "thinking": { "type": "enabled", "budget_tokens": 31999 } });
        assert!(fix_max_tokens(&mut body, false, 32000));
        assert_eq!(body["max_tokens"], 32000);
        assert_eq!(body["thinking"]["budget_tokens"], 23808);
        let mut small = json!({ "max_tokens": 32000, "thinking": { "type": "enabled", "budget_tokens": 16000 } });
        assert!(fix_max_tokens(&mut small, false, 2000));
        assert!(
            small.get("thinking").is_none(),
            "预算压不到 1024 以上就关思考"
        );
        let mut codex = json!({ "reasoning": { "effort": "high" } });
        assert!(fix_max_tokens(&mut codex, true, 8192));
        assert_eq!(codex["max_output_tokens"], 8192);
    }

    #[test]
    fn effort_rejection_downgrades_then_drops() {
        assert!(is_effort_error(
            "Unsupported value: 'reasoning_effort' does not support 'xhigh' with this model. Supported values are: 'low', 'medium', and 'high'."
        ));
        assert!(!is_effort_error("rate limit exceeded"));
        let mut claude =
            json!({ "output_config": { "effort": "max" }, "thinking": { "type": "adaptive" } });
        assert!(fix_effort(&mut claude, false));
        assert_eq!(claude["output_config"]["effort"], "high");
        let mut codex = json!({ "reasoning": { "effort": "high", "summary": "auto" } });
        assert!(fix_effort(&mut codex, true));
        assert!(codex["reasoning"].get("effort").is_none());
        let mut implicit = json!({ "thinking": { "type": "adaptive" } });
        assert!(fix_effort(&mut implicit, false));
        assert!(implicit.get("thinking").is_none());
        assert!(!fix_effort(&mut json!({}), true));
    }
}
