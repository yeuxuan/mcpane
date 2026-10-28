//! 协议转换中的图片处理：工具结果图片搬运、纯文本模型的图片降级（ADR-022 二期）。

use serde_json::{Value, json};

/// 工具结果图片搬运后，tool 消息文本里补的占位提示。
const TOOL_IMAGE_MOVED_MARKER: &str = "[图片见下一条消息]";
/// 纯文本模型降级时的图片占位文案。
const STRIP_IMAGE_PLACEHOLDER: &str = "[图片已省略：当前模型不支持图片输入]";

// ---------------------------------------------------------------------------
// Task 1：工具结果图片搬运（tool_result / function_call_output 内嵌图片）
// ---------------------------------------------------------------------------

/// 取 Anthropic `image` 块的图片 URL（base64 → data URL，url → 原样）。
pub(crate) fn anthropic_image_url(block: &Value) -> Option<String> {
    let source = block.get("source")?;
    match source.get("type").and_then(Value::as_str) {
        Some("base64") => Some(format!(
            "data:{};base64,{}",
            source
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("image/png"),
            source.get("data").and_then(Value::as_str)?
        )),
        Some("url") => source.get("url").and_then(Value::as_str).map(str::to_owned),
        _ => None,
    }
}

/// 取 Responses `input_image` part 的图片 URL（字符串或 `{url}` 对象）。
pub(crate) fn responses_input_image_url(part: &Value) -> Option<String> {
    match part.get("image_url")? {
        Value::String(s) => Some(s.clone()),
        Value::Object(obj) => obj.get("url").and_then(Value::as_str).map(str::to_owned),
        _ => None,
    }
}

/// 拆分 Anthropic `tool_result` 的 content 数组：文本块拼接返回，`image` 块的
/// URL 收集到第二项；命中图片时在文本末尾附加搬运提示。
pub(crate) fn split_anthropic_tool_result_content(items: &[Value]) -> (String, Vec<String>) {
    let mut texts = Vec::new();
    let mut urls = Vec::new();
    for item in items {
        if item.get("type").and_then(Value::as_str) == Some("image") {
            if let Some(url) = anthropic_image_url(item) {
                urls.push(url);
            }
            continue;
        }
        if let Some(text) = item.get("text").and_then(Value::as_str) {
            texts.push(text);
        }
    }
    (join_with_marker(&texts, &urls), urls)
}

/// 拆分 Responses `function_call_output`/`custom_tool_call_output` 的 output
/// 数组：文本块拼接返回，`input_image` part 的 URL 收集到第二项。
pub(crate) fn split_responses_tool_output_parts(parts: &[Value]) -> (String, Vec<String>) {
    let mut texts = Vec::new();
    let mut urls = Vec::new();
    for part in parts {
        if part.get("type").and_then(Value::as_str) == Some("input_image") {
            if let Some(url) = responses_input_image_url(part) {
                urls.push(url);
            }
            continue;
        }
        if let Some(text) = part.get("text").and_then(Value::as_str) {
            texts.push(text);
        }
    }
    (join_with_marker(&texts, &urls), urls)
}

fn join_with_marker(texts: &[&str], urls: &[String]) -> String {
    let mut text = texts.join("\n");
    if !urls.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(TOOL_IMAGE_MOVED_MARKER);
    }
    text
}

/// 累积一段连续 tool 消息搬运出来的图片，收尾时一次性 flush 成单条合成
/// user 消息（OpenAI 要求 tool 消息紧跟在带 tool_calls 的 assistant 消息后，
/// 图片只能挪到它们之后）。
#[derive(Default)]
pub(crate) struct PendingToolImages {
    content: Vec<Value>,
}

impl PendingToolImages {
    /// 登记一个 tool_call 搬运出的图片；`urls` 为空时不产生任何内容。
    pub(crate) fn push(&mut self, tool_call_id: &str, urls: Vec<String>) {
        if urls.is_empty() {
            return;
        }
        self.content.push(json!({
            "type": "text",
            "text": format!("工具结果中的图片（tool_call_id: {tool_call_id}）：")
        }));
        self.content.extend(
            urls.into_iter()
                .map(|url| json!({ "type": "image_url", "image_url": { "url": url } })),
        );
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.content.is_empty()
    }

    /// 有累积图片时生成合成 user 消息并清空缓冲，否则返回 `None`。
    pub(crate) fn take_message(&mut self) -> Option<Value> {
        if self.content.is_empty() {
            return None;
        }
        Some(json!({ "role": "user", "content": std::mem::take(&mut self.content) }))
    }
}

// ---------------------------------------------------------------------------
// Task 2：纯文本模型的图片降级
// ---------------------------------------------------------------------------

/// 上游错误是否为「不支持图片输入」类。
pub(crate) fn is_unsupported_image_error(status: u16, body: &str) -> bool {
    if !matches!(status, 400 | 415 | 422 | 501) {
        return false;
    }
    let message = extract_error_text(body).to_ascii_lowercase();

    // 自证性表述：本身就断言「仅接受文本」，无需再要求出现 image 等字样。
    const SELF_EVIDENT_HINTS: &[&str] = &["only support text", "only supports text"];
    if SELF_EVIDENT_HINTS.iter().any(|hint| message.contains(hint)) {
        return true;
    }

    let mentions_image = [
        "image",
        "vision",
        "multimodal",
        "multi-modal",
        "modality",
        "modalities",
        "media",
        "attachment",
    ]
    .iter()
    .any(|kw| message.contains(kw));
    if !mentions_image {
        return false;
    }

    const UNSUPPORTED_HINTS: &[&str] = &[
        "unsupported",
        "not supported",
        "does not support",
        "doesn't support",
        "do not support",
        "don't support",
        "text only",
        "text-only",
        "invalid content type",
        "invalid message content",
        "unknown variant",
        "unknown content type",
        "unrecognized content type",
        "cannot process",
        "cannot handle",
        "can't process",
        "can't handle",
        "unable to process",
    ];
    UNSUPPORTED_HINTS.iter().any(|hint| message.contains(hint))
}

/// 从错误体里取可读文本（`error.message` / `message` / `detail` / `error`
/// 优先，取不到就整体序列化）；namespace.rs 的拒收识别复用同一套提取逻辑。
pub(crate) fn extract_error_text(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return body.to_owned();
    };
    [
        value.pointer("/error/message"),
        value.pointer("/message"),
        value.pointer("/detail"),
        value.pointer("/error"),
    ]
    .into_iter()
    .flatten()
    .find_map(Value::as_str)
    .map_or_else(|| value.to_string(), str::to_owned)
}

/// 把请求体中的图片替换为文字占位，返回替换数量。按客户端请求格式识别：
/// Anthropic messages（含 tool_result 内嵌）、Responses input（含
/// function_call_output 数组）、Chat messages（image_url）、Gemini contents。
pub(crate) fn strip_images(body: &mut Value) -> usize {
    let mut count = 0;
    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages {
            if let Some(content) = message.get_mut("content") {
                count += strip_images_in_content(content);
            }
        }
    }
    if let Some(input) = body.get_mut("input") {
        count += strip_images_in_responses_input(input);
    }
    if let Some(contents) = body.get_mut("contents").and_then(Value::as_array_mut) {
        for content in contents {
            count += strip_images_in_gemini_content(content);
        }
    }
    count
}

/// Anthropic/Chat 共用的 content 数组遍历：`image`/`image_url`/`input_image`
/// 块原地替换为占位文本；`tool_result` 等带嵌套 content 的块递归处理。
fn strip_images_in_content(content: &mut Value) -> usize {
    let Some(blocks) = content.as_array_mut() else {
        return 0;
    };
    let mut count = 0;
    for block in blocks.iter_mut() {
        let block_type = block.get("type").and_then(Value::as_str);
        if matches!(block_type, Some("image" | "image_url" | "input_image")) {
            *block = json!({ "type": "text", "text": STRIP_IMAGE_PLACEHOLDER });
            count += 1;
            continue;
        }
        if let Some(nested) = block.get_mut("content") {
            count += strip_images_in_content(nested);
        }
    }
    count
}

fn strip_images_in_responses_input(input: &mut Value) -> usize {
    match input {
        Value::Array(items) => items.iter_mut().map(strip_images_in_responses_item).sum(),
        Value::Object(_) => strip_images_in_responses_item(input),
        _ => 0,
    }
}

fn strip_images_in_responses_item(item: &mut Value) -> usize {
    if item.get("type").and_then(Value::as_str) == Some("input_image") {
        *item = json!({ "type": "input_text", "text": STRIP_IMAGE_PLACEHOLDER });
        return 1;
    }
    let mut count = 0;
    if let Some(content) = item.get_mut("content") {
        count += strip_images_in_content(content);
    }
    if let Some(Value::Array(parts)) = item.get_mut("output") {
        for part in parts.iter_mut() {
            if part.get("type").and_then(Value::as_str) == Some("input_image") {
                *part = json!({ "type": "input_text", "text": STRIP_IMAGE_PLACEHOLDER });
                count += 1;
            }
        }
    }
    count
}

fn strip_images_in_gemini_content(content: &mut Value) -> usize {
    let Some(parts) = content.get_mut("parts").and_then(Value::as_array_mut) else {
        return 0;
    };
    let mut count = 0;
    for part in parts.iter_mut() {
        if gemini_part_is_image(part) {
            *part = json!({ "text": STRIP_IMAGE_PLACEHOLDER });
            count += 1;
        }
    }
    count
}

fn gemini_part_is_image(part: &Value) -> bool {
    gemini_media_is_image(part.get("inlineData").or_else(|| part.get("inline_data")))
        || gemini_media_is_image(part.get("fileData").or_else(|| part.get("file_data")))
}

fn gemini_media_is_image(payload: Option<&Value>) -> bool {
    payload
        .and_then(|p| p.get("mimeType").or_else(|| p.get("mime_type")))
        .and_then(Value::as_str)
        .is_some_and(|mime| mime.to_ascii_lowercase().starts_with("image/"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn anthropic_image_url_covers_base64_and_url_sources() {
        let base64 =
            json!({ "source": { "type": "base64", "media_type": "image/jpeg", "data": "AAA" } });
        assert_eq!(
            anthropic_image_url(&base64).unwrap(),
            "data:image/jpeg;base64,AAA"
        );
        let url = json!({ "source": { "type": "url", "url": "https://x/y.png" } });
        assert_eq!(anthropic_image_url(&url).unwrap(), "https://x/y.png");
        assert!(anthropic_image_url(&json!({})).is_none());
    }

    #[test]
    fn responses_input_image_url_covers_string_and_object() {
        assert_eq!(
            responses_input_image_url(&json!({ "image_url": "https://x/y.png" })).unwrap(),
            "https://x/y.png"
        );
        assert_eq!(
            responses_input_image_url(&json!({ "image_url": { "url": "https://x/z.png" } }))
                .unwrap(),
            "https://x/z.png"
        );
        assert!(responses_input_image_url(&json!({})).is_none());
    }

    #[test]
    fn split_anthropic_tool_result_content_moves_images_and_marks_text() {
        let items = vec![
            json!({ "type": "text", "text": "a.txt" }),
            json!({ "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAA" } }),
        ];
        let (text, urls) = split_anthropic_tool_result_content(&items);
        assert_eq!(text, "a.txt\n[图片见下一条消息]");
        assert_eq!(urls, vec!["data:image/png;base64,AAA".to_owned()]);
    }

    #[test]
    fn split_anthropic_tool_result_content_no_images_untouched() {
        let items = vec![json!({ "type": "text", "text": "a.txt" })];
        let (text, urls) = split_anthropic_tool_result_content(&items);
        assert_eq!(text, "a.txt");
        assert!(urls.is_empty());
    }

    #[test]
    fn split_responses_tool_output_parts_moves_images() {
        let parts = vec![
            json!({ "type": "input_text", "text": "ok" }),
            json!({ "type": "input_image", "image_url": "https://x/y.png" }),
        ];
        let (text, urls) = split_responses_tool_output_parts(&parts);
        assert_eq!(text, "ok\n[图片见下一条消息]");
        assert_eq!(urls, vec!["https://x/y.png".to_owned()]);
    }

    #[test]
    fn pending_tool_images_batches_multiple_calls_into_one_message() {
        let mut pending = PendingToolImages::default();
        assert!(pending.is_empty());
        pending.push("t1", vec!["https://x/a.png".to_owned()]);
        pending.push("t2", vec![]); // 空 URL 不产生内容
        pending.push("t2", vec!["https://x/b.png".to_owned()]);
        assert!(!pending.is_empty());
        let msg = pending.take_message().unwrap();
        assert_eq!(msg["role"], "user");
        let content = msg["content"].as_array().unwrap();
        // t1 的文本+图片，t2 的文本+图片，共 4 项。
        assert_eq!(content.len(), 4);
        assert!(content[0]["text"].as_str().unwrap().contains("t1"));
        assert_eq!(content[1]["image_url"]["url"], "https://x/a.png");
        assert!(content[2]["text"].as_str().unwrap().contains("t2"));
        assert_eq!(content[3]["image_url"]["url"], "https://x/b.png");
        assert!(pending.is_empty());
        assert!(pending.take_message().is_none());
    }

    #[test]
    fn strip_images_replaces_anthropic_blocks_including_tool_result() {
        let mut body = json!({
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAA" } },
                    { "type": "tool_result", "tool_use_id": "t1", "content": [
                        { "type": "text", "text": "ok" },
                        { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "BBB" } }
                    ] }
                ]
            }]
        });
        let count = strip_images(&mut body);
        assert_eq!(count, 2);
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], STRIP_IMAGE_PLACEHOLDER);
        assert_eq!(content[1]["content"][1]["text"], STRIP_IMAGE_PLACEHOLDER);
    }

    #[test]
    fn strip_images_replaces_responses_input_image_and_function_output() {
        let mut body = json!({
            "input": [
                { "type": "input_image", "image_url": "https://x/y.png" },
                { "type": "function_call_output", "call_id": "c1", "output": [
                    { "type": "input_text", "text": "ok" },
                    { "type": "input_image", "image_url": "https://x/z.png" }
                ] }
            ]
        });
        let count = strip_images(&mut body);
        assert_eq!(count, 2);
        assert_eq!(body["input"][0]["type"], "input_text");
        assert_eq!(body["input"][0]["text"], STRIP_IMAGE_PLACEHOLDER);
        assert_eq!(body["input"][1]["output"][1]["type"], "input_text");
        assert_eq!(
            body["input"][1]["output"][1]["text"],
            STRIP_IMAGE_PLACEHOLDER
        );
        assert_eq!(body["input"][1]["output"][0]["text"], "ok");
    }

    #[test]
    fn strip_images_replaces_chat_image_url_parts_incl_tool_role() {
        let mut body = json!({
            "messages": [
                { "role": "user", "content": [
                    { "type": "text", "text": "look" },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAA" } }
                ] },
                { "role": "tool", "tool_call_id": "t1", "content": [
                    { "type": "image_url", "image_url": { "url": "https://x/y.png" } }
                ] }
            ]
        });
        let count = strip_images(&mut body);
        assert_eq!(count, 2);
        assert_eq!(body["messages"][0]["content"][1]["type"], "text");
        assert_eq!(body["messages"][1]["content"][0]["type"], "text");
    }

    #[test]
    fn strip_images_replaces_gemini_inline_and_file_data() {
        let mut body = json!({
            "contents": [{
                "role": "user",
                "parts": [
                    { "text": "look" },
                    { "inlineData": { "mimeType": "image/png", "data": "AAA" } },
                    { "inline_data": { "mime_type": "image/webp", "data": "BBB" } },
                    { "fileData": { "mimeType": "image/gif", "fileUri": "gs://x" } },
                    { "file_data": { "mime_type": "image/gif", "file_uri": "gs://y" } }
                ]
            }]
        });
        let count = strip_images(&mut body);
        assert_eq!(count, 4);
        let parts = body["contents"][0]["parts"].as_array().unwrap();
        assert_eq!(parts[0]["text"], "look");
        for part in &parts[1..] {
            assert_eq!(part["text"], STRIP_IMAGE_PLACEHOLDER);
        }
    }

    #[test]
    fn strip_images_noop_without_images() {
        let mut body = json!({ "messages": [{ "role": "user", "content": "hi" }] });
        assert_eq!(strip_images(&mut body), 0);
        assert_eq!(body["messages"][0]["content"], "hi");
    }

    #[test]
    fn is_unsupported_image_error_matches_known_vendor_phrasings() {
        assert!(is_unsupported_image_error(
            400,
            r#"{"error":{"message":"This model does not support image input"}}"#
        ));
        assert!(is_unsupported_image_error(
            400,
            r#"{"error":{"message":"image input is not supported"}}"#
        ));
        assert!(is_unsupported_image_error(
            400,
            r#"{"message":"unsupported content type image_url"}"#
        ));
        assert!(is_unsupported_image_error(
            400,
            r#"{"message":"Invalid content type. image_url is only supported by certain models"}"#
        ));
        assert!(is_unsupported_image_error(
            422,
            r#"{"error":"model does not support vision"}"#
        ));
        // 火山方舟等网关：不含 image/media 字样，且英文缺三单 s。
        assert!(is_unsupported_image_error(
            400,
            r#"{"error":{"message":"Model only support text input"}}"#
        ));
    }

    #[test]
    fn is_unsupported_image_error_rejects_generic_or_unrelated_errors() {
        assert!(!is_unsupported_image_error(
            400,
            r#"{"error":{"message":"Invalid API key"}}"#
        ));
        assert!(!is_unsupported_image_error(
            500,
            r#"{"error":{"message":"image not supported"}}"#
        ));
        assert!(!is_unsupported_image_error(
            400,
            r#"{"error":{"message":"quota exceeded, please check your image credits"}}"#
        ));
        assert!(!is_unsupported_image_error(400, "not json"));
    }
}
