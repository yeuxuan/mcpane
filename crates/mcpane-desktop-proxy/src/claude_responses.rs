//! Claude（Anthropic Messages）↔ OpenAI Responses 协议转换（ADR-022 二期）。
//!
//! 让只提供 Responses 端点的厂商（如 `-codex` 系模型）可用于 Claude Code /
//! Claude Desktop：请求转成 Responses、响应（含 SSE）转回 Anthropic 事件。
//! 不搬 web_search 厂商分支、文档附件与引用标注。
//!
//! reasoning 往返：Responses 的 `reasoning` 输出项（含 `encrypted_content`）在
//! Anthropic 侧没有对应字段，借 `thinking.signature` 透传——序列化成带版本前缀
//! 的不透明 JSON 文本（未引入 base64 依赖，直接嵌 JSON 字符串），下一轮请求
//! 原样解码还原，模型侧看到的是无损的原始 reasoning item。

use axum::body::Bytes;
use futures_util::{Stream, StreamExt};
use serde_json::{Value, json};

/// Claude Code 在 system 开头注入的计费头（每次请求变化，会击穿前缀缓存）。
const BILLING_HEADER_PREFIX: &str = "x-anthropic-billing-header:";
/// reasoning item 不透明载荷的版本前缀，未来格式变化时可安全区分旧数据。
const REASONING_SIGNATURE_PREFIX: &str = "mcpane-responses-v1:";
/// OpenAI Responses 拒绝小于 16 的 max_output_tokens；Claude Code 的模型探测
/// 请求常发 max_tokens=1，直接透传会让探测请求整体失败，抬到下限而非报错。
const RESPONSES_MIN_MAX_OUTPUT_TOKENS: u64 = 16;

fn clamp_max_output_tokens(value: &Value) -> Value {
    match value.as_u64() {
        Some(n) if (1..RESPONSES_MIN_MAX_OUTPUT_TOKENS).contains(&n) => {
            json!(RESPONSES_MIN_MAX_OUTPUT_TOKENS)
        }
        _ => value.clone(),
    }
}

// ---------------------------------------------------------------------------
// 请求：Anthropic → Responses
// ---------------------------------------------------------------------------

pub(crate) fn request_to_responses(body: &Value) -> Value {
    let mut out = json!({});
    if let Some(model) = body.get("model") {
        out["model"] = model.clone();
    }
    let instructions = system_to_instructions(body);
    if !instructions.is_empty() {
        out["instructions"] = json!(instructions);
    }
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    out["input"] = json!(convert_messages_to_input(&messages));

    if let Some(v) = body.get("max_tokens") {
        out["max_output_tokens"] = clamp_max_output_tokens(v);
    }
    for key in ["temperature", "top_p", "stream"] {
        if let Some(v) = body.get(key) {
            out[key] = v.clone();
        }
    }
    // stop_sequences：Responses 无对应字段，丢弃。

    let tools: Vec<Value> = body
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        // 服务端工具（web_search 等）Responses 上游语义不同，只转换客户端函数工具。
        .filter(|t| t.get("input_schema").is_some())
        .map(|t| {
            let mut tool = json!({
                "type": "function",
                "name": t.get("name").and_then(Value::as_str).unwrap_or(""),
                "strict": false,
            });
            if let Some(description) = t.get("description").filter(|d| !d.is_null()) {
                tool["description"] = description.clone();
            }
            tool["parameters"] =
                function_parameters(t.get("input_schema").cloned().unwrap_or_else(|| json!({})));
            tool
        })
        .collect();
    if !tools.is_empty() {
        out["tools"] = Value::Array(tools);
        if let Some(choice) = body.get("tool_choice") {
            out["tool_choice"] = tool_choice_to_responses(choice);
            if choice
                .get("disable_parallel_tool_use")
                .and_then(Value::as_bool)
                == Some(true)
            {
                out["parallel_tool_calls"] = json!(false);
            }
        }
    }

    let model = body.get("model").and_then(Value::as_str).unwrap_or("");
    let effort = crate::effort::from_anthropic(body)
        .filter(|_| crate::effort::supports_reasoning_effort(model));
    if let Some(effort) = effort {
        out["reasoning"] = json!({ "effort": effort, "summary": "auto" });
        // 非推理模型收到该 include 会被上游 400（"Encrypted content is not
        // supported with this model"），只在确实启用 reasoning 时才带上。
        out["include"] = json!(["reasoning.encrypted_content"]);
    }

    // 多轮 reasoning 靠客户端原样回传 encrypted_content 续接，上游不必落状态。
    out["store"] = json!(false);
    out
}

fn system_to_instructions(body: &Value) -> String {
    match body.get("system") {
        Some(Value::String(text)) => strip_billing_header(text).to_owned(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .map(strip_billing_header)
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn strip_billing_header(text: &str) -> &str {
    if !text.starts_with(BILLING_HEADER_PREFIX) {
        return text;
    }
    text.split_once('\n')
        .map_or("", |(_, rest)| rest.trim_start_matches(['\r', '\n']))
}

fn tool_choice_to_responses(choice: &Value) -> Value {
    let kind = choice
        .as_str()
        .or_else(|| choice.get("type").and_then(Value::as_str));
    match kind {
        Some("any") => json!("required"),
        Some("auto") => json!("auto"),
        Some("none") => json!("none"),
        Some("tool") => json!({
            "type": "function",
            "name": choice.get("name").and_then(Value::as_str).unwrap_or("")
        }),
        _ => choice.clone(),
    }
}

/// Responses 对根 schema 要求比 Chat 更严格：必须是 object 且要有 properties；
/// clean_schema 只在根缺 type 时顺带补 properties，这里补齐「有 type 但缺
/// properties」的情况，不改 clean_schema 本身以免影响 Chat 路径。
fn function_parameters(schema: Value) -> Value {
    let mut schema = crate::claude_chat::clean_schema(schema, true);
    let Some(obj) = schema.as_object_mut() else {
        return json!({ "type": "object", "properties": {} });
    };
    if obj.get("type").and_then(Value::as_str) != Some("object") {
        obj.insert("type".into(), json!("object"));
    }
    obj.entry("properties").or_insert_with(|| json!({}));
    schema
}

fn image_url(block: &Value) -> Option<String> {
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

fn text_part(role: &str, text: &str) -> Value {
    let kind = if role == "assistant" {
        "output_text"
    } else {
        "input_text"
    };
    json!({ "type": kind, "text": text })
}

/// tool_result 内容 → (文本拼接, 图片 input_image 分段)；图片不能塞进
/// function_call_output（Responses 该字段只接受字符串/结构化文本），
/// 随后补一条 user 消息承载，避免图片被静默丢弃。
fn tool_result_parts(block: &Value) -> (String, Vec<Value>) {
    match block.get("content") {
        Some(Value::String(s)) => (s.clone(), Vec::new()),
        Some(Value::Array(items)) => {
            let mut texts = Vec::new();
            let mut images = Vec::new();
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = item.get("text").and_then(Value::as_str) {
                            texts.push(t.to_owned());
                        }
                    }
                    Some("image") => {
                        if let Some(url) = image_url(item) {
                            images.push(json!({ "type": "input_image", "image_url": url }));
                        }
                    }
                    _ => {}
                }
            }
            (texts.join("\n"), images)
        }
        Some(other) => (other.to_string(), Vec::new()),
        None => (String::new(), Vec::new()),
    }
}

fn flush_parts(input: &mut Vec<Value>, role: &str, parts: &mut Vec<Value>) {
    if !parts.is_empty() {
        input.push(json!({ "role": role, "content": std::mem::take(parts) }));
    }
}

/// Anthropic messages → Responses input：tool_use/tool_result 从 content 里
/// “提升”为独立顶层项，思考块按 signature 是否为本模块签发的不透明载荷决定
/// 还原原始 reasoning item 还是丢弃。
fn convert_messages_to_input(messages: &[Value]) -> Vec<Value> {
    let mut input = Vec::new();
    for msg in messages {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
        let message_start = input.len();
        match msg.get("content") {
            Some(Value::String(text)) => {
                input.push(json!({ "role": role, "content": [text_part(role, text)] }));
            }
            Some(Value::Array(blocks)) => {
                let mut parts = Vec::new();
                for block in blocks {
                    match block.get("type").and_then(Value::as_str).unwrap_or("") {
                        "text" => {
                            if let Some(text) = block.get("text").and_then(Value::as_str) {
                                parts.push(text_part(role, text));
                            }
                        }
                        "image" => {
                            if let Some(url) = image_url(block) {
                                parts.push(json!({ "type": "input_image", "image_url": url }));
                            }
                        }
                        "tool_use" => {
                            flush_parts(&mut input, role, &mut parts);
                            input.push(json!({
                                "type": "function_call",
                                "call_id": block.get("id").and_then(Value::as_str).unwrap_or(""),
                                "name": block.get("name").and_then(Value::as_str).unwrap_or(""),
                                "arguments": block.get("input").cloned().unwrap_or_else(|| json!({})).to_string(),
                            }));
                        }
                        "tool_result" => {
                            flush_parts(&mut input, role, &mut parts);
                            let (text, images) = tool_result_parts(block);
                            input.push(json!({
                                "type": "function_call_output",
                                "call_id": block.get("tool_use_id").and_then(Value::as_str).unwrap_or(""),
                                "output": text,
                            }));
                            if !images.is_empty() {
                                input.push(json!({ "role": "user", "content": images }));
                            }
                        }
                        "thinking" => {
                            let restored = block
                                .get("signature")
                                .and_then(Value::as_str)
                                .and_then(decode_reasoning_signature);
                            if let Some(item) = restored {
                                flush_parts(&mut input, role, &mut parts);
                                input.push(item);
                            }
                            // 非本模块签发的思考块无法还原成 Responses reasoning item，丢弃。
                        }
                        // redacted_thinking 无摘要文本可展示，且签名格式与 thinking 不同，直接丢弃。
                        "redacted_thinking" => {}
                        _ => {}
                    }
                }
                flush_parts(&mut input, role, &mut parts);
            }
            _ => {}
        }
        if role == "assistant" {
            prune_trailing_reasoning_without_follower(&mut input, message_start);
        }
    }
    input
}

/// 回放的 reasoning item 只有同一轮还跟着 function_call/助手消息才合法，否则
/// 下一轮请求会被拒「reasoning item ... without its required following item」
/// ——典型场景是思考过程被 max_tokens 截断，assistant 消息里只剩 thinking 块。
fn prune_trailing_reasoning_without_follower(input: &mut Vec<Value>, start: usize) {
    let mut has_follower = false;
    let mut index = input.len();
    while index > start {
        index -= 1;
        let item_type = input[index].get("type").and_then(Value::as_str);
        let is_assistant_message =
            input[index].get("role").and_then(Value::as_str) == Some("assistant");
        if item_type == Some("reasoning") {
            if !has_follower {
                input.remove(index);
            }
        } else if item_type == Some("function_call") || is_assistant_message {
            has_follower = true;
        }
    }
}

fn encode_reasoning_signature(item: &Value) -> String {
    format!("{REASONING_SIGNATURE_PREFIX}{item}")
}

fn decode_reasoning_signature(signature: &str) -> Option<Value> {
    let payload = signature.strip_prefix(REASONING_SIGNATURE_PREFIX)?;
    let item: Value = serde_json::from_str(payload).ok()?;
    (item.get("type").and_then(Value::as_str) == Some("reasoning")).then_some(item)
}

// ---------------------------------------------------------------------------
// 响应：Responses → Anthropic（非流式）
// ---------------------------------------------------------------------------

fn thinking_block_from_reasoning_item(item: &Value) -> Value {
    let text = item
        .get("summary")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");
    json!({ "type": "thinking", "thinking": text, "signature": encode_reasoning_signature(item) })
}

/// Responses 的 input_tokens 含缓存命中，Anthropic 的 input_tokens 不含。
fn usage_to_anthropic(usage: Option<&Value>) -> Value {
    let usage = usage.filter(|u| u.is_object());
    let get = |key: &str| {
        usage
            .and_then(|u| u.get(key))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let cached = usage
        .and_then(|u| u.pointer("/input_tokens_details/cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut out = json!({
        "input_tokens": get("input_tokens").saturating_sub(cached),
        "output_tokens": get("output_tokens"),
    });
    if cached > 0 {
        out["cache_read_input_tokens"] = json!(cached);
    }
    out
}

/// 终态非成功时拒绝：`failed`/`cancelled` 状态，或 error 字段非 null（可能与
/// 其它状态一起出现）。不能把这类响应包装成成功，否则客户端会把半截结果当正文。
fn terminal_failure_message(body: &Value) -> Option<String> {
    let status = body.get("status").and_then(Value::as_str);
    let has_error = body.get("error").is_some_and(|e| !e.is_null());
    if !matches!(status, Some("failed" | "cancelled")) && !has_error {
        return None;
    }
    let message = body
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("响应生成失败");
    Some(format!("上游 Responses 返回失败状态：{message}"))
}

/// `incomplete` 优先于 has_tool_use 判断：工具参数很可能被截断，报 tool_use
/// 会让客户端拿着残缺 JSON 去执行工具调用。
fn resolve_stop_reason(
    incomplete: bool,
    incomplete_reason: Option<&str>,
    has_tool_use: bool,
) -> &'static str {
    if incomplete {
        return match incomplete_reason {
            Some("content_filter") => "refusal",
            _ => "max_tokens",
        };
    }
    if has_tool_use { "tool_use" } else { "end_turn" }
}

pub(crate) fn response_from_responses(body: &Value) -> Result<Value, String> {
    if let Some(message) = terminal_failure_message(body) {
        return Err(message);
    }
    let output = body
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| "上游 Responses 响应缺少 output".to_owned())?;

    let mut content = Vec::new();
    let mut has_tool_use = false;
    for item in output {
        match item.get("type").and_then(Value::as_str).unwrap_or("") {
            "reasoning" => content.push(thinking_block_from_reasoning_item(item)),
            "message" => {
                for part in item
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    match part.get("type").and_then(Value::as_str) {
                        Some("output_text") => {
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                content.push(json!({ "type": "text", "text": text }));
                            }
                        }
                        // 内容被安全过滤拒绝时上游走 refusal part 而非 output_text。
                        Some("refusal") => {
                            if let Some(text) = part.get("refusal").and_then(Value::as_str) {
                                content.push(json!({ "type": "text", "text": text }));
                            }
                        }
                        _ => {}
                    }
                }
            }
            "function_call" => {
                has_tool_use = true;
                let arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                content.push(json!({
                    "type": "tool_use",
                    "id": item.get("call_id").and_then(Value::as_str).unwrap_or(""),
                    "name": item.get("name").and_then(Value::as_str).unwrap_or(""),
                    "input": serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| json!({})),
                }));
            }
            _ => {}
        }
    }

    let stop_reason = resolve_stop_reason(
        body.get("status").and_then(Value::as_str) == Some("incomplete"),
        body.pointer("/incomplete_details/reason")
            .and_then(Value::as_str),
        has_tool_use,
    );

    Ok(json!({
        "id": body.get("id").and_then(Value::as_str).unwrap_or(""),
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": body.get("model").and_then(Value::as_str).unwrap_or(""),
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": usage_to_anthropic(body.get("usage")),
    }))
}

// ---------------------------------------------------------------------------
// 响应：Responses SSE → Anthropic SSE（流式）
// ---------------------------------------------------------------------------

fn sse(event: &str, data: &Value) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {data}\n\n"))
}

/// Responses 生命周期事件把整个响应对象套在 `response` 字段下；
/// item 级增量事件（delta/added/done）字段直接摊平在顶层。两种都要取，
/// 缺 `response` 时退回事件本身（例如裸 `error` 事件）。
fn response_object(data: &Value) -> &Value {
    data.get("response").unwrap_or(data)
}

fn error_message(data: &Value) -> String {
    let response = response_object(data);
    let error = response.get("error").unwrap_or(response);
    error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .filter(|m| !m.trim().is_empty())
        .unwrap_or("上游 Responses 请求失败")
        .to_owned()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OpenBlock {
    Thinking,
    Text,
}

struct ToolBlock {
    index: u32,
    item_id: String,
    call_id: String,
    done: bool,
}

/// Responses 事件序列 → Anthropic 事件序列。与 Chat 增量拼接 id/name 不同，
/// Responses 的 `output_item.added` 就带齐 call_id/name，工具块可以立即开启。
pub(crate) struct StreamConverter {
    started: bool,
    finished: bool,
    has_output: bool,
    message_id: String,
    model: String,
    next_index: u32,
    open: Option<(OpenBlock, u32)>,
    tools: Vec<ToolBlock>,
    usage: Option<Value>,
}

impl StreamConverter {
    pub(crate) fn new() -> Self {
        Self {
            started: false,
            finished: false,
            has_output: false,
            message_id: "msg_mcpane".into(),
            model: String::new(),
            next_index: 0,
            open: None,
            tools: Vec::new(),
            usage: None,
        }
    }

    fn alloc_index(&mut self) -> u32 {
        let index = self.next_index;
        self.next_index += 1;
        index
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

    /// 关闭当前打开的 thinking/text 块（不含 signature，用于非 reasoning-done 触发的收尾）。
    fn close_open(&mut self) -> Vec<Bytes> {
        match self.open.take() {
            Some((_, index)) => vec![sse(
                "content_block_stop",
                &json!({ "type": "content_block_stop", "index": index }),
            )],
            None => Vec::new(),
        }
    }

    fn block_delta(&mut self, kind: OpenBlock, text: &str) -> Vec<Bytes> {
        let mut events = Vec::new();
        let index = match self.open {
            Some((open, index)) if open == kind => index,
            _ => {
                events.extend(self.close_open());
                let index = self.alloc_index();
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

    /// reasoning item 完结：先补 signature_delta 再关块，让签名在 stop 前落到内容里。
    fn close_reasoning(&mut self, item: &Value) -> Vec<Bytes> {
        let Some((OpenBlock::Thinking, index)) = self.open else {
            return Vec::new();
        };
        self.open = None;
        vec![
            sse(
                "content_block_delta",
                &json!({ "type": "content_block_delta", "index": index,
                         "delta": { "type": "signature_delta", "signature": encode_reasoning_signature(item) } }),
            ),
            sse(
                "content_block_stop",
                &json!({ "type": "content_block_stop", "index": index }),
            ),
        ]
    }

    fn start_tool(&mut self, item: &Value) -> Vec<Bytes> {
        let mut events = self.close_open();
        let index = self.alloc_index();
        let call_id = item
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let name = item.get("name").and_then(Value::as_str).unwrap_or("");
        let item_id = item
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(&call_id)
            .to_owned();
        events.push(sse(
            "content_block_start",
            &json!({ "type": "content_block_start", "index": index,
                     "content_block": { "type": "tool_use", "id": call_id, "name": name, "input": {} } }),
        ));
        self.tools.push(ToolBlock {
            index,
            item_id,
            call_id,
            done: false,
        });
        events
    }

    fn tool_arguments_delta(&mut self, item_id: &str, delta: &str) -> Vec<Bytes> {
        let Some(tool) = self.tools.iter().find(|t| t.item_id == item_id) else {
            return Vec::new();
        };
        vec![sse(
            "content_block_delta",
            &json!({ "type": "content_block_delta", "index": tool.index,
                     "delta": { "type": "input_json_delta", "partial_json": delta } }),
        )]
    }

    fn close_tool(&mut self, item: &Value) -> Vec<Bytes> {
        let call_id = item.get("call_id").and_then(Value::as_str).unwrap_or("");
        let Some(tool) = self
            .tools
            .iter_mut()
            .find(|t| t.call_id == call_id && !t.done)
        else {
            return Vec::new();
        };
        tool.done = true;
        vec![sse(
            "content_block_stop",
            &json!({ "type": "content_block_stop", "index": tool.index }),
        )]
    }

    fn close_pending_tools(&mut self) -> Vec<Bytes> {
        let mut events = Vec::new();
        for tool in &mut self.tools {
            if !tool.done {
                tool.done = true;
                events.push(sse(
                    "content_block_stop",
                    &json!({ "type": "content_block_stop", "index": tool.index }),
                ));
            }
        }
        events
    }

    /// `response.completed` / `response.incomplete`：收尾并发出唯一的 message_delta。
    /// 内层 `response.status` 为 failed/cancelled 或 error 非 null 时不能当成功
    /// 包装，改走 `fail` 发 error 事件——终态字段套在外层事件里，容易被漏检。
    fn complete(&mut self, data: &Value, incomplete: bool) -> Vec<Bytes> {
        if self.finished {
            return Vec::new();
        }
        let response = response_object(data);
        if terminal_failure_message(response).is_some() {
            let message = error_message(data);
            return vec![self.fail(&message)];
        }
        self.finished = true;
        let mut events = self.ensure_started();
        events.extend(self.close_open());
        events.extend(self.close_pending_tools());
        self.usage = Some(usage_to_anthropic(response.get("usage")));
        let has_tool_use = !self.tools.is_empty();
        let incomplete_reason = response
            .pointer("/incomplete_details/reason")
            .and_then(Value::as_str);
        let stop = resolve_stop_reason(incomplete, incomplete_reason, has_tool_use);
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

    pub(crate) fn handle_event(&mut self, event_type: &str, data: &Value) -> Vec<Bytes> {
        let response = response_object(data);
        if let Some(id) = response
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            id.clone_into(&mut self.message_id);
        }
        if let Some(model) = response
            .get("model")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            model.clone_into(&mut self.model);
        }

        match event_type {
            "response.output_text.delta" => {
                let mut events = self.ensure_started();
                if let Some(delta) = data.get("delta").and_then(Value::as_str) {
                    self.has_output = true;
                    events.extend(self.block_delta(OpenBlock::Text, delta));
                }
                events
            }
            // response.reasoning_text.delta / response.reasoning.delta 是同一语义的别名事件。
            "response.reasoning_summary_text.delta"
            | "response.reasoning_text.delta"
            | "response.reasoning.delta" => {
                let mut events = self.ensure_started();
                if let Some(delta) = data.get("delta").and_then(Value::as_str) {
                    self.has_output = true;
                    events.extend(self.block_delta(OpenBlock::Thinking, delta));
                }
                events
            }
            // 内容被安全过滤拒绝时上游走 refusal 增量而非 output_text，按文本处理。
            "response.refusal.delta" => {
                let mut events = self.ensure_started();
                if let Some(delta) = data.get("delta").and_then(Value::as_str) {
                    self.has_output = true;
                    events.extend(self.block_delta(OpenBlock::Text, delta));
                }
                events
            }
            "response.output_item.added" => {
                let mut events = self.ensure_started();
                if let Some(item) = data
                    .get("item")
                    .filter(|i| i.get("type").and_then(Value::as_str) == Some("function_call"))
                {
                    self.has_output = true;
                    events.extend(self.start_tool(item));
                }
                events
            }
            "response.function_call_arguments.delta" => {
                let mut events = self.ensure_started();
                if let (Some(item_id), Some(delta)) = (
                    data.get("item_id").and_then(Value::as_str),
                    data.get("delta").and_then(Value::as_str),
                ) {
                    self.has_output = true;
                    events.extend(self.tool_arguments_delta(item_id, delta));
                }
                events
            }
            "response.output_item.done" => {
                let mut events = self.ensure_started();
                if let Some(item) = data.get("item") {
                    events.extend(match item.get("type").and_then(Value::as_str) {
                        Some("reasoning") => self.close_reasoning(item),
                        Some("function_call") => self.close_tool(item),
                        _ => Vec::new(),
                    });
                }
                events
            }
            "response.completed" => self.complete(data, false),
            "response.incomplete" => self.complete(data, true),
            "response.failed" | "error" => {
                let message = error_message(data);
                vec![self.fail(&message)]
            }
            _ => self.ensure_started(),
        }
    }

    /// 上游报错：只发 error 事件，不把失败伪装成正常结束。
    pub(crate) fn fail(&mut self, message: &str) -> Bytes {
        self.finished = true;
        sse(
            "error",
            &json!({ "type": "error", "error": { "type": "api_error", "message": message } }),
        )
    }

    /// 流结束但没收到终结事件：有产出时按截断收尾，否则报错而非伪装成功。
    pub(crate) fn finish(&mut self) -> Vec<Bytes> {
        if self.finished {
            return Vec::new();
        }
        if !self.has_output {
            return vec![self.fail("上游 Responses 流在完成前结束")];
        }
        self.finished = true;
        let mut events = self.ensure_started();
        events.extend(self.close_open());
        events.extend(self.close_pending_tools());
        events.push(sse(
            "message_delta",
            &json!({
                "type": "message_delta",
                "delta": { "stop_reason": "max_tokens", "stop_sequence": null },
                "usage": self.usage.clone().unwrap_or_else(|| json!({ "input_tokens": 0, "output_tokens": 0 })),
            }),
        ));
        events.push(sse("message_stop", &json!({ "type": "message_stop" })));
        events
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
                    let event_type = chunk.get("type").and_then(Value::as_str).unwrap_or("");
                    events.extend(st.conv.handle_event(event_type, &chunk));
                    if st.conv.finished {
                        st.ended = true;
                        break;
                    }
                }
                events
            }
            Some(Err(error)) => {
                st.ended = true;
                vec![st.conv.fail(&format!("上游流中断：{error}"))]
            }
            None => {
                st.ended = true;
                st.conv.finish()
            }
        };
        Some((futures_util::stream::iter(events.into_iter().map(Ok)), st))
    })
    .flatten()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn request_converts_system_messages_tools_and_thinking() {
        let body = json!({
            "model": "gpt-5-codex", "max_tokens": 500, "stream": true,
            "system": [{ "type": "text", "text": "x-anthropic-billing-header: cch=1;\nBe concise." }],
            "thinking": { "type": "enabled", "budget_tokens": 2000 },
            "tools": [
                { "name": "Bash", "description": "run", "input_schema": { "properties": { "cmd": { "type": "string" } } } },
                { "type": "web_search_20250305", "name": "web_search" }
            ],
            "tool_choice": { "type": "any" },
            "messages": [
                { "role": "user", "content": [
                    { "type": "text", "text": "hi" },
                    { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAA" } }
                ] },
                { "role": "assistant", "content": [
                    { "type": "text", "text": "ok" },
                    { "type": "tool_use", "id": "t1", "name": "Bash", "input": { "cmd": "ls" } }
                ] },
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "t1", "content": [
                        { "type": "text", "text": "a.txt" },
                        { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "BBB" } }
                    ] }
                ] }
            ]
        });
        let responses = request_to_responses(&body);
        assert_eq!(responses["instructions"], "Be concise.");
        assert_eq!(responses["max_output_tokens"], 500);
        assert_eq!(responses["store"], false);
        assert_eq!(responses["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(
            responses["reasoning"],
            json!({ "effort": "low", "summary": "auto" })
        );

        let input = responses["input"].as_array().unwrap();
        assert_eq!(
            input[0]["content"][0],
            json!({ "type": "input_text", "text": "hi" })
        );
        assert_eq!(
            input[0]["content"][1]["image_url"],
            "data:image/png;base64,AAA"
        );
        assert_eq!(
            input[1]["content"][0],
            json!({ "type": "output_text", "text": "ok" })
        );
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["call_id"], "t1");
        assert_eq!(input[2]["arguments"], "{\"cmd\":\"ls\"}");
        assert_eq!(input[3]["type"], "function_call_output");
        assert_eq!(input[3]["call_id"], "t1");
        assert_eq!(input[3]["output"], "a.txt");
        assert_eq!(input[4]["role"], "user");
        assert_eq!(input[4]["content"][0]["type"], "input_image");

        let tools = responses["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1, "服务端工具不转换");
        assert_eq!(tools[0]["strict"], false);
        assert_eq!(tools[0]["parameters"]["type"], "object");
        assert_eq!(responses["tool_choice"], "required");
    }

    #[test]
    fn reasoning_signature_round_trips_through_request_and_response() {
        let responses_body = json!({
            "id": "resp_1", "model": "gpt-5-codex", "status": "completed",
            "output": [
                { "id": "rs_1", "type": "reasoning",
                  "summary": [{ "type": "summary_text", "text": "need a tool" }],
                  "encrypted_content": "opaque" },
                { "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "ok" }] }
            ],
            "usage": { "input_tokens": 10, "output_tokens": 5 }
        });
        let anthropic = response_from_responses(&responses_body).unwrap();
        let thinking = &anthropic["content"][0];
        assert_eq!(thinking["type"], "thinking");
        assert_eq!(thinking["thinking"], "need a tool");
        let signature = thinking["signature"].as_str().unwrap();
        assert!(signature.starts_with("mcpane-responses-v1:"));

        let next_request = json!({
            "model": "gpt-5-codex", "max_tokens": 100,
            "messages": [
                { "role": "assistant", "content": [
                    { "type": "thinking", "thinking": "need a tool", "signature": signature },
                    { "type": "text", "text": "ok" }
                ] }
            ]
        });
        let converted = request_to_responses(&next_request);
        let input = converted["input"].as_array().unwrap();
        assert_eq!(input[0]["type"], "reasoning");
        assert_eq!(input[0]["id"], "rs_1");
        assert_eq!(input[0]["encrypted_content"], "opaque");
        assert_eq!(input[1]["role"], "assistant");
    }

    #[test]
    fn foreign_thinking_block_is_dropped() {
        let body = json!({
            "model": "m", "max_tokens": 10,
            "messages": [{ "role": "assistant", "content": [
                { "type": "thinking", "thinking": "native", "signature": "sig-from-anthropic" },
                { "type": "redacted_thinking", "data": "opaque" },
                { "type": "text", "text": "ok" }
            ] }]
        });
        let input = request_to_responses(&body)["input"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(input.len(), 1, "非本模块签名的思考块整体丢弃，只剩文本消息");
        assert_eq!(input[0]["content"][0]["text"], "ok");
    }

    #[test]
    fn non_stream_response_maps_reasoning_text_tool_use_and_usage() {
        let body = json!({
            "id": "resp_2", "model": "gpt-5-codex", "status": "completed",
            "output": [
                { "type": "reasoning", "summary": [{ "type": "summary_text", "text": "plan" }] },
                { "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "doing" }] },
                { "type": "function_call", "call_id": "t1", "name": "Bash", "arguments": "{\"cmd\":\"ls\"}" }
            ],
            "usage": { "input_tokens": 100, "output_tokens": 9, "input_tokens_details": { "cached_tokens": 60 } }
        });
        let resp = response_from_responses(&body).unwrap();
        assert_eq!(resp["content"][0]["type"], "thinking");
        assert_eq!(resp["content"][1]["text"], "doing");
        assert_eq!(resp["content"][2]["input"]["cmd"], "ls");
        assert_eq!(resp["stop_reason"], "tool_use");
        assert_eq!(resp["usage"]["input_tokens"], 40);
        assert_eq!(resp["usage"]["cache_read_input_tokens"], 60);
    }

    #[test]
    fn non_stream_incomplete_status_maps_to_max_tokens() {
        let body = json!({
            "id": "resp_3", "model": "m", "status": "incomplete",
            "incomplete_details": { "reason": "max_output_tokens" },
            "output": [{ "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "cut" }] }],
            "usage": { "input_tokens": 5, "output_tokens": 3 }
        });
        let resp = response_from_responses(&body).unwrap();
        assert_eq!(resp["stop_reason"], "max_tokens");
    }

    #[test]
    fn non_stream_failed_status_is_rejected() {
        let body = json!({ "status": "failed", "error": { "message": "boom" }, "output": [] });
        let err = response_from_responses(&body).unwrap_err();
        assert!(err.contains("boom"));
    }

    #[test]
    fn non_stream_cancelled_status_is_rejected() {
        let body = json!({ "status": "cancelled", "output": [] });
        assert!(response_from_responses(&body).is_err());
    }

    #[test]
    fn non_stream_error_field_is_rejected_regardless_of_status() {
        let body =
            json!({ "status": "completed", "error": { "message": "rate limited" }, "output": [] });
        let err = response_from_responses(&body).unwrap_err();
        assert!(err.contains("rate limited"));
    }

    #[test]
    fn non_stream_refusal_part_becomes_text_block() {
        let body = json!({
            "id": "resp_5", "model": "m", "status": "completed",
            "output": [{ "type": "message", "role": "assistant", "content": [
                { "type": "refusal", "refusal": "I can't help with that." }
            ] }],
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        });
        let resp = response_from_responses(&body).unwrap();
        assert_eq!(
            resp["content"][0],
            json!({ "type": "text", "text": "I can't help with that." })
        );
    }

    #[test]
    fn non_stream_incomplete_precedes_tool_use_and_content_filter_maps_to_refusal() {
        let body = json!({
            "id": "resp_4", "model": "m", "status": "incomplete",
            "incomplete_details": { "reason": "content_filter" },
            "output": [
                { "type": "function_call", "call_id": "t1", "name": "Bash", "arguments": "{\"cmd\":\"l" }
            ],
            "usage": { "input_tokens": 5, "output_tokens": 3 }
        });
        let resp = response_from_responses(&body).unwrap();
        assert_eq!(
            resp["stop_reason"], "refusal",
            "incomplete 优先于 tool_use，避免客户端拿残缺参数执行工具"
        );
    }

    #[test]
    fn request_reasoning_effort_uses_shared_module_and_gates_by_model() {
        let supported = request_to_responses(&json!({
            "model": "gpt-5-codex", "max_tokens": 100,
            "output_config": { "effort": "high" }, "messages": []
        }));
        assert_eq!(
            supported["reasoning"],
            json!({ "effort": "high", "summary": "auto" })
        );
        assert_eq!(supported["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(supported["store"], false);

        let unsupported = request_to_responses(&json!({
            "model": "gpt-4.1", "max_tokens": 100,
            "thinking": { "type": "enabled", "budget_tokens": 30000 }, "messages": []
        }));
        assert!(
            unsupported.get("reasoning").is_none(),
            "非推理模型不应带 reasoning"
        );
        assert!(
            unsupported.get("include").is_none(),
            "非推理模型带 encrypted_content include 会被上游 400"
        );
        assert_eq!(unsupported["store"], false);
    }

    #[test]
    fn request_clamps_tiny_max_tokens_to_responses_minimum() {
        let tiny = request_to_responses(&json!({ "model": "m", "max_tokens": 1, "messages": [] }));
        assert_eq!(tiny["max_output_tokens"], 16);

        let normal =
            request_to_responses(&json!({ "model": "m", "max_tokens": 500, "messages": [] }));
        assert_eq!(normal["max_output_tokens"], 500);
    }

    #[test]
    fn request_tool_schema_uses_shared_clean_schema_and_fills_missing_root_properties() {
        let body = json!({
            "model": "m", "max_tokens": 100,
            "tools": [
                { "name": "ReadFile", "input_schema": {
                    "type": "object",
                    "properties": { "path": { "type": "string", "format": "uri" } }
                } },
                { "name": "NoSchema", "input_schema": {} }
            ],
            "messages": []
        });
        let tools = request_to_responses(&body)["tools"]
            .as_array()
            .unwrap()
            .clone();
        assert!(
            tools[0]["parameters"]["properties"]["path"]
                .get("format")
                .is_none(),
            "clean_schema 应剥掉 format: uri"
        );
        assert_eq!(
            tools[1]["parameters"],
            json!({ "type": "object", "properties": {} })
        );
    }

    #[test]
    fn assistant_reasoning_only_turn_is_dropped_to_avoid_dangling_reasoning_item() {
        let signature = encode_reasoning_signature(&json!({
            "type": "reasoning", "id": "rs_x",
            "summary": [{ "type": "summary_text", "text": "" }]
        }));
        let body = json!({
            "model": "m", "max_tokens": 10,
            "messages": [
                { "role": "user", "content": "hi" },
                { "role": "assistant", "content": [
                    { "type": "thinking", "thinking": "", "signature": signature }
                ] }
            ]
        });
        let input = request_to_responses(&body)["input"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(
            input.len(),
            1,
            "无后续项的 reasoning-only assistant 轮次应整体丢弃"
        );
        assert_eq!(input[0]["content"][0]["text"], "hi");
    }

    #[test]
    fn assistant_reasoning_followed_by_tool_call_is_kept() {
        let signature = encode_reasoning_signature(&json!({
            "type": "reasoning", "id": "rs_y",
            "summary": [{ "type": "summary_text", "text": "plan" }]
        }));
        let body = json!({
            "model": "m", "max_tokens": 10,
            "messages": [{ "role": "assistant", "content": [
                { "type": "thinking", "thinking": "plan", "signature": signature },
                { "type": "tool_use", "id": "t1", "name": "Bash", "input": { "cmd": "ls" } }
            ] }]
        });
        let input = request_to_responses(&body)["input"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(input.len(), 2, "reasoning 后跟着 function_call，应保留");
        assert_eq!(input[0]["type"], "reasoning");
        assert_eq!(input[1]["type"], "function_call");
    }

    fn events_of(out: &[Bytes]) -> Vec<(String, Value)> {
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
    async fn stream_converts_reasoning_text_tool_call_and_completes_once() {
        let events = [
            r#"{"type":"response.created","response":{"id":"resp_9","model":"gpt-5-codex"}}"#,
            r#"{"type":"response.reasoning_summary_text.delta","item_id":"rs_9","output_index":0,"delta":"pla"}"#,
            r#"{"type":"response.reasoning_summary_text.delta","item_id":"rs_9","output_index":0,"delta":"n"}"#,
            r#"{"type":"response.output_item.done","output_index":0,"item":{"id":"rs_9","type":"reasoning","summary":[{"type":"summary_text","text":"plan"}],"encrypted_content":"opaque"}}"#,
            r#"{"type":"response.output_text.delta","item_id":"msg_9","output_index":1,"delta":"Hi"}"#,
            r#"{"type":"response.output_item.added","output_index":2,"item":{"id":"fc_9","type":"function_call","call_id":"t1","name":"Bash"}}"#,
            r#"{"type":"response.function_call_arguments.delta","item_id":"fc_9","output_index":2,"delta":"{\"cmd\""}"#,
            r#"{"type":"response.function_call_arguments.delta","item_id":"fc_9","output_index":2,"delta":":\"ls\"}"}"#,
            r#"{"type":"response.output_item.done","output_index":2,"item":{"id":"fc_9","type":"function_call","call_id":"t1","name":"Bash","arguments":"{\"cmd\":\"ls\"}"}}"#,
            r#"{"type":"response.completed","response":{"id":"resp_9","status":"completed","usage":{"input_tokens":8,"output_tokens":4}}}"#,
        ];
        let mut raw = String::new();
        for e in events {
            raw.push_str(&format!("data: {e}\n\n"));
        }
        let bytes = raw.into_bytes();
        // 故意在事件中间切开，验证跨块 SSE 解析。
        let cut = bytes.len() / 2;
        let (a, b) = bytes.split_at(cut);
        let upstream = futures_util::stream::iter(vec![
            Ok::<_, std::io::Error>(Bytes::copy_from_slice(a)),
            Ok(Bytes::copy_from_slice(b)),
        ]);
        let out: Vec<Bytes> = convert_stream(upstream).map(|r| r.unwrap()).collect().await;
        let ev = events_of(&out);
        let names: Vec<&str> = ev.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(names.first(), Some(&"message_start"));
        assert_eq!(names.last(), Some(&"message_stop"));
        assert_eq!(names.iter().filter(|n| **n == "message_delta").count(), 1);
        // thinking 块 → text 块 → tool_use 块，各自开关成对。
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
        assert_eq!(json_parts, "{\"cmd\":\"ls\"}");
        let signature = ev
            .iter()
            .find_map(|(_, d)| d.pointer("/delta/signature").and_then(Value::as_str))
            .unwrap();
        assert!(signature.starts_with("mcpane-responses-v1:"));
        let delta = &ev.iter().find(|(e, _)| e == "message_delta").unwrap().1;
        assert_eq!(delta["delta"]["stop_reason"], "tool_use");
        assert_eq!(delta["usage"]["output_tokens"], 4);
    }

    #[tokio::test]
    async fn stream_failure_event_becomes_single_error_event() {
        let raw = "data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"message\":\"quota exceeded\"}}}\n\n";
        let upstream = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(raw))]);
        let out: Vec<Bytes> = convert_stream(upstream).map(|r| r.unwrap()).collect().await;
        let ev = events_of(&out);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].0, "error");
        assert_eq!(ev[0].1["error"]["message"], "quota exceeded");
    }

    #[tokio::test]
    async fn stream_completed_with_cancelled_status_becomes_error_not_success() {
        let raw = "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r\",\"status\":\"cancelled\"}}\n\n";
        let upstream = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(raw))]);
        let out: Vec<Bytes> = convert_stream(upstream).map(|r| r.unwrap()).collect().await;
        let ev = events_of(&out);
        assert_eq!(
            ev.len(),
            1,
            "cancelled 终态不能包装成 message_stop 成功收尾"
        );
        assert_eq!(ev[0].0, "error");
    }

    #[tokio::test]
    async fn stream_incomplete_with_error_field_becomes_error_not_success() {
        let raw = "data: {\"type\":\"response.incomplete\",\"response\":{\"id\":\"r\",\"status\":\"incomplete\",\"error\":{\"message\":\"boom\"}}}\n\n";
        let upstream = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(raw))]);
        let out: Vec<Bytes> = convert_stream(upstream).map(|r| r.unwrap()).collect().await;
        let ev = events_of(&out);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].0, "error");
        assert_eq!(ev[0].1["error"]["message"], "boom");
    }

    #[tokio::test]
    async fn stream_incomplete_precedes_tool_use_and_content_filter_maps_to_refusal() {
        let events = [
            r#"{"type":"response.output_item.added","output_index":0,"item":{"id":"fc_1","type":"function_call","call_id":"t1","name":"Bash"}}"#,
            r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{\"cmd\""}"#,
            r#"{"type":"response.incomplete","response":{"id":"r","status":"incomplete","incomplete_details":{"reason":"content_filter"},"usage":{"input_tokens":1,"output_tokens":1}}}"#,
        ];
        let mut raw = String::new();
        for e in events {
            raw.push_str(&format!("data: {e}\n\n"));
        }
        let upstream = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(raw))]);
        let out: Vec<Bytes> = convert_stream(upstream).map(|r| r.unwrap()).collect().await;
        let ev = events_of(&out);
        let delta = &ev.iter().find(|(e, _)| e == "message_delta").unwrap().1;
        assert_eq!(
            delta["delta"]["stop_reason"], "refusal",
            "incomplete 优先于 tool_use，避免客户端执行残缺工具调用"
        );
    }

    #[tokio::test]
    async fn stream_refusal_and_reasoning_alias_events_map_to_expected_deltas() {
        let raw = concat!(
            "data: {\"type\":\"response.refusal.delta\",\"delta\":\"no\"}\n\n",
            "data: {\"type\":\"response.reasoning_text.delta\",\"delta\":\"think\"}\n\n",
        );
        let upstream = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(raw))]);
        let out: Vec<Bytes> = convert_stream(upstream).map(|r| r.unwrap()).collect().await;
        let ev = events_of(&out);
        let text_delta = ev.iter().find_map(|(_, d)| {
            (d["delta"]["type"] == "text_delta")
                .then(|| d["delta"]["text"].as_str().unwrap().to_owned())
        });
        assert_eq!(
            text_delta.as_deref(),
            Some("no"),
            "response.refusal.delta 应作为文本增量输出"
        );
        let thinking_delta = ev.iter().find_map(|(_, d)| {
            (d["delta"]["type"] == "thinking_delta")
                .then(|| d["delta"]["thinking"].as_str().unwrap().to_owned())
        });
        assert_eq!(
            thinking_delta.as_deref(),
            Some("think"),
            "response.reasoning_text.delta 应作为 thinking 增量输出"
        );
    }

    #[tokio::test]
    async fn stream_ends_without_any_output_becomes_error() {
        let empty = futures_util::stream::iter(Vec::<Result<Bytes, std::io::Error>>::new());
        let out: Vec<Bytes> = convert_stream(empty).map(|r| r.unwrap()).collect().await;
        let ev = events_of(&out);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].0, "error");
    }

    #[tokio::test]
    async fn stream_ends_with_output_but_no_completion_uses_max_tokens() {
        let raw = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n";
        let upstream = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(raw))]);
        let out: Vec<Bytes> = convert_stream(upstream).map(|r| r.unwrap()).collect().await;
        let ev = events_of(&out);
        let delta = &ev.iter().find(|(e, _)| e == "message_delta").unwrap().1;
        assert_eq!(delta["delta"]["stop_reason"], "max_tokens");
        assert_eq!(names_last(&ev), "message_stop");
    }

    fn names_last(ev: &[(String, Value)]) -> &str {
        ev.last().map(|(e, _)| e.as_str()).unwrap_or("")
    }
}
