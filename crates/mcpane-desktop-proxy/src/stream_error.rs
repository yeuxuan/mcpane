//! 流里的错误事件识别（ADR-022 §10）。
//!
//! 中转站常常先回 200、再在 SSE 里报错（限流、过载、上游超时）。只看响应头会把这类
//! 失败记成成功，熔断永远不触发、也不会转移。这里从**客户端看到的那条流**里认出错误事件，
//! 它已经是客户端协议的形态，转换路径与直通路径都一样处理。

use serde_json::Value;

/// 流里的一条错误。
pub(crate) struct StreamError {
    /// 换一家有可能成功（限流、过载、服务端错误、鉴权失败）。
    /// 请求本身有问题的（参数无效、对话装不下）换哪家都一样，不算供应商的失败。
    pub retryable: bool,
    pub message: String,
}

/// 识别 Anthropic（`type:"error"`）、Responses（`type:"error"` / `response.failed`）、
/// Chat 与 Gemini（顶层 `error` 对象且没有 `choices`）三种形态的错误事件。
pub(crate) fn classify(event: &Value) -> Option<StreamError> {
    let kind = event.get("type").and_then(Value::as_str);
    let error = match kind {
        Some("error") => event
            .get("error")
            .filter(|e| e.is_object())
            .unwrap_or(event),
        Some("response.failed") => event.pointer("/response/error")?,
        _ => event
            .get("error")
            .filter(|e| e.is_object() && event.get("choices").is_none())?,
    };
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("上游在流中报错")
        .to_owned();
    let label = ["type", "code", "status"]
        .iter()
        .filter_map(|key| error.get(*key).and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let request_fault = label.contains("invalid")
        || label.contains("context_length")
        || label.contains("not_found")
        || crate::overflow::is_context_overflow(400, &message);
    Some(StreamError {
        retryable: !request_fault,
        message,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use serde_json::json;

    #[test]
    fn recognises_all_three_protocol_shapes() {
        let anthropic =
            json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}});
        let responses_failed = json!({"type":"response.failed","response":{"status":"failed",
            "error":{"code":"server_error","message":"upstream timeout"}}});
        let responses_error =
            json!({"type":"error","code":"rate_limit_exceeded","message":"slow down"});
        let chat = json!({"error":{"message":"quota exceeded","type":"insufficient_quota"}});
        for (event, expect) in [
            (anthropic, "Overloaded"),
            (responses_failed, "upstream timeout"),
            (responses_error, "slow down"),
            (chat, "quota exceeded"),
        ] {
            let error = classify(&event).unwrap();
            assert!(error.retryable, "{event}");
            assert_eq!(error.message, expect);
        }
    }

    #[test]
    fn request_faults_are_not_the_providers_failure() {
        for event in [
            json!({"type":"error","error":{"type":"invalid_request_error","message":"bad field"}}),
            json!({"type":"response.failed","response":{"error":{"code":"invalid_prompt","message":"x"}}}),
            json!({"type":"response.failed","response":{"error":{"code":"context_length_exceeded","message":"x"}}}),
            json!({"error":{"message":"Range of input length should be [1, 98304]"}}),
        ] {
            assert!(!classify(&event).unwrap().retryable, "{event}");
        }
    }

    #[test]
    fn ordinary_events_are_never_errors() {
        for event in [
            json!({"type":"message_start","message":{"id":"m"}}),
            json!({"type":"content_block_delta","delta":{"text":"hi"}}),
            json!({"type":"response.output_text.delta","delta":"hi"}),
            json!({"type":"response.completed","response":{"status":"completed","error":null}}),
            json!({"choices":[{"delta":{"content":"hi"}}],"error":null}),
            json!({"choices":[{"delta":{}}],"usage":{"total_tokens":3}}),
        ] {
            assert!(classify(&event).is_none(), "{event}");
        }
    }
}
