//! Claude（Anthropic Messages）↔ Chat Completions 协议转换（ADR-022）。
//!
//! 让只提供 Chat 端点的厂商可用于 Claude Code / Claude Desktop：请求转成 Chat，
//! 响应（含 SSE）转回 Anthropic 事件。不做 Copilot 空白死循环检测与
//! o-series 特化；工具结果图片搬运见 `media`。

use axum::body::Bytes;
use futures_util::{Stream, StreamExt};
use serde_json::{Value, json};

/// Claude Code 在 system 开头注入的计费头（每次请求变化，会击穿前缀缓存）。
const BILLING_HEADER_PREFIX: &str = "x-anthropic-billing-header:";

// ---------------------------------------------------------------------------
// 请求：Anthropic → Chat
// ---------------------------------------------------------------------------

pub(crate) fn request_to_chat(body: &Value) -> Value {
    let mut out = json!({});
    let model = body.get("model").and_then(Value::as_str).unwrap_or("");
    if let Some(m) = body.get("model") {
        out["model"] = m.clone();
    }
    let mut messages = Vec::new();
    let system = match body.get("system") {
        Some(Value::String(text)) => strip_billing_header(text).to_owned(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .map(strip_billing_header)
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    if !system.is_empty() {
        messages.push(json!({ "role": "system", "content": system }));
    }
    let mut pending_images = crate::media::PendingToolImages::default();
    for msg in body
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
        convert_message(role, msg.get("content"), &mut messages, &mut pending_images);
    }
    out["messages"] = Value::Array(messages);

    if let Some(v) = body.get("max_tokens") {
        if crate::effort::uses_max_completion_tokens(model) {
            out["max_completion_tokens"] = v.clone();
        } else {
            out["max_tokens"] = v.clone();
        }
    }
    for (from, to) in [
        ("temperature", "temperature"),
        ("top_p", "top_p"),
        ("stop_sequences", "stop"),
        ("stream", "stream"),
    ] {
        if let Some(v) = body.get(from) {
            out[to] = v.clone();
        }
    }
    // DeepSeek/Kimi 等非 OpenAI 系严格网关会拒收未知字段，仅对已知支持的模型写入。
    if crate::effort::supports_reasoning_effort(model)
        && let Some(effort) = crate::effort::from_anthropic(body)
    {
        out["reasoning_effort"] = json!(effort);
    }
    let tools: Vec<Value> = body
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        // 服务端工具（web_search 等）Chat 上游无法执行，只转换客户端函数工具。
        .filter(|t| t.get("input_schema").is_some())
        .map(|t| {
            let mut function =
                json!({ "name": t.get("name").and_then(Value::as_str).unwrap_or("") });
            if let Some(description) = t.get("description").filter(|d| !d.is_null()) {
                function["description"] = description.clone();
            }
            function["parameters"] =
                clean_schema(t.get("input_schema").cloned().unwrap_or(json!({})), true);
            json!({ "type": "function", "function": function })
        })
        .collect();
    if !tools.is_empty() {
        out["tools"] = Value::Array(tools);
        if let Some(choice) = body.get("tool_choice") {
            out["tool_choice"] = tool_choice_to_chat(choice);
        }
    }
    if out.get("stream").and_then(Value::as_bool) == Some(true) {
        out["stream_options"] = json!({ "include_usage": true });
    }
    out
}

fn strip_billing_header(text: &str) -> &str {
    if !text.starts_with(BILLING_HEADER_PREFIX) {
        return text;
    }
    text.split_once('\n')
        .map_or("", |(_, rest)| rest.trim_start_matches(['\r', '\n']))
}

fn tool_choice_to_chat(choice: &Value) -> Value {
    let kind = choice
        .as_str()
        .or_else(|| choice.get("type").and_then(Value::as_str));
    match kind {
        Some("any") => json!("required"),
        Some("auto") => json!("auto"),
        Some("none") => json!("none"),
        Some("tool") => json!({
            "type": "function",
            "function": { "name": choice.get("name").and_then(Value::as_str).unwrap_or("") }
        }),
        _ => choice.clone(),
    }
}

/// OpenAI 要求根 schema 是 object；`format: uri` 部分厂商会拒收。
pub(crate) fn clean_schema(mut schema: Value, root: bool) -> Value {
    if let Some(obj) = schema.as_object_mut() {
        if root && !obj.contains_key("type") {
            obj.insert("type".into(), json!("object"));
            obj.entry("properties").or_insert_with(|| json!({}));
        }
        if obj.get("format").and_then(Value::as_str) == Some("uri") {
            obj.remove("format");
        }
        if let Some(props) = obj.get_mut("properties").and_then(Value::as_object_mut) {
            for value in props.values_mut() {
                *value = clean_schema(value.take(), false);
            }
        }
        if let Some(items) = obj.get_mut("items") {
            *items = clean_schema(items.take(), false);
        }
    }
    schema
}

/// 一条 Anthropic 消息可能拆成多条 Chat 消息（tool_result 各成一条 tool 消息；
/// 其中的图片挪到紧随其后的一条合成 user 消息，Chat tool 消息只能是纯文本）。
fn convert_message(
    role: &str,
    content: Option<&Value>,
    out: &mut Vec<Value>,
    pending_images: &mut crate::media::PendingToolImages,
) {
    let Some(blocks) = content.and_then(Value::as_array) else {
        out.push(json!({ "role": role, "content": content.cloned().unwrap_or(Value::Null) }));
        return;
    };
    let mut parts = Vec::new();
    let mut tool_calls = Vec::new();
    let mut reasoning = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str).unwrap_or("") {
            "text" => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    parts.push(json!({ "type": "text", "text": text }));
                }
            }
            "image" => {
                if let Some(url) = crate::media::anthropic_image_url(block) {
                    parts.push(json!({ "type": "image_url", "image_url": { "url": url } }));
                }
            }
            "tool_use" => tool_calls.push(json!({
                "id": block.get("id").and_then(Value::as_str).unwrap_or(""),
                "type": "function",
                "function": {
                    "name": block.get("name").and_then(Value::as_str).unwrap_or(""),
                    "arguments": block.get("input").cloned().unwrap_or_else(|| json!({})).to_string(),
                }
            })),
            "tool_result" => {
                let tool_call_id = block.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                let content = match block.get("content") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Array(items)) => {
                        let (text, images) =
                            crate::media::split_anthropic_tool_result_content(items);
                        pending_images.push(tool_call_id, images);
                        text
                    }
                    Some(other) => other.to_string(),
                    None => String::new(),
                };
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "content": content,
                }));
            }
            "thinking" => {
                if let Some(text) = block.get("thinking").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                    reasoning.push(text.to_owned());
                }
            }
            _ => {}
        }
    }
    // tool 消息必须紧跟在带 tool_calls 的 assistant 消息后；图片消息在此落地，
    // 早于本条消息里可能出现的普通 user 内容（下方 parts 组装成的 msg）。
    if let Some(image_msg) = pending_images.take_message() {
        out.push(image_msg);
    }
    if parts.is_empty() && tool_calls.is_empty() {
        return;
    }
    let mut msg = json!({ "role": role });
    msg["content"] = match parts.as_slice() {
        [] => Value::Null,
        [single] if single.get("type").and_then(Value::as_str) == Some("text") => {
            single.get("text").cloned().unwrap_or(Value::Null)
        }
        _ => Value::Array(parts),
    };
    if !tool_calls.is_empty() {
        msg["tool_calls"] = Value::Array(tool_calls);
        // 思考模型（DeepSeek/Kimi 等）要求工具调用消息回传思考；只回传真实思考。
        if role == "assistant" && !reasoning.is_empty() {
            msg["reasoning_content"] = json!(reasoning.join("\n"));
        }
    }
    out.push(msg);
}

// ---------------------------------------------------------------------------
// 响应：Chat → Anthropic（非流式）
// ---------------------------------------------------------------------------

fn stop_reason(finish: Option<&str>, has_tool_use: bool) -> Option<&'static str> {
    match finish {
        Some("length") => Some("max_tokens"),
        Some("tool_calls" | "function_call") => Some("tool_use"),
        Some(_) => Some(if has_tool_use { "tool_use" } else { "end_turn" }),
        None => has_tool_use.then_some("tool_use"),
    }
}

/// OpenAI 的 prompt_tokens 含缓存命中，Anthropic 的 input_tokens 不含：减去后三桶互斥。
pub(crate) fn usage_to_anthropic(usage: Option<&Value>) -> Value {
    let get = |pointers: &[&str]| {
        pointers
            .iter()
            .find_map(|p| usage.and_then(|u| u.pointer(p)).and_then(Value::as_u64))
            .unwrap_or(0)
    };
    let cached = get(&[
        "/cache_read_input_tokens",
        "/prompt_tokens_details/cached_tokens",
        "/prompt_cache_hit_tokens",
    ]);
    let created = get(&[
        "/cache_creation_input_tokens",
        "/prompt_tokens_details/cache_write_tokens",
    ]);
    let mut out = json!({
        "input_tokens": get(&["/prompt_tokens"]).saturating_sub(cached).saturating_sub(created),
        "output_tokens": get(&["/completion_tokens"]),
    });
    if cached > 0 {
        out["cache_read_input_tokens"] = json!(cached);
    }
    if created > 0 {
        out["cache_creation_input_tokens"] = json!(created);
    }
    out
}

fn reasoning_text(value: &Value) -> Option<&str> {
    value
        .get("reasoning_content")
        .or_else(|| value.get("reasoning"))
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
}

pub(crate) fn response_from_chat(body: &Value) -> Result<Value, String> {
    let choice = body
        .pointer("/choices/0")
        .ok_or("上游 Chat 响应缺少 choices")?;
    let message = choice.get("message").ok_or("上游 Chat 响应缺少 message")?;
    let mut content = Vec::new();
    if let Some(thinking) = reasoning_text(message) {
        content.push(json!({ "type": "thinking", "thinking": thinking, "signature": "" }));
    }
    match message.get("content") {
        Some(Value::String(text)) if !text.is_empty() => {
            content.push(json!({ "type": "text", "text": text }));
        }
        Some(Value::Array(parts)) => {
            for part in parts {
                // 严格模式下拒答走 refusal part（而非 text），仍需要展示给用户。
                if part.get("type").and_then(Value::as_str) == Some("refusal") {
                    if let Some(text) = part
                        .get("refusal")
                        .and_then(Value::as_str)
                        .filter(|t| !t.is_empty())
                    {
                        content.push(json!({ "type": "text", "text": text }));
                    }
                } else if let Some(text) = part.get("text").and_then(Value::as_str) {
                    content.push(json!({ "type": "text", "text": text }));
                }
            }
        }
        _ => {}
    }
    // 部分厂商把 refusal 放在 message 顶层而非 content parts 里。
    if let Some(text) = message
        .get("refusal")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
    {
        content.push(json!({ "type": "text", "text": text }));
    }
    let calls = message.get("tool_calls").and_then(Value::as_array);
    let mut has_tool_use = false;
    for call in calls.into_iter().flatten() {
        has_tool_use = true;
        let arguments = call
            .pointer("/function/arguments")
            .and_then(Value::as_str)
            .unwrap_or("{}");
        content.push(json!({
            "type": "tool_use",
            "id": call.get("id").and_then(Value::as_str).unwrap_or(""),
            "name": call.pointer("/function/name").and_then(Value::as_str).unwrap_or(""),
            "input": serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| json!({})),
        }));
    }
    // 旧式 function_call（无 tool_calls 时的回退，部分厂商仍按该格式返回）。
    if !has_tool_use
        && let Some(function_call) = message.get("function_call")
        && (function_call
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
            || function_call.get("arguments").is_some())
    {
        let input = match function_call.get("arguments") {
            Some(Value::String(s)) => serde_json::from_str(s).unwrap_or_else(|_| json!({})),
            Some(v @ (Value::Object(_) | Value::Array(_))) => v.clone(),
            _ => json!({}),
        };
        content.push(json!({
            "type": "tool_use",
            "id": function_call.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("toolu_mcpane_0"),
            "name": function_call.get("name").and_then(Value::as_str).unwrap_or(""),
            "input": input,
        }));
        has_tool_use = true;
    }
    Ok(json!({
        "id": body.get("id").and_then(Value::as_str).unwrap_or(""),
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": body.get("model").and_then(Value::as_str).unwrap_or(""),
        "stop_reason": stop_reason(choice.get("finish_reason").and_then(Value::as_str), has_tool_use),
        "stop_sequence": null,
        "usage": usage_to_anthropic(body.get("usage")),
    }))
}

// ---------------------------------------------------------------------------
// 响应：Chat SSE → Anthropic SSE（流式）
// ---------------------------------------------------------------------------

fn sse(event: &str, data: &Value) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {data}\n\n"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OpenBlock {
    Thinking,
    Text,
}

struct ToolBlock {
    index: u32,
    id: String,
    name: String,
    pending_args: String,
    started: bool,
}

/// Chat chunk 序列 → Anthropic 事件序列。Anthropic 要求一条消息只有一个
/// `message_delta`：finish_reason 先缓存，到 `[DONE]` / 流尾带上最终 usage 再发。
pub(crate) struct StreamConverter {
    started: bool,
    finished: bool,
    message_id: String,
    model: String,
    next_index: u32,
    open: Option<(OpenBlock, u32)>,
    tools: Vec<(usize, ToolBlock)>,
    stop_reason: Option<&'static str>,
    usage: Option<Value>,
}

impl StreamConverter {
    pub(crate) fn new() -> Self {
        Self {
            started: false,
            finished: false,
            message_id: "msg_mcpane".into(),
            model: String::new(),
            next_index: 0,
            open: None,
            tools: Vec::new(),
            stop_reason: None,
            usage: None,
        }
    }

    fn ensure_started(&mut self) -> Vec<Bytes> {
        if self.started {
            return Vec::new();
        }
        self.started = true;
        vec![sse(
            "message_start",
            &json!({
                "type": "message_start",
                "message": {
                    "id": self.message_id, "type": "message", "role": "assistant",
                    "model": self.model, "content": [], "stop_reason": null, "stop_sequence": null,
                    "usage": { "input_tokens": 0, "output_tokens": 0 }
                }
            }),
        )]
    }

    fn close_open(&mut self) -> Vec<Bytes> {
        match self.open.take() {
            Some((_, index)) => vec![sse(
                "content_block_stop",
                &json!({ "type": "content_block_stop", "index": index }),
            )],
            None => Vec::new(),
        }
    }

    /// 文本/思考增量：块类型切换时先关旧块再开新块。
    fn block_delta(&mut self, kind: OpenBlock, text: &str) -> Vec<Bytes> {
        let mut events = Vec::new();
        let index = match self.open {
            Some((open, index)) if open == kind => index,
            _ => {
                events.extend(self.close_open());
                let index = self.next_index;
                self.next_index += 1;
                self.open = Some((kind, index));
                let block = match kind {
                    OpenBlock::Thinking => {
                        json!({ "type": "thinking", "thinking": "", "signature": "" })
                    }
                    OpenBlock::Text => json!({ "type": "text", "text": "" }),
                };
                events.push(sse(
                    "content_block_start",
                    &json!({ "type": "content_block_start", "index": index, "content_block": block }),
                ));
                index
            }
        };
        let delta = match kind {
            OpenBlock::Thinking => json!({ "type": "thinking_delta", "thinking": text }),
            OpenBlock::Text => json!({ "type": "text_delta", "text": text }),
        };
        events.push(sse(
            "content_block_delta",
            &json!({ "type": "content_block_delta", "index": index, "delta": delta }),
        ));
        events
    }

    fn tool_delta(&mut self, call: &Value) -> Vec<Bytes> {
        let key = call
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|i| usize::try_from(i).ok())
            .unwrap_or(0);
        let mut events = Vec::new();
        if !self.tools.iter().any(|(k, _)| *k == key) {
            events.extend(self.close_open());
            let index = self.next_index;
            self.next_index += 1;
            self.tools.push((
                key,
                ToolBlock {
                    index,
                    id: String::new(),
                    name: String::new(),
                    pending_args: String::new(),
                    started: false,
                },
            ));
        }
        let Some((_, tool)) = self.tools.iter_mut().find(|(k, _)| *k == key) else {
            return events;
        };
        if let Some(id) = call
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            id.clone_into(&mut tool.id);
        }
        if let Some(name) = call
            .pointer("/function/name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            name.clone_into(&mut tool.name);
        }
        let args = call
            .pointer("/function/arguments")
            .and_then(Value::as_str)
            .unwrap_or("");
        tool.pending_args.push_str(args);
        // id 与 name 都到齐才能开块；此前的参数片段先攒着。
        if !tool.started && !tool.id.is_empty() && !tool.name.is_empty() {
            tool.started = true;
            events.push(sse(
                "content_block_start",
                &json!({ "type": "content_block_start", "index": tool.index,
                         "content_block": { "type": "tool_use", "id": tool.id, "name": tool.name, "input": {} } }),
            ));
        }
        if tool.started && !tool.pending_args.is_empty() {
            let partial = std::mem::take(&mut tool.pending_args);
            events.push(sse(
                "content_block_delta",
                &json!({ "type": "content_block_delta", "index": tool.index,
                         "delta": { "type": "input_json_delta", "partial_json": partial } }),
            ));
        }
        events
    }

    pub(crate) fn handle_chunk(&mut self, chunk: &Value) -> Vec<Bytes> {
        if let Some(id) = chunk
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            id.clone_into(&mut self.message_id);
        }
        if let Some(model) = chunk
            .get("model")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            model.clone_into(&mut self.model);
        }
        let mut events = self.ensure_started();
        if let Some(usage) = chunk.get("usage").filter(|u| u.is_object()) {
            self.usage = Some(usage_to_anthropic(Some(usage)));
        }
        let Some(choice) = chunk.pointer("/choices/0") else {
            return events;
        };
        if let Some(delta) = choice.get("delta") {
            if let Some(thinking) = reasoning_text(delta) {
                events.extend(self.block_delta(OpenBlock::Thinking, thinking));
            }
            if let Some(text) = delta
                .get("content")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
            {
                events.extend(self.block_delta(OpenBlock::Text, text));
            }
            // 严格模式拒答的流式增量走 delta.refusal，并入正文文本块。
            if let Some(text) = delta
                .get("refusal")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
            {
                events.extend(self.block_delta(OpenBlock::Text, text));
            }
            for call in delta
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                events.extend(self.tool_delta(call));
            }
            // 旧式 function_call：无 index 字段，按单个工具调用（index 0）处理。
            // 兼容网关常在每个增量里带 `"function_call": null`，只认对象。
            if let Some(function_call) = delta.get("function_call").filter(|f| f.is_object()) {
                let normalized = json!({
                    "index": 0,
                    "function": {
                        "name": function_call.get("name"),
                        "arguments": function_call.get("arguments"),
                    }
                });
                events.extend(self.tool_delta(&normalized));
            }
        }
        // 部分上游会发多个带 finish_reason 的 chunk，只认第一个。
        if self.stop_reason.is_none()
            && let Some(finish) = choice.get("finish_reason").and_then(Value::as_str)
        {
            self.stop_reason = stop_reason(Some(finish), !self.tools.is_empty());
        }
        events
    }

    pub(crate) fn finish(&mut self) -> Vec<Bytes> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let mut events = self.ensure_started();
        events.extend(self.close_open());
        // 名字、id、参数全空的是网关 null 字段带出的空壳，不是真实调用。
        self.tools.retain(|(_, tool)| {
            tool.started
                || !(tool.name.is_empty() && tool.id.is_empty() && tool.pending_args.is_empty())
        });
        for (_, tool) in &mut self.tools {
            if !tool.started {
                if tool.id.is_empty() {
                    tool.id = format!("toolu_mcpane_{}", tool.index);
                }
                // 上游偶发只给参数没给函数名：占位名对齐 cc-switch，不丢弃调用。
                if tool.name.is_empty() {
                    tracing::warn!("本地路由：Chat 工具调用缺少函数名，回退占位名 unknown_tool");
                    tool.name = "unknown_tool".to_owned();
                }
                events.push(sse(
                    "content_block_start",
                    &json!({ "type": "content_block_start", "index": tool.index,
                             "content_block": { "type": "tool_use", "id": tool.id, "name": tool.name, "input": {} } }),
                ));
                if !tool.pending_args.is_empty() {
                    events.push(sse(
                        "content_block_delta",
                        &json!({ "type": "content_block_delta", "index": tool.index,
                                 "delta": { "type": "input_json_delta", "partial_json": tool.pending_args } }),
                    ));
                }
            }
            events.push(sse(
                "content_block_stop",
                &json!({ "type": "content_block_stop", "index": tool.index }),
            ));
        }
        let stop = self
            .stop_reason
            .or_else(|| stop_reason(None, !self.tools.is_empty()))
            .unwrap_or("end_turn");
        events.push(sse(
            "message_delta",
            &json!({
                "type": "message_delta",
                "delta": { "stop_reason": stop, "stop_sequence": null },
                "usage": self.usage.clone().unwrap_or_else(|| json!({ "input_tokens": 0, "output_tokens": 0 })),
            }),
        ));
        events.push(sse("message_stop", &json!({ "type": "message_stop" })));
        events
    }

    /// 上游报错：只发 error 事件，不把失败伪装成正常结束。
    pub(crate) fn fail(&mut self, message: &str) -> Bytes {
        self.finished = true;
        sse(
            "error",
            &json!({ "type": "error", "error": { "type": "api_error", "message": message } }),
        )
    }
}

struct StreamState<S> {
    upstream: S,
    conv: StreamConverter,
    buffer: String,
    remainder: Vec<u8>,
    ended: bool,
}

pub(crate) fn convert_stream<S, E>(
    upstream: S,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send
where
    S: Stream<Item = Result<Bytes, E>> + Send + Unpin + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let state = StreamState {
        upstream,
        conv: StreamConverter::new(),
        buffer: String::new(),
        remainder: Vec::new(),
        ended: false,
    };
    futures_util::stream::unfold(state, |mut st| async move {
        if st.ended {
            return None;
        }
        let events = match st.upstream.next().await {
            Some(Ok(bytes)) => {
                crate::sse::append_utf8(&mut st.buffer, &mut st.remainder, &bytes);
                let mut events = Vec::new();
                while let Some(data) = crate::sse::take_data(&mut st.buffer) {
                    if data.trim() == "[DONE]" {
                        events.extend(st.conv.finish());
                        st.ended = true;
                        break;
                    }
                    let Ok(chunk) = serde_json::from_str::<Value>(&data) else {
                        continue;
                    };
                    if chunk.get("error").is_some() {
                        let message = crate::codex_chat::error_message_from_chat(data.as_bytes());
                        events.push(st.conv.fail(&message));
                        st.ended = true;
                        break;
                    }
                    events.extend(st.conv.handle_chunk(&chunk));
                }
                events
            }
            Some(Err(error)) => {
                st.ended = true;
                vec![st.conv.fail(&format!("上游流中断：{error}"))]
            }
            None => {
                st.ended = true;
                // 未收到 [DONE] 但流正常结束：补发收尾事件。
                st.conv.finish()
            }
        };
        Some((futures_util::stream::iter(events.into_iter().map(Ok)), st))
    })
    .flatten()
}

/// count_tokens 的本地估算（Chat 上游没有该端点）：约 4 字节一个 token。
pub(crate) fn estimate_tokens(body: &[u8]) -> Value {
    json!({ "input_tokens": body.len().div_ceil(4) })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn request_converts_system_tools_and_tool_round_trip() {
        let body = json!({
            "model": "glm", "max_tokens": 100, "stream": true,
            "system": [{ "type": "text", "text": "x-anthropic-billing-header: cch=1;\nBe brief." }],
            "tools": [
                { "name": "Bash", "description": "run", "input_schema": { "properties": { "u": { "type": "string", "format": "uri" } } } },
                { "type": "web_search_20250305", "name": "web_search" }
            ],
            "tool_choice": { "type": "any" },
            "messages": [
                { "role": "user", "content": "hi" },
                { "role": "assistant", "content": [
                    { "type": "thinking", "thinking": "plan" },
                    { "type": "text", "text": "ok" },
                    { "type": "tool_use", "id": "t1", "name": "Bash", "input": { "cmd": "ls" } }
                ] },
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "t1", "content": [{ "type": "text", "text": "a.txt" }] },
                    { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAA" } }
                ] }
            ]
        });
        let chat = request_to_chat(&body);
        let m = chat["messages"].as_array().unwrap();
        assert_eq!(m[0], json!({ "role": "system", "content": "Be brief." }));
        assert_eq!(m[1], json!({ "role": "user", "content": "hi" }));
        assert_eq!(m[2]["content"], "ok");
        assert_eq!(m[2]["reasoning_content"], "plan");
        assert_eq!(
            m[2]["tool_calls"][0]["function"]["arguments"],
            "{\"cmd\":\"ls\"}"
        );
        assert_eq!(
            m[3],
            json!({ "role": "tool", "tool_call_id": "t1", "content": "a.txt" })
        );
        assert_eq!(
            m[4]["content"][0]["image_url"]["url"],
            "data:image/png;base64,AAA"
        );
        let tools = chat["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1, "服务端工具不转换");
        assert_eq!(tools[0]["function"]["parameters"]["type"], "object");
        assert!(
            tools[0]["function"]["parameters"]["properties"]["u"]
                .get("format")
                .is_none()
        );
        assert_eq!(chat["tool_choice"], "required");
        assert_eq!(chat["stream_options"]["include_usage"], true);
    }

    #[test]
    fn tool_result_images_move_to_synthetic_user_message_after_tool_messages() {
        let body = json!({
            "messages": [
                { "role": "assistant", "content": [
                    { "type": "tool_use", "id": "t1", "name": "Read", "input": {} },
                    { "type": "tool_use", "id": "t2", "name": "Read", "input": {} }
                ] },
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "t1", "content": [
                        { "type": "text", "text": "a.txt" },
                        { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAA" } }
                    ] },
                    { "type": "tool_result", "tool_use_id": "t2", "content": [
                        { "type": "text", "text": "no image here" }
                    ] },
                    { "type": "text", "text": "what do you see?" }
                ] }
            ]
        });
        let chat = request_to_chat(&body);
        let m = chat["messages"].as_array().unwrap();
        // assistant(tool_calls), tool(t1), tool(t2), 合成 user(图片), user(文本)。
        assert_eq!(m.len(), 5);
        assert_eq!(m[0]["role"], "assistant");
        assert_eq!(
            m[1],
            json!({ "role": "tool", "tool_call_id": "t1", "content": "a.txt\n[图片见下一条消息]" })
        );
        assert_eq!(
            m[2],
            json!({ "role": "tool", "tool_call_id": "t2", "content": "no image here" })
        );
        assert_eq!(m[3]["role"], "user");
        let synth_content = m[3]["content"].as_array().unwrap();
        assert!(synth_content[0]["text"].as_str().unwrap().contains("t1"));
        assert_eq!(
            synth_content[1]["image_url"]["url"],
            "data:image/png;base64,AAA"
        );
        assert_eq!(
            m[4],
            json!({ "role": "user", "content": "what do you see?" })
        );
    }

    #[test]
    fn non_stream_response_maps_blocks_stop_reason_and_usage() {
        let chat = json!({
            "id": "c1", "model": "glm",
            "choices": [{ "finish_reason": "tool_calls", "message": {
                "reasoning_content": "think", "content": "doing",
                "tool_calls": [{ "id": "t1", "function": { "name": "Bash", "arguments": "{\"cmd\":\"ls\"}" } }]
            } }],
            "usage": { "prompt_tokens": 100, "completion_tokens": 9, "prompt_tokens_details": { "cached_tokens": 60 } }
        });
        let resp = response_from_chat(&chat).unwrap();
        assert_eq!(resp["content"][0]["type"], "thinking");
        assert_eq!(resp["content"][1]["text"], "doing");
        assert_eq!(resp["content"][2]["input"]["cmd"], "ls");
        assert_eq!(resp["stop_reason"], "tool_use");
        assert_eq!(resp["usage"]["input_tokens"], 40);
        assert_eq!(resp["usage"]["cache_read_input_tokens"], 60);
    }

    fn events(out: &[Bytes]) -> Vec<(String, Value)> {
        let text: String = out
            .iter()
            .map(|b| String::from_utf8(b.to_vec()).unwrap())
            .collect();
        text.split("\n\n")
            .filter(|b| !b.is_empty())
            .map(|block| {
                let mut lines = block.lines();
                let event = lines
                    .next()
                    .unwrap()
                    .trim_start_matches("event: ")
                    .to_owned();
                let data = lines.next().unwrap().trim_start_matches("data: ");
                (event, serde_json::from_str(data).unwrap())
            })
            .collect()
    }

    #[tokio::test]
    async fn stream_emits_single_message_delta_with_final_usage() {
        let raw = [
            r#"{"id":"c","model":"glm","choices":[{"delta":{"reasoning_content":"hm","content":""}}]}"#,
            r#"{"choices":[{"delta":{"content":"Hi","reasoning_content":""}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"a\""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"t1","function":{"name":"Bash","arguments":":1}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":3}}"#,
        ]
        .iter()
        .map(|c| format!("data: {c}\n\n"))
        .collect::<String>()
            + "data: [DONE]\n\n";
        let upstream = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(raw))]);
        let out: Vec<Bytes> = convert_stream(upstream).map(|r| r.unwrap()).collect().await;
        let ev = events(&out);
        let names: Vec<&str> = ev.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(names.first(), Some(&"message_start"));
        assert_eq!(names.last(), Some(&"message_stop"));
        assert_eq!(names.iter().filter(|n| **n == "message_delta").count(), 1);
        // thinking 块 → text 块 → tool_use 块，各自开关成对，空 reasoning 不打断正文。
        assert_eq!(
            names
                .iter()
                .filter(|n| **n == "content_block_start")
                .count(),
            3
        );
        assert_eq!(
            names.iter().filter(|n| **n == "content_block_stop").count(),
            3
        );
        let json_parts: String = ev
            .iter()
            .filter_map(|(_, d)| d.pointer("/delta/partial_json").and_then(Value::as_str))
            .collect();
        assert_eq!(json_parts, "{\"a\":1}");
        let delta = &ev.iter().find(|(e, _)| e == "message_delta").unwrap().1;
        assert_eq!(delta["delta"]["stop_reason"], "tool_use");
        assert_eq!(delta["usage"]["output_tokens"], 3);
    }

    #[test]
    fn reasoning_effort_written_only_for_supported_models() {
        let body = |model: &str| {
            json!({ "model": model, "max_tokens": 10, "thinking": { "type": "enabled", "budget_tokens": 30000 },
                    "messages": [] })
        };
        let gpt5 = request_to_chat(&body("gpt-5.2"));
        assert_eq!(gpt5["reasoning_effort"], "high");
        // DeepSeek/Kimi 等非 OpenAI 系模型：严格网关会拒收未知字段，不写。
        let deepseek = request_to_chat(&body("deepseek-chat"));
        assert!(deepseek.get("reasoning_effort").is_none());
    }

    #[test]
    fn max_completion_tokens_for_openai_reasoning_models() {
        let body = |model: &str| json!({ "model": model, "max_tokens": 4096, "messages": [] });
        for model in ["o1", "o3-mini", "o4-mini", "gpt-5.2"] {
            let chat = request_to_chat(&body(model));
            assert!(
                chat.get("max_tokens").is_none(),
                "{model} 不应有 max_tokens"
            );
            assert_eq!(
                chat["max_completion_tokens"], 4096,
                "{model} 应转 max_completion_tokens"
            );
        }
        // OpenAI 对 GPT-5 同样拒收 max_tokens（比 cc-switch 只认 o 系列更全）；grok 与非推理模型照旧。
        for model in ["grok-4.5", "gpt-4o", "deepseek-chat"] {
            let chat = request_to_chat(&body(model));
            assert_eq!(chat["max_tokens"], 4096, "{model} 应保留 max_tokens");
            assert!(chat.get("max_completion_tokens").is_none());
        }
    }

    #[test]
    fn non_stream_response_converts_refusal_and_legacy_function_call() {
        let chat = json!({
            "id": "c1", "model": "glm",
            "choices": [{ "message": {
                "refusal": "抱歉，我不能帮你做这个。",
                "function_call": { "name": "Bash", "arguments": "{\"cmd\":\"ls\"}" }
            } }]
        });
        let resp = response_from_chat(&chat).unwrap();
        assert_eq!(resp["content"][0]["type"], "text");
        assert_eq!(resp["content"][0]["text"], "抱歉，我不能帮你做这个。");
        assert_eq!(resp["content"][1]["type"], "tool_use");
        assert_eq!(resp["content"][1]["name"], "Bash");
        assert_eq!(resp["content"][1]["input"]["cmd"], "ls");
        assert_eq!(resp["stop_reason"], "tool_use");
    }

    #[test]
    fn non_stream_response_converts_refusal_content_part() {
        let chat = json!({
            "id": "c1", "model": "glm",
            "choices": [{ "message": {
                "content": [{ "type": "refusal", "refusal": "no can do" }]
            } }]
        });
        let resp = response_from_chat(&chat).unwrap();
        assert_eq!(
            resp["content"][0],
            json!({ "type": "text", "text": "no can do" })
        );
    }

    #[tokio::test]
    async fn stream_converts_refusal_delta_and_legacy_function_call() {
        let raw = [
            r#"{"choices":[{"delta":{"refusal":"can't "}}]}"#,
            r#"{"choices":[{"delta":{"refusal":"help"}}]}"#,
            r#"{"choices":[{"delta":{"function_call":{"name":"Bash","arguments":"{\"cmd\""}}}]}"#,
            r#"{"choices":[{"delta":{"function_call":{"arguments":":\"ls\"}"}}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"function_call"}]}"#,
        ]
        .iter()
        .map(|c| format!("data: {c}\n\n"))
        .collect::<String>()
            + "data: [DONE]\n\n";
        let upstream = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(raw))]);
        let out: Vec<Bytes> = convert_stream(upstream).map(|r| r.unwrap()).collect().await;
        let ev = events(&out);
        let text: String = ev
            .iter()
            .filter_map(|(_, d)| d.pointer("/delta/text").and_then(Value::as_str))
            .collect();
        assert_eq!(text, "can't help");
        let tool_start = ev
            .iter()
            .find(|(e, d)| e == "content_block_start" && d["content_block"]["type"] == "tool_use")
            .unwrap()
            .1
            .clone();
        assert_eq!(tool_start["content_block"]["name"], "Bash");
        let json_parts: String = ev
            .iter()
            .filter_map(|(_, d)| d.pointer("/delta/partial_json").and_then(Value::as_str))
            .collect();
        assert_eq!(json_parts, "{\"cmd\":\"ls\"}");
        let delta = &ev.iter().find(|(e, _)| e == "message_delta").unwrap().1;
        assert_eq!(delta["delta"]["stop_reason"], "tool_use");
    }

    /// 兼容网关在每个增量里带 null 字段（`function_call` / `tool_calls` / `refusal`）：
    /// 不能凭空生成工具调用，纯文本回答必须以 end_turn 结束。
    #[tokio::test]
    async fn null_fields_in_deltas_do_not_create_phantom_tool_calls() {
        let raw = [
            r#"{"choices":[{"delta":{"role":"assistant","content":"po","function_call":null,"tool_calls":null,"refusal":null,"reasoning_content":null}}]}"#,
            r#"{"choices":[{"delta":{"content":"ng","function_call":null,"tool_calls":null}}]}"#,
            r#"{"choices":[{"delta":{"function_call":null},"finish_reason":"stop"}]}"#,
        ]
        .iter()
        .map(|c| format!("data: {c}\n\n"))
        .collect::<String>()
            + "data: [DONE]\n\n";
        let upstream = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(raw))]);
        let out: Vec<Bytes> = convert_stream(upstream).map(|r| r.unwrap()).collect().await;
        let ev = events(&out);
        assert!(
            !ev.iter()
                .any(|(_, d)| d["content_block"]["type"] == "tool_use"),
            "{ev:?}"
        );
        let delta = &ev.iter().find(|(e, _)| e == "message_delta").unwrap().1;
        assert_eq!(delta["delta"]["stop_reason"], "end_turn");
    }

    #[tokio::test]
    async fn stream_finish_uses_placeholder_name_instead_of_dropping_unnamed_tool() {
        // 只收到参数片段，没有函数名就结束（部分上游异常输出）：不能整段丢弃。
        let raw = [
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"t1","function":{"arguments":"{}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        ]
        .iter()
        .map(|c| format!("data: {c}\n\n"))
        .collect::<String>()
            + "data: [DONE]\n\n";
        let upstream = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(raw))]);
        let out: Vec<Bytes> = convert_stream(upstream).map(|r| r.unwrap()).collect().await;
        let ev = events(&out);
        let tool_start = ev
            .iter()
            .find(|(e, d)| e == "content_block_start" && d["content_block"]["type"] == "tool_use")
            .unwrap() // 工具调用不应被丢弃
            .1
            .clone();
        assert_eq!(tool_start["content_block"]["name"], "unknown_tool");
        assert_eq!(tool_start["content_block"]["id"], "t1");
    }

    #[tokio::test]
    async fn stream_error_chunk_becomes_error_event_only() {
        let raw = "data: {\"error\":{\"message\":\"quota exceeded\"}}\n\n";
        let upstream = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(raw))]);
        let out: Vec<Bytes> = convert_stream(upstream).map(|r| r.unwrap()).collect().await;
        let ev = events(&out);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].0, "error");
        assert_eq!(ev[0].1["error"]["message"], "quota exceeded");
    }
}
