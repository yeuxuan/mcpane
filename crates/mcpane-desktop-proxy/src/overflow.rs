//! 上下文超限错误归一（ADR-022 §10）。
//!
//! Claude Code 靠错误里的「prompt is too long」、Codex 靠错误码 `context_length_exceeded`
//! 判定「对话装不下了」并自动压缩后重试。各家中转站措辞五花八门（「Range of input length
//! should be」「maximum context length」「输入过长」…），客户端认不出就不压缩，
//! 会话只能卡在这条错误上。这里把确凿的超限错误改写成客户端自己的 API 会说的那种形态。
//!
//! 宁可漏判也不误判：误判成超限会让客户端反复压缩一段本来没问题的对话。

use axum::body::Body;
use axum::http::{Response, StatusCode};
use serde_json::{Value, json};

use mcpane_provider::ClientApp;

use crate::server::anthropic_error;

/// 单个措辞命中即算（均已小写）。
const PHRASES: &[&str] = &[
    "context_length_exceeded",
    "prompt is too long",
    "input is too long",
    "prompt too long",
    "prompt tokens too long",
    "maximum context length",
    "context length exceeded",
    "exceeds the context window",
    "exceeded the context window",
    "exceeds the context length",
    "exceeds the model's context",
    "exceeds the maximum context",
    "context window exceeded",
    "context size has been exceeded",
    "range of input length",
    "maximum prompt length",
    "reduce the length of the messages",
    "exceeded model token limit",
    "exceeds max length",
    "exceeds the maximum number of input tokens",
    "input token count exceeds",
    "maximum allowed input length",
    "longer than the model's context length",
    "available context size",
    "model_context_window_exceeded",
    "超出上下文",
    "超过上下文",
    "超出模型上下文",
    "超过最大上下文",
    "上下文长度超",
    "上下文窗口超",
    "输入内容过长",
    "输入过长",
    "输入长度超",
];

/// 同时出现两个词才算（单词太泛，必须成对）。
const PAIRS: &[(&str, &str)] = &[
    ("input length", "should be"),
    ("input tokens", "exceed"),
    ("total tokens", "exceed max"),
    ("tokens", "exceeds the maximum"),
    ("context", "too long"),
];

/// 限速类措辞：说的是「多快 / 多少次」，不是「这一条多长」。
/// 把限速当超限，Claude Code 会压缩了再压缩，直到判定在空转而放弃。
const PACE_WORDS: &[&str] = &[
    "rate limit",
    "rate-limit",
    "ratelimit",
    "per minute",
    "per hour",
    "per day",
    "tokens per",
    "please wait",
    "try again later",
    "throttl",
    "too many requests",
    "限流",
    "请求过于频繁",
];

/// 这是不是一条「对话装不下」的错误。
pub(crate) fn is_context_overflow(status: u16, text: &str) -> bool {
    if !matches!(status, 400 | 413 | 422) {
        return false;
    }
    let lower = text.to_lowercase();
    // 说的是回复额度（max_tokens）而不是对话长度，压缩没用。
    if ["max_tokens", "max_output_tokens", "max_completion_tokens"]
        .iter()
        .any(|word| lower.contains(word))
    {
        return false;
    }
    if PACE_WORDS.iter().any(|word| lower.contains(word)) {
        return false;
    }
    PHRASES.iter().any(|phrase| lower.contains(phrase))
        || PAIRS
            .iter()
            .any(|(a, b)| lower.contains(a) && lower.contains(b))
}

/// 上游错误若是超限，返回按客户端协议改写后的错误响应；否则 `None`（原样处理）。
/// 已经是客户端认得的形态时同样返回 `None`，避免动良构的错误。
pub(crate) fn normalize(
    app: ClientApp,
    status: StatusCode,
    message: &str,
    raw_body: &[u8],
) -> Option<Response<Body>> {
    if app == ClientApp::Gemini || !is_context_overflow(status.as_u16(), message) {
        return None;
    }
    let bad_request = StatusCode::BAD_REQUEST;
    if app == ClientApp::Claude {
        if status == bad_request && message.to_lowercase().contains("prompt is too long") {
            return None;
        }
        let message = if message.to_lowercase().contains("prompt is too long") {
            message.to_owned()
        } else {
            format!("prompt is too long: {message}")
        };
        return Some(anthropic_error(
            bad_request,
            "invalid_request_error",
            &message,
        ));
    }
    let already_ok = status == bad_request
        && serde_json::from_slice::<Value>(raw_body)
            .ok()
            .and_then(|v| {
                v.pointer("/error/code")
                    .and_then(Value::as_str)
                    .map(|code| code == "context_length_exceeded")
            })
            .unwrap_or(false);
    if already_ok {
        return None;
    }
    let body = json!({
        "error": {
            "message": message,
            "type": "invalid_request_error",
            "code": "context_length_exceeded",
            "param": Value::Null,
        }
    });
    Some(
        Response::builder()
            .status(bad_request)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn recognises_vendor_wordings() {
        for text in [
            // OpenAI / Anthropic
            "This model's maximum context length is 128000 tokens. However, your messages resulted in 130000 tokens.",
            r#"{"error":{"code":"context_length_exceeded","message":"x"}}"#,
            "prompt is too long: 210000 tokens > 200000 maximum",
            // DashScope / Volcengine / Zhipu / Kimi / Qianfan 类
            "Range of input length should be [1, 98304]",
            "Total tokens of image and text exceed max message tokens",
            "Prompt exceeds max length",
            "Your request exceeded model token limit: 262144",
            "Prompt tokens too long",
            // 中文
            "输入内容过长，超出模型上下文限制",
            "请求超过最大上下文长度",
        ] {
            assert!(is_context_overflow(400, text), "{text}");
        }
        assert!(is_context_overflow(413, "context_length_exceeded"));
        assert!(is_context_overflow(422, "maximum context length"));
    }

    #[test]
    fn leaves_everything_else_alone() {
        // 回复额度、限速、鉴权、普通参数错误都不是「对话装不下」。
        for text in [
            "max_tokens: 40000 > 32000, which is the maximum allowed number of output tokens",
            "max_completion_tokens is too large, exceeds the context window of the model",
            "Rate limit reached: 30000 tokens per minute (TPM). Please try again later",
            "Too many tokens, please wait before trying again",
            "invalid api key",
            "model not found",
            "messages: field required",
            "请求过于频繁，输入过长的请求会被限流",
        ] {
            assert!(!is_context_overflow(400, text), "{text}");
        }
    }

    #[test]
    fn only_request_errors_count() {
        for status in [200, 401, 403, 429, 500, 502, 503] {
            assert!(
                !is_context_overflow(status, "maximum context length"),
                "{status}"
            );
        }
    }

    async fn body_json(response: Response<Body>) -> (StatusCode, Value) {
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn claude_gets_anthropic_prompt_is_too_long() {
        let response = normalize(
            ClientApp::Claude,
            StatusCode::BAD_REQUEST,
            "Range of input length should be [1, 98304]",
            b"",
        )
        .unwrap();
        let (status, json) = body_json(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["type"], "error");
        assert_eq!(json["error"]["type"], "invalid_request_error");
        assert_eq!(
            json["error"]["message"],
            "prompt is too long: Range of input length should be [1, 98304]"
        );
    }

    #[tokio::test]
    async fn a_413_becomes_a_400_the_client_can_act_on() {
        let response = normalize(
            ClientApp::Claude,
            StatusCode::PAYLOAD_TOO_LARGE,
            "context_length_exceeded",
            b"",
        )
        .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn codex_and_grok_get_the_openai_code() {
        for app in [ClientApp::Codex, ClientApp::Grok] {
            let response = normalize(
                app,
                StatusCode::UNPROCESSABLE_ENTITY,
                "输入内容过长，超出模型上下文限制",
                b"",
            )
            .unwrap();
            let (status, json) = body_json(response).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(json["error"]["code"], "context_length_exceeded");
            assert_eq!(json["error"]["type"], "invalid_request_error");
        }
    }

    #[test]
    fn already_well_formed_errors_and_gemini_are_untouched() {
        assert!(
            normalize(
                ClientApp::Claude,
                StatusCode::BAD_REQUEST,
                "prompt is too long: 210000 tokens > 200000 maximum",
                b"",
            )
            .is_none()
        );
        assert!(
            normalize(
                ClientApp::Codex,
                StatusCode::BAD_REQUEST,
                "maximum context length is 1000 tokens",
                br#"{"error":{"code":"context_length_exceeded","message":"maximum context length is 1000 tokens"}}"#,
            )
            .is_none()
        );
        assert!(
            normalize(
                ClientApp::Gemini,
                StatusCode::BAD_REQUEST,
                "context_length_exceeded",
                b"",
            )
            .is_none()
        );
        assert!(
            normalize(
                ClientApp::Claude,
                StatusCode::BAD_REQUEST,
                "invalid api key",
                b""
            )
            .is_none()
        );
    }
}
