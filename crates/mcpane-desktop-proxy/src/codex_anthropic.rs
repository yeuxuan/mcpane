//! Codex（Responses API）↔ Anthropic Messages 协议转换（ADR-022 三期）。
//!
//! 场景：Codex CLI 只讲 Responses 协议，部分厂商网关只提供原生 Anthropic
//! `/v1/messages`。转换语义按两家协议的公开规范实现；不做 adaptive
//! thinking（特定厂商专属自适应思考）、工具结果图片搬运与厂商特化分支。
//! 签名信封用纯 JSON 文本而非 base64（不引入额外依赖，
//! serde_json 自动转义，语义等价，只是体积略大）。

use std::collections::BTreeMap;

use axum::body::Bytes;
use futures_util::{Stream, StreamExt};
use serde_json::{Map, Value, json};

use crate::codex_chat::{ToolContext, custom_input_from_arguments};

const TOOL_SEARCH_NAME: &str = "tool_search";
pub(crate) const DEFAULT_MAX_TOKENS: u64 = 32000;
const THINKING_BUDGET_HEADROOM: u64 = 8192;
/// 我们自产 reasoning 的信封前缀；只回放带此前缀的思考块，外来厂商的原样丢弃。
const REASONING_ENCRYPTED_PREFIX: &str = "mcpane-anthropic-v1:";

fn has_text(text: &str) -> bool {
    !text.trim().is_empty()
}

// ---------------------------------------------------------------------------
// reasoning 信封：Anthropic 签名 thinking/redacted_thinking ↔ Responses encrypted_content
// ---------------------------------------------------------------------------

/// 只有带签名的 thinking / 带 data 的 redacted_thinking 才可安全回放。
fn encode_thinking_block(block: &Value) -> Option<String> {
    match block.get("type").and_then(Value::as_str) {
        Some("thinking")
            if block
                .get("signature")
                .and_then(Value::as_str)
                .is_some_and(has_text) => {}
        Some("redacted_thinking")
            if block
                .get("data")
                .and_then(Value::as_str)
                .is_some_and(has_text) => {}
        _ => return None,
    }
    let text = serde_json::to_string(block).ok()?;
    Some(format!("{REASONING_ENCRYPTED_PREFIX}{text}"))
}

fn decode_thinking_block(encrypted: &str) -> Option<Value> {
    let payload = encrypted.strip_prefix(REASONING_ENCRYPTED_PREFIX)?;
    let block: Value = serde_json::from_str(payload).ok()?;
    // 借编码器的校验，拒绝信封被截断/伪造成不可回放形态的情况。
    encode_thinking_block(&block).map(|_| block)
}

// ---------------------------------------------------------------------------
// 工具目录：Chat function 形态 → Anthropic tool
// ---------------------------------------------------------------------------

fn chat_tool_to_anthropic(tool: &Value) -> Option<Value> {
    let function = tool.get("function")?;
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| has_text(n))?;
    let mut input_schema = function
        .get("parameters")
        .cloned()
        .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
    if input_schema.get("type").and_then(Value::as_str) != Some("object")
        && let Some(obj) = input_schema.as_object_mut()
    {
        obj.insert("type".into(), json!("object"));
    }
    let mut out = json!({ "name": name, "input_schema": input_schema });
    if let Some(description) = function.get("description").and_then(Value::as_str) {
        out["description"] = json!(description);
    }
    Some(out)
}

fn tool_choice_to_anthropic(choice: &Value, ctx: &ToolContext) -> Value {
    let kind = choice
        .as_str()
        .or_else(|| choice.get("type").and_then(Value::as_str));
    match kind {
        Some("auto") => json!({ "type": "auto" }),
        Some("required") => json!({ "type": "any" }),
        Some("none") => json!({ "type": "none" }),
        Some("function") => {
            let name = ctx.chat_name_for(
                choice.get("name").and_then(Value::as_str).unwrap_or(""),
                choice.get("namespace").and_then(Value::as_str),
            );
            json!({ "type": "tool", "name": name })
        }
        Some("custom") => json!({
            "type": "tool",
            "name": choice.get("name").and_then(Value::as_str).unwrap_or("")
        }),
        Some("tool_search") => json!({ "type": "tool", "name": TOOL_SEARCH_NAME }),
        _ => json!({ "type": "auto" }),
    }
}

/// Codex `reasoning.effort` → Anthropic thinking 预算（token）；none/minimal/未知一律不开思考。
fn effort_budget(effort: &str) -> Option<u64> {
    match effort.trim().to_ascii_lowercase().as_str() {
        "low" => Some(4096),
        "medium" => Some(10000),
        "high" => Some(24000),
        "xhigh" => Some(32000),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 请求：Responses → Anthropic
// ---------------------------------------------------------------------------

fn text_of(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str).or_else(|| p.as_str()))
            .filter(|s| has_text(s))
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

/// 请求体 → Anthropic 请求；同时返回工具上下文供响应侧还原。
pub(crate) fn request_to_anthropic(body: &Value) -> (Value, ToolContext) {
    let ctx = ToolContext::from_request(body);
    let mut out = json!({});
    if let Some(model) = body.get("model") {
        out["model"] = model.clone();
    }

    // instructions + system/developer 历史消息 → 顶层 system；Anthropic messages 只认 user/assistant。
    let mut system_parts = Vec::new();
    if let Some(instructions) = body.get("instructions") {
        let text = text_of(instructions);
        if has_text(&text) {
            system_parts.push(text);
        }
    }
    if let Some(items) = body.get("input").and_then(Value::as_array) {
        for item in items {
            if matches!(
                item.get("role").and_then(Value::as_str),
                Some("system" | "developer")
            ) {
                let text = item.get("content").map(text_of).unwrap_or_default();
                if has_text(&text) {
                    system_parts.push(text);
                }
            }
        }
    }
    if !system_parts.is_empty() {
        out["system"] = json!(system_parts.join("\n\n"));
    }

    let mut messages = Vec::new();
    match body.get("input") {
        Some(Value::String(text)) if has_text(text) => {
            messages.push(json!({ "role": "user", "content": [{ "type": "text", "text": text }] }));
        }
        Some(Value::Array(items)) => convert_items(items, &ctx, &mut messages),
        _ => {}
    }
    // 会话压缩/续接、Ctrl-C 中断都可能留下孤儿 tool_result / 未答复的 tool_use，
    // 按回合整体清洗，否则 Anthropic 直接 400。
    drop_incomplete_tool_turns(&mut messages);
    drop_empty_messages(&mut messages);
    if messages.is_empty() {
        messages.push(json!({ "role": "user", "content": [{ "type": "text", "text": "" }] }));
    } else if messages
        .first()
        .and_then(|m| m.get("role"))
        .and_then(Value::as_str)
        != Some("user")
    {
        // 压缩/续接会话可能以 assistant/tool_use 开头，Anthropic 要求首条必须是 user。
        messages.insert(
            0,
            json!({ "role": "user", "content": [{ "type": "text", "text": "(continuing the conversation)" }] }),
        );
    }
    trim_trailing_assistant_text(&mut messages);
    drop_empty_messages(&mut messages);
    if messages.is_empty() {
        messages.push(json!({ "role": "user", "content": [{ "type": "text", "text": "" }] }));
    }
    out["messages"] = Value::Array(messages);

    let explicit_max_tokens = body
        .get("max_output_tokens")
        .and_then(Value::as_u64)
        .filter(|v| *v > 0);
    let mut max_tokens = explicit_max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
    let mut thinking_budget = body
        .pointer("/reasoning/effort")
        .and_then(Value::as_str)
        .and_then(effort_budget);
    if let Some(budget) = thinking_budget {
        if explicit_max_tokens.is_some() {
            // Codex 显式给了输出上限：路由层的「上游报超限 → 下调 max_output_tokens 重试」
            // 整流依赖这个上限保持不变，这里只钳思考预算、不抬高 max_tokens；预算钳到上限
            // 一半以下给可见输出留空间，低于 1024（Anthropic 最小值）就干脆不开思考。
            let clamped = budget.min(max_tokens / 2);
            if clamped >= 1024 {
                out["thinking"] = json!({ "type": "enabled", "budget_tokens": clamped });
            } else {
                thinking_budget = None;
            }
        } else {
            if max_tokens <= budget {
                max_tokens = budget + THINKING_BUDGET_HEADROOM;
            }
            out["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
        }
    }
    out["max_tokens"] = json!(max_tokens);

    // Anthropic 扩展思考期间不接受显式采样参数（400）；未开思考时才透传。
    if thinking_budget.is_none() {
        if let Some(v) = body.get("temperature") {
            out["temperature"] = v.clone();
        }
        if let Some(v) = body.get("top_p") {
            out["top_p"] = v.clone();
        }
    }
    if let Some(v) = body.get("stop") {
        out["stop_sequences"] = v.clone();
    }
    if let Some(v) = body.get("stream") {
        out["stream"] = v.clone();
    }

    let tools: Vec<Value> = ctx
        .chat_tools()
        .iter()
        .filter_map(chat_tool_to_anthropic)
        .collect();
    if !tools.is_empty() {
        out["tools"] = Value::Array(tools);
        let mut tool_choice = body
            .get("tool_choice")
            .map(|c| tool_choice_to_anthropic(c, &ctx));
        if body.get("parallel_tool_calls").and_then(Value::as_bool) == Some(false) {
            let mut choice = tool_choice.unwrap_or_else(|| json!({ "type": "auto" }));
            if let Some(obj) = choice.as_object_mut() {
                obj.insert("disable_parallel_tool_use".into(), json!(true));
            }
            tool_choice = Some(choice);
        }
        if let Some(choice) = tool_choice {
            out["tool_choice"] = choice;
        }
    }

    // Codex 长会话每轮都会把 system/tools/历史原样重发；不打断点就没法命中
    // Anthropic 的 prompt cache，只能按全价重复计费。
    inject_prompt_cache(&mut out);

    (out, ctx)
}

fn parse_arguments(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(s)) if has_text(s) => serde_json::from_str::<Value>(s)
            .ok()
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({})),
        _ => json!({}),
    }
}

/// Responses input[] → Anthropic messages；同角色相邻内容合并进同一条消息。
fn convert_items(items: &[Value], ctx: &ToolContext, messages: &mut Vec<Value>) {
    for item in items {
        let item_type = item.get("type").and_then(Value::as_str);
        // 压缩/中断会话里未跑完的工具调用（status=incomplete）连同它的输出一起丢弃，
        // 避免半截 tool_use 进 Anthropic 消息。
        if matches!(
            item_type,
            Some("function_call" | "custom_tool_call" | "tool_search_call")
        ) && item.get("status").and_then(Value::as_str) == Some("incomplete")
        {
            continue;
        }
        match item_type {
            Some("function_call") => {
                let call_id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let chat_name = ctx.chat_name_for(
                    item.get("name").and_then(Value::as_str).unwrap_or(""),
                    item.get("namespace").and_then(Value::as_str),
                );
                let input = parse_arguments(item.get("arguments"));
                push_block(
                    messages,
                    "assistant",
                    json!({ "type": "tool_use", "id": call_id, "name": chat_name, "input": input }),
                );
            }
            Some("custom_tool_call") => {
                let call_id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                let input_value = item.get("input").cloned().unwrap_or_else(|| json!(""));
                push_block(
                    messages,
                    "assistant",
                    json!({ "type": "tool_use", "id": call_id, "name": name,
                            "input": { "input": input_value } }),
                );
            }
            Some("tool_search_call") => {
                let call_id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let input = item
                    .get("arguments")
                    .cloned()
                    .filter(Value::is_object)
                    .unwrap_or_else(|| json!({}));
                push_block(
                    messages,
                    "assistant",
                    json!({ "type": "tool_use", "id": call_id, "name": TOOL_SEARCH_NAME, "input": input }),
                );
            }
            Some("function_call_output" | "custom_tool_call_output" | "tool_search_output") => {
                let call_id = item.get("call_id").and_then(Value::as_str).unwrap_or("");
                let content = tool_result_content(item.get("output"));
                push_tool_result(messages, call_id, content);
            }
            Some("reasoning") => {
                if let Some(block) = item
                    .get("encrypted_content")
                    .and_then(Value::as_str)
                    .and_then(decode_thinking_block)
                {
                    push_thinking(messages, block);
                }
                // 外来厂商的 reasoning（前缀不匹配、无法解出签名）静默丢弃。
            }
            Some("additional_tools") => {}
            _ => {
                let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
                if matches!(role, "system" | "developer") {
                    continue;
                }
                let anth_role = if role == "assistant" {
                    "assistant"
                } else {
                    "user"
                };
                match item.get("content") {
                    Some(Value::String(text)) if has_text(text) => {
                        push_block(messages, anth_role, json!({ "type": "text", "text": text }));
                    }
                    Some(Value::Array(parts)) => {
                        for part in parts {
                            match part.get("type").and_then(Value::as_str).unwrap_or("") {
                                "input_text" | "output_text" => {
                                    if let Some(text) = part
                                        .get("text")
                                        .and_then(Value::as_str)
                                        .filter(|t| has_text(t))
                                    {
                                        push_block(
                                            messages,
                                            anth_role,
                                            json!({ "type": "text", "text": text }),
                                        );
                                    }
                                }
                                "refusal" => {
                                    if let Some(text) = part
                                        .get("refusal")
                                        .and_then(Value::as_str)
                                        .filter(|t| has_text(t))
                                    {
                                        push_block(
                                            messages,
                                            anth_role,
                                            json!({ "type": "text", "text": text }),
                                        );
                                    }
                                }
                                "input_image" => {
                                    if let Some(block) = image_block(part) {
                                        push_block(messages, anth_role, block);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn tool_result_content(output: Option<&Value>) -> Value {
    match output {
        Some(Value::String(s)) => json!(s),
        Some(Value::Array(parts)) => {
            let blocks: Vec<Value> = parts
                .iter()
                .filter_map(|p| match p.get("type").and_then(Value::as_str) {
                    Some("input_text" | "output_text" | "text") => p
                        .get("text")
                        .and_then(Value::as_str)
                        .map(|t| json!({ "type": "text", "text": t })),
                    Some("input_image") => image_block(p),
                    _ => None,
                })
                .collect();
            Value::Array(blocks)
        }
        Some(other) => json!(other.to_string()),
        None => json!(""),
    }
}

/// Responses `input_image` → Anthropic image block（data URL 走 base64，其余走 url）。
fn image_block(part: &Value) -> Option<Value> {
    let url = part.get("image_url").and_then(|v| {
        v.as_str()
            .map(str::to_owned)
            .or_else(|| v.get("url").and_then(Value::as_str).map(str::to_owned))
    })?;
    if let Some(rest) = url.strip_prefix("data:") {
        let (meta, data) = rest.split_once(',')?;
        let media_type = meta.split(';').next().unwrap_or("image/png");
        Some(json!({
            "type": "image",
            "source": { "type": "base64", "media_type": media_type, "data": data }
        }))
    } else {
        Some(json!({ "type": "image", "source": { "type": "url", "url": url } }))
    }
}

fn push_block(messages: &mut Vec<Value>, role: &str, block: Value) {
    if let Some(last) = messages.last_mut()
        && last.get("role").and_then(Value::as_str) == Some(role)
        && let Some(arr) = last.get_mut("content").and_then(Value::as_array_mut)
    {
        arr.push(block);
        return;
    }
    messages.push(json!({ "role": role, "content": [block] }));
}

/// tool_result 必须排在 user 回合其余内容之前（Anthropic 硬性要求）。
fn push_tool_result(messages: &mut Vec<Value>, call_id: &str, content: Value) {
    let block = json!({ "type": "tool_result", "tool_use_id": call_id, "content": content });
    if let Some(last) = messages.last_mut()
        && last.get("role").and_then(Value::as_str) == Some("user")
        && let Some(arr) = last.get_mut("content").and_then(Value::as_array_mut)
    {
        let pos = arr
            .iter()
            .position(|b| b.get("type").and_then(Value::as_str) != Some("tool_result"))
            .unwrap_or(arr.len());
        arr.insert(pos, block);
        return;
    }
    messages.push(json!({ "role": "user", "content": [block] }));
}

/// 还原的签名 thinking 块插在同回合已有思考块之后、其余内容之前。
fn push_thinking(messages: &mut Vec<Value>, block: Value) {
    if let Some(last) = messages.last_mut()
        && last.get("role").and_then(Value::as_str) == Some("assistant")
        && let Some(arr) = last.get_mut("content").and_then(Value::as_array_mut)
    {
        let idx = arr
            .iter()
            .take_while(|b| {
                matches!(
                    b.get("type").and_then(Value::as_str),
                    Some("thinking" | "redacted_thinking")
                )
            })
            .count();
        arr.insert(idx, block);
        return;
    }
    messages.push(json!({ "role": "assistant", "content": [block] }));
}

/// 压缩/续接会话或 Ctrl-C 中断可能留下没有配对的 tool_use / tool_result：
/// 前者会被 Anthropic 拒收（未回复的工具调用），后者是孤儿结果（配对的调用已被丢弃）。
/// 按回合整体丢弃而非裁剪，避免改动一条已签名 thinking/tool_use 消息的子集。
fn drop_incomplete_tool_turns(messages: &mut Vec<Value>) {
    let original = std::mem::take(messages);
    let mut sanitized = Vec::with_capacity(original.len());
    let mut index = 0;

    while index < original.len() {
        let message = &original[index];
        let is_assistant = message.get("role").and_then(Value::as_str) == Some("assistant");
        let tool_use_ids = if is_assistant {
            message_block_ids(message, "tool_use", "id")
        } else {
            Vec::new()
        };

        if !tool_use_ids.is_empty() {
            let paired_user = original
                .get(index + 1)
                .filter(|next| next.get("role").and_then(Value::as_str) == Some("user"));
            match paired_user {
                Some(user) => {
                    let tool_result_ids = message_block_ids(user, "tool_result", "tool_use_id");
                    let unique_uses: std::collections::HashSet<&str> =
                        tool_use_ids.iter().copied().collect();
                    let unique_results: std::collections::HashSet<&str> =
                        tool_result_ids.iter().copied().collect();
                    let complete = tool_use_ids.iter().all(|id| !id.is_empty())
                        && tool_result_ids.iter().all(|id| !id.is_empty())
                        && unique_uses.len() == tool_use_ids.len()
                        && unique_results.len() == tool_result_ids.len()
                        && unique_uses == unique_results;

                    if complete {
                        sanitized.push(message.clone());
                        sanitized.push(user.clone());
                    } else {
                        let mut user = user.clone();
                        drop_tool_result_blocks(&mut user);
                        if message_has_content(&user) {
                            sanitized.push(user);
                        }
                    }
                    index += 2;
                }
                None => index += 1,
            }
            continue;
        }

        let mut message = message.clone();
        if message.get("role").and_then(Value::as_str) == Some("user") {
            // 没被当作完整配对消费的 user 回合，不能残留任何 tool_result 块。
            drop_tool_result_blocks(&mut message);
        }
        if message_has_content(&message) {
            sanitized.push(message);
        }
        index += 1;
    }

    *messages = sanitized;
}

fn message_block_ids<'a>(message: &'a Value, block_type: &str, id_field: &str) -> Vec<&'a str> {
    message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some(block_type))
        .map(|block| block.get(id_field).and_then(Value::as_str).unwrap_or(""))
        .collect()
}

fn drop_tool_result_blocks(message: &mut Value) {
    if let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) {
        content.retain(|block| block.get("type").and_then(Value::as_str) != Some("tool_result"));
    }
}

fn message_has_content(message: &Value) -> bool {
    message
        .get("content")
        .and_then(Value::as_array)
        .map(|content| !content.is_empty())
        .unwrap_or(true)
}

fn drop_empty_messages(messages: &mut Vec<Value>) {
    messages.retain(message_has_content);
}

/// 空白/纯空白的 assistant 结尾文本块整体丢弃，非空文本去掉尾随空白：
/// Anthropic 拒收以空白结尾的 assistant 预填文本。
fn trim_trailing_assistant_text(messages: &mut [Value]) {
    let Some(last) = messages.last_mut() else {
        return;
    };
    if last.get("role").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    let Some(blocks) = last.get_mut("content").and_then(Value::as_array_mut) else {
        return;
    };
    let Some(block) = blocks.last_mut() else {
        return;
    };
    if block.get("type").and_then(Value::as_str) != Some("text") {
        return;
    }
    let Some(text) = block.get("text").and_then(Value::as_str) else {
        return;
    };
    let trimmed = text.trim_end();
    if trimmed.is_empty() {
        blocks.pop();
    } else if trimmed.len() != text.len() {
        block["text"] = json!(trimmed);
    }
}

// ---------------------------------------------------------------------------
// Prompt cache：断点注入（system 末尾 / tools 末尾 / 最近消息 / 次新 user 回合）
// ---------------------------------------------------------------------------

const CACHE_BREAKPOINT_CAP: usize = 4;

fn make_cache_control() -> Value {
    json!({ "type": "ephemeral" })
}

fn count_existing_cache_breakpoints(body: &Value) -> usize {
    let mut count = 0;
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        count += tools
            .iter()
            .filter(|t| t.get("cache_control").is_some())
            .count();
    }
    if let Some(system) = body.get("system").and_then(Value::as_array) {
        count += system
            .iter()
            .filter(|b| b.get("cache_control").is_some())
            .count();
    }
    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        for message in messages {
            if let Some(content) = message.get("content").and_then(Value::as_array) {
                count += content
                    .iter()
                    .filter(|b| b.get("cache_control").is_some())
                    .count();
            }
        }
    }
    count
}

/// 消息最后一个非 thinking 块打断点；thinking 块不可缓存也不该被截断显示。
fn inject_message_breakpoint(message: &mut Value) -> bool {
    let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) else {
        return false;
    };
    let Some(block) = content.iter_mut().rev().find(|block| {
        !matches!(
            block.get("type").and_then(Value::as_str),
            Some("thinking" | "redacted_thinking")
        )
    }) else {
        return false;
    };
    if block.get("cache_control").is_some() {
        return false;
    }
    let Some(object) = block.as_object_mut() else {
        return false;
    };
    object.insert("cache_control".into(), make_cache_control());
    true
}

/// 按 (a)tools 末尾 (b)system 末尾 (c)最近消息 (d)次新 user 回合 的顺序注入缓存断点，
/// 已有断点计入总预算（上限 4），默认 TTL 不写 ttl 字段（即 5m）。Codex 长会话每轮都会
/// 把 system/tools/历史原样重发，不打断点就没法命中 Anthropic 的 prompt cache。
fn inject_prompt_cache(body: &mut Value) {
    let existing = count_existing_cache_breakpoints(body);
    let mut budget = CACHE_BREAKPOINT_CAP.saturating_sub(existing);
    if budget == 0 {
        return;
    }

    if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut)
        && let Some(last) = tools.last_mut()
        && last.get("cache_control").is_none()
        && let Some(o) = last.as_object_mut()
    {
        o.insert("cache_control".into(), make_cache_control());
        budget -= 1;
    }

    if budget > 0 {
        if let Some(text) = body
            .get("system")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            body["system"] = json!([{ "type": "text", "text": text }]);
        }
        if let Some(system) = body.get_mut("system").and_then(Value::as_array_mut)
            && let Some(last) = system.last_mut()
            && last.get("cache_control").is_none()
            && let Some(o) = last.as_object_mut()
        {
            o.insert("cache_control".into(), make_cache_control());
            budget -= 1;
        }
    }

    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };

    // (c) 最新一条可打断点的消息：工具循环通常以 user/tool_result 结束，只标 assistant
    // 会让最新稳定前缀无法命中缓存。
    if budget > 0 {
        for message in messages.iter_mut().rev() {
            if inject_message_breakpoint(message) {
                budget -= 1;
                break;
            }
        }
    }

    // (d) 次新 user 回合兜底：长工具历史的稳定前缀可能落在 Anthropic 20 块回看窗口之外。
    if budget > 0 && messages.len() >= 4 {
        let mut user_count = 0;
        for message in messages.iter_mut().rev() {
            if message.get("role").and_then(Value::as_str) != Some("user") {
                continue;
            }
            user_count += 1;
            if user_count == 2 {
                inject_message_breakpoint(message);
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 响应：Anthropic → Responses（非流式）
// ---------------------------------------------------------------------------

fn response_id(id: Option<&str>) -> String {
    let id = id.filter(|i| has_text(i)).unwrap_or("mcpane");
    if id.starts_with("resp_") {
        id.to_owned()
    } else {
        format!("resp_{id}")
    }
}

fn status_from_stop_reason(stop_reason: Option<&str>) -> (&'static str, Option<&'static str>) {
    match stop_reason {
        Some("max_tokens") => ("incomplete", Some("max_output_tokens")),
        // 安全拒答：报 incomplete，避免 Codex 把它当成正常完成的空回复。
        Some("refusal") => ("incomplete", Some("content_filter")),
        Some("model_context_window_exceeded") => ("incomplete", Some("max_output_tokens")),
        // pause_turn 只在声明了 Anthropic 服务端工具时出现，本路径的 Codex 请求不会触发；
        // 真出现就按完成处理并记日志，避免误判成截断。
        Some("pause_turn") => {
            tracing::warn!("本地路由：Anthropic stop_reason=pause_turn 超出预期，按完成处理");
            ("completed", None)
        }
        _ => ("completed", None),
    }
}

fn message_item(item_id: &str, text: &str) -> Value {
    json!({
        "id": item_id, "type": "message", "status": "completed", "role": "assistant",
        "content": [{ "type": "output_text", "text": text, "annotations": [] }]
    })
}

/// Anthropic thinking/redacted_thinking block → Responses reasoning item；
/// 签名信封只在能安全回放时附上，缺签名仍展示摘要文本但不可回放。
fn reasoning_item(item_id: &str, block: &Value) -> Value {
    let text = block.get("thinking").and_then(Value::as_str).unwrap_or("");
    let summary = if has_text(text) {
        json!([{ "type": "summary_text", "text": text }])
    } else {
        json!([])
    };
    let mut item = json!({ "id": item_id, "type": "reasoning", "summary": summary });
    if let Some(encrypted) = encode_thinking_block(block) {
        item["encrypted_content"] = json!(encrypted);
    }
    item
}

/// Anthropic `input_tokens` 不含缓存命中，Responses `input_tokens` 含缓存命中：
/// input = fresh + cache_read + cache_creation；cached_tokens = cache_read。
fn usage_from_anthropic(usage: Option<&Value>) -> Value {
    let usage = usage.filter(|u| u.is_object());
    let get = |key: &str| {
        usage
            .and_then(|u| u.get(key))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let fresh = get("input_tokens");
    let output = get("output_tokens");
    let cache_read = get("cache_read_input_tokens");
    let cache_creation = get("cache_creation_input_tokens");
    let input = fresh
        .saturating_add(cache_read)
        .saturating_add(cache_creation);
    json!({
        "input_tokens": input,
        "input_tokens_details": { "cached_tokens": cache_read },
        "output_tokens": output,
        "output_tokens_details": { "reasoning_tokens": 0 },
        "total_tokens": input.saturating_add(output),
    })
}

/// Anthropic 非流式响应 → Responses 响应对象。
pub(crate) fn response_from_anthropic(body: &Value, ctx: &ToolContext) -> Result<Value, String> {
    if body.get("type").and_then(Value::as_str) == Some("error") || body.get("error").is_some() {
        let error = body.get("error").unwrap_or(body);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| error.as_str())
            .unwrap_or("Anthropic 上游返回错误");
        return Err(message.to_owned());
    }

    let id = response_id(body.get("id").and_then(Value::as_str));
    let mut output = Vec::new();
    let mut text_parts: Vec<Value> = Vec::new();
    macro_rules! flush_text {
        () => {
            if !text_parts.is_empty() {
                let idx = output.len();
                output.push(json!({
                    "id": format!("{id}_msg_{idx}"), "type": "message", "status": "completed",
                    "role": "assistant", "content": std::mem::take(&mut text_parts),
                }));
            }
        };
    }

    for block in body
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match block.get("type").and_then(Value::as_str).unwrap_or("") {
            "text" => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    text_parts
                        .push(json!({ "type": "output_text", "text": text, "annotations": [] }));
                }
            }
            "tool_use" => {
                flush_text!();
                let call_id = block.get("id").and_then(Value::as_str).unwrap_or("");
                let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                let arguments = block
                    .get("input")
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "{}".into());
                output.push(ctx.response_item("completed", call_id, name, &arguments));
            }
            "thinking" | "redacted_thinking" => {
                flush_text!();
                let idx = output.len();
                output.push(reasoning_item(&format!("rs_{id}_{idx}"), block));
            }
            _ => {}
        }
    }
    flush_text!();

    let (status, incomplete_reason) =
        status_from_stop_reason(body.get("stop_reason").and_then(Value::as_str));
    let mut response = json!({
        "id": id, "object": "response", "created_at": 0, "status": status,
        "model": body.get("model").cloned().unwrap_or(Value::Null),
        "output": output,
        "usage": usage_from_anthropic(body.get("usage")),
    });
    if let Some(reason) = incomplete_reason {
        response["incomplete_details"] = json!({ "reason": reason });
    }
    Ok(response)
}

// ---------------------------------------------------------------------------
// 响应：Anthropic SSE → Responses SSE（流式）
// ---------------------------------------------------------------------------

fn sse(event: &str, data: Value) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {data}\n\n"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    Thinking,
    Tool,
}

struct StreamBlock {
    kind: BlockKind,
    output_index: u32,
    item_id: String,
    call_id: String,
    name: String,
    /// 文本/思考累积文本，或工具 input_json_delta 累积的 JSON 片段。
    accum: String,
    signature: String,
    redacted_data: String,
    is_redacted: bool,
    /// 少数网关把完整 input 放在 content_block_start 而不发 delta，兜底用。
    start_input: String,
    has_summary: bool,
    done: bool,
}

/// Anthropic chunk 序列 → Responses 事件序列的状态机；按 index 分块，
/// 事件形态与 codex_chat::StreamConverter 保持一致（Codex 客户端只认这套事件）。
pub(crate) struct StreamConverter {
    ctx: ToolContext,
    started: bool,
    completed: bool,
    response_id: String,
    model: String,
    next_index: u32,
    blocks: BTreeMap<u64, StreamBlock>,
    output: Vec<(u32, Value)>,
    usage: Map<String, Value>,
    stop_reason: Option<String>,
    /// 上游流在 message_stop 前断开（看门中止/断连）；已开始但没跑完的工具调用
    /// 不能被标 completed 并发 arguments.done，否则 Codex 会拿半截参数去执行。
    stream_truncated: bool,
}

impl StreamConverter {
    pub(crate) fn new(ctx: ToolContext) -> Self {
        Self {
            ctx,
            started: false,
            completed: false,
            response_id: "resp_mcpane".into(),
            model: String::new(),
            next_index: 0,
            blocks: BTreeMap::new(),
            output: Vec::new(),
            usage: Map::new(),
            stop_reason: None,
            stream_truncated: false,
        }
    }

    fn alloc_index(&mut self) -> u32 {
        let index = self.next_index;
        self.next_index += 1;
        index
    }

    fn base_response(&self, status: &str) -> Value {
        let mut output = self.output.clone();
        output.sort_by_key(|(index, _)| *index);
        json!({
            "id": self.response_id, "object": "response", "created_at": 0, "status": status,
            "model": self.model,
            "output": output.into_iter().map(|(_, item)| item).collect::<Vec<_>>(),
            "usage": usage_from_anthropic(Some(&Value::Object(self.usage.clone()))),
        })
    }

    fn ensure_started(&mut self) -> Vec<Bytes> {
        if self.started {
            return Vec::new();
        }
        self.started = true;
        let response = self.base_response("in_progress");
        vec![
            sse(
                "response.created",
                json!({ "type": "response.created", "response": response }),
            ),
            sse(
                "response.in_progress",
                json!({ "type": "response.in_progress", "response": response }),
            ),
        ]
    }

    fn merge_usage(&mut self, usage: &Value) {
        if let Some(obj) = usage.as_object() {
            for (key, value) in obj {
                if !value.is_null() {
                    self.usage.insert(key.clone(), value.clone());
                }
            }
        }
    }

    pub(crate) fn handle_chunk(&mut self, chunk: &Value) -> Vec<Bytes> {
        match chunk.get("type").and_then(Value::as_str).unwrap_or("") {
            "message_start" => self.on_message_start(chunk),
            "content_block_start" => self.on_block_start(chunk),
            "content_block_delta" => self.on_block_delta(chunk),
            "content_block_stop" => self.on_block_stop(chunk),
            "message_delta" => self.on_message_delta(chunk),
            "message_stop" => self.finish(),
            _ => Vec::new(),
        }
    }

    fn on_message_start(&mut self, chunk: &Value) -> Vec<Bytes> {
        if let Some(message) = chunk.get("message") {
            if let Some(id) = message.get("id").and_then(Value::as_str) {
                self.response_id = response_id(Some(id));
            }
            if let Some(model) = message
                .get("model")
                .and_then(Value::as_str)
                .filter(|m| has_text(m))
            {
                model.clone_into(&mut self.model);
            }
            if let Some(usage) = message.get("usage") {
                self.merge_usage(usage);
            }
        }
        self.ensure_started()
    }

    fn on_block_start(&mut self, chunk: &Value) -> Vec<Bytes> {
        let mut events = self.ensure_started();
        let Some(index) = chunk.get("index").and_then(Value::as_u64) else {
            return events;
        };
        let block = chunk.get("content_block").cloned().unwrap_or(Value::Null);
        match block.get("type").and_then(Value::as_str).unwrap_or("") {
            "text" => {
                let output_index = self.alloc_index();
                let item_id = format!("{}_msg_{output_index}", self.response_id);
                events.push(sse(
                    "response.output_item.added",
                    json!({ "type": "response.output_item.added", "output_index": output_index,
                            "item": { "id": item_id, "type": "message", "status": "in_progress",
                                      "role": "assistant", "content": [] } }),
                ));
                events.push(sse(
                    "response.content_part.added",
                    json!({ "type": "response.content_part.added", "item_id": item_id,
                            "output_index": output_index, "content_index": 0,
                            "part": { "type": "output_text", "text": "", "annotations": [] } }),
                ));
                self.blocks.insert(
                    index,
                    StreamBlock {
                        kind: BlockKind::Text,
                        output_index,
                        item_id,
                        call_id: String::new(),
                        name: String::new(),
                        accum: block
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        signature: String::new(),
                        redacted_data: String::new(),
                        is_redacted: false,
                        start_input: String::new(),
                        has_summary: false,
                        done: false,
                    },
                );
            }
            "tool_use" => {
                let output_index = self.alloc_index();
                let call_id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let start_input = block
                    .get("input")
                    .filter(|v| v.as_object().is_some_and(|o| !o.is_empty()))
                    .map(ToString::to_string)
                    .unwrap_or_default();
                let item_id = self.ctx.item_id(&call_id, &name);
                let item = self.ctx.response_item("in_progress", &call_id, &name, "");
                events.push(sse(
                    "response.output_item.added",
                    json!({ "type": "response.output_item.added", "output_index": output_index, "item": item }),
                ));
                self.blocks.insert(
                    index,
                    StreamBlock {
                        kind: BlockKind::Tool,
                        output_index,
                        item_id,
                        call_id,
                        name,
                        accum: String::new(),
                        signature: String::new(),
                        redacted_data: String::new(),
                        is_redacted: false,
                        start_input,
                        has_summary: false,
                        done: false,
                    },
                );
            }
            "thinking" | "redacted_thinking" => {
                let is_redacted =
                    block.get("type").and_then(Value::as_str) == Some("redacted_thinking");
                let output_index = self.alloc_index();
                let item_id = format!("rs_{}_{output_index}", self.response_id);
                events.push(sse(
                    "response.output_item.added",
                    json!({ "type": "response.output_item.added", "output_index": output_index,
                            "item": { "id": item_id, "type": "reasoning", "status": "in_progress", "summary": [] } }),
                ));
                if !is_redacted {
                    events.push(sse(
                        "response.reasoning_summary_part.added",
                        json!({ "type": "response.reasoning_summary_part.added", "item_id": item_id,
                                "output_index": output_index, "summary_index": 0,
                                "part": { "type": "summary_text", "text": "" } }),
                    ));
                }
                self.blocks.insert(
                    index,
                    StreamBlock {
                        kind: BlockKind::Thinking,
                        output_index,
                        item_id,
                        call_id: String::new(),
                        name: String::new(),
                        accum: block
                            .get("thinking")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        signature: block
                            .get("signature")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        redacted_data: block
                            .get("data")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        is_redacted,
                        start_input: String::new(),
                        has_summary: !is_redacted,
                        done: false,
                    },
                );
            }
            _ => {}
        }
        events
    }

    fn on_block_delta(&mut self, chunk: &Value) -> Vec<Bytes> {
        let Some(index) = chunk.get("index").and_then(Value::as_u64) else {
            return Vec::new();
        };
        let delta = chunk.get("delta").cloned().unwrap_or(Value::Null);
        let delta_type = delta.get("type").and_then(Value::as_str).unwrap_or("");
        let Some(block) = self.blocks.get_mut(&index) else {
            return Vec::new();
        };
        match delta_type {
            "text_delta" => {
                let text = delta.get("text").and_then(Value::as_str).unwrap_or("");
                block.accum.push_str(text);
                vec![sse(
                    "response.output_text.delta",
                    json!({ "type": "response.output_text.delta", "item_id": block.item_id,
                            "output_index": block.output_index, "content_index": 0, "delta": text }),
                )]
            }
            "thinking_delta" => {
                let text = delta.get("thinking").and_then(Value::as_str).unwrap_or("");
                block.accum.push_str(text);
                vec![sse(
                    "response.reasoning_summary_text.delta",
                    json!({ "type": "response.reasoning_summary_text.delta", "item_id": block.item_id,
                            "output_index": block.output_index, "summary_index": 0, "delta": text }),
                )]
            }
            "signature_delta" => {
                if let Some(sig) = delta.get("signature").and_then(Value::as_str) {
                    sig.clone_into(&mut block.signature);
                }
                Vec::new()
            }
            "input_json_delta" => {
                let partial = delta
                    .get("partial_json")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                block.accum.push_str(&partial);
                let (item_id, output_index, name) = (
                    block.item_id.clone(),
                    block.output_index,
                    block.name.clone(),
                );
                if partial.is_empty() || self.ctx.is_custom(&name) {
                    return Vec::new();
                }
                vec![sse(
                    "response.function_call_arguments.delta",
                    json!({ "type": "response.function_call_arguments.delta", "item_id": item_id,
                            "output_index": output_index, "delta": partial }),
                )]
            }
            _ => Vec::new(),
        }
    }

    fn on_block_stop(&mut self, chunk: &Value) -> Vec<Bytes> {
        let Some(index) = chunk.get("index").and_then(Value::as_u64) else {
            return Vec::new();
        };
        self.close_block(index)
    }

    fn close_block(&mut self, index: u64) -> Vec<Bytes> {
        let Some(block) = self.blocks.get(&index) else {
            return Vec::new();
        };
        if block.done {
            return Vec::new();
        }
        let kind = block.kind;
        let output_index = block.output_index;
        let item_id = block.item_id.clone();
        let accum = block.accum.clone();
        let call_id = block.call_id.clone();
        let name = block.name.clone();
        let signature = block.signature.clone();
        let redacted_data = block.redacted_data.clone();
        let is_redacted = block.is_redacted;
        let start_input = block.start_input.clone();
        let has_summary = block.has_summary;
        if let Some(b) = self.blocks.get_mut(&index) {
            b.done = true;
        }

        match kind {
            BlockKind::Text => {
                let item = message_item(&item_id, &accum);
                self.output.push((output_index, item.clone()));
                vec![
                    sse(
                        "response.output_text.done",
                        json!({ "type": "response.output_text.done", "item_id": item_id,
                                "output_index": output_index, "content_index": 0, "text": accum }),
                    ),
                    sse(
                        "response.content_part.done",
                        json!({ "type": "response.content_part.done", "item_id": item_id,
                                "output_index": output_index, "content_index": 0,
                                "part": { "type": "output_text", "text": accum, "annotations": [] } }),
                    ),
                    sse(
                        "response.output_item.done",
                        json!({ "type": "response.output_item.done", "output_index": output_index, "item": item }),
                    ),
                ]
            }
            BlockKind::Tool => {
                let raw_input = if has_text(&accum) { accum } else { start_input };
                let arguments = if has_text(&raw_input) {
                    raw_input
                } else {
                    "{}".to_owned()
                };
                // 流被截断时不能标 completed：半截参数一旦被当作 arguments.done 发出，
                // Codex 会直接拿去执行。
                let status = if self.stream_truncated {
                    "incomplete"
                } else {
                    "completed"
                };
                let item = self.ctx.response_item(status, &call_id, &name, &arguments);
                let mut events = Vec::new();
                if !self.stream_truncated {
                    if self.ctx.is_custom(&name) {
                        let input = custom_input_from_arguments(&arguments);
                        events.push(sse(
                            "response.custom_tool_call_input.done",
                            json!({ "type": "response.custom_tool_call_input.done", "item_id": item_id,
                                    "output_index": output_index, "input": input }),
                        ));
                    } else {
                        events.push(sse(
                            "response.function_call_arguments.done",
                            json!({ "type": "response.function_call_arguments.done", "item_id": item_id,
                                    "output_index": output_index, "arguments": arguments }),
                        ));
                    }
                }
                events.push(sse(
                    "response.output_item.done",
                    json!({ "type": "response.output_item.done", "output_index": output_index, "item": item }),
                ));
                self.output.push((output_index, item));
                events
            }
            BlockKind::Thinking => {
                let block_value = if is_redacted {
                    json!({ "type": "redacted_thinking", "data": redacted_data })
                } else {
                    let mut v = json!({ "type": "thinking", "thinking": accum });
                    if has_text(&signature) {
                        v["signature"] = json!(signature);
                    }
                    v
                };
                let item = reasoning_item(&item_id, &block_value);
                self.output.push((output_index, item.clone()));
                let mut events = Vec::new();
                if has_summary {
                    events.push(sse(
                        "response.reasoning_summary_text.done",
                        json!({ "type": "response.reasoning_summary_text.done", "item_id": item_id,
                                "output_index": output_index, "summary_index": 0, "text": accum }),
                    ));
                    events.push(sse(
                        "response.reasoning_summary_part.done",
                        json!({ "type": "response.reasoning_summary_part.done", "item_id": item_id,
                                "output_index": output_index, "summary_index": 0,
                                "part": { "type": "summary_text", "text": accum } }),
                    ));
                }
                events.push(sse(
                    "response.output_item.done",
                    json!({ "type": "response.output_item.done", "output_index": output_index, "item": item }),
                ));
                events
            }
        }
    }

    fn on_message_delta(&mut self, chunk: &Value) -> Vec<Bytes> {
        if let Some(reason) = chunk.pointer("/delta/stop_reason").and_then(Value::as_str) {
            self.stop_reason = Some(reason.to_owned());
        }
        if let Some(usage) = chunk.get("usage") {
            self.merge_usage(usage);
        }
        Vec::new()
    }

    pub(crate) fn finish(&mut self) -> Vec<Bytes> {
        if self.completed {
            return Vec::new();
        }
        let mut events = self.ensure_started();
        let keys: Vec<u64> = self.blocks.keys().copied().collect();
        for key in keys {
            events.extend(self.close_block(key));
        }
        let (status, incomplete_reason) = status_from_stop_reason(self.stop_reason.as_deref());
        let mut response = self.base_response(status);
        if let Some(reason) = incomplete_reason {
            response["incomplete_details"] = json!({ "reason": reason });
        }
        events.push(sse(
            "response.completed",
            json!({ "type": "response.completed", "response": response }),
        ));
        self.completed = true;
        events
    }

    pub(crate) fn fail(&mut self, message: &str) -> Bytes {
        self.completed = true;
        let mut response = self.base_response("failed");
        response["error"] = json!({ "message": message, "type": "upstream_error" });
        sse(
            "response.failed",
            json!({ "type": "response.failed", "response": response }),
        )
    }

    fn has_output(&self) -> bool {
        !self.output.is_empty()
            || self
                .blocks
                .values()
                .any(|b| has_text(&b.accum) || !b.call_id.is_empty() || !b.name.is_empty())
    }
}

fn anthropic_error_message(chunk: &Value) -> String {
    let error = chunk.get("error").unwrap_or(chunk);
    error
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| error.as_str().map(str::to_owned))
        .unwrap_or_else(|| "Anthropic 上游返回错误".to_owned())
}

/// 处理一块上游字节；返回待发事件与流是否已终结（message_stop / error）。
fn on_bytes(
    conv: &mut StreamConverter,
    buffer: &mut String,
    remainder: &mut Vec<u8>,
    bytes: &[u8],
) -> (Vec<Bytes>, bool) {
    crate::sse::append_utf8(buffer, remainder, bytes);
    let mut events = Vec::new();
    while let Some(data) = crate::sse::take_data(buffer) {
        let Ok(chunk) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        let msg_type = chunk.get("type").and_then(Value::as_str).unwrap_or("");
        if msg_type == "error" {
            events.push(conv.fail(&anthropic_error_message(&chunk)));
            return (events, true);
        }
        events.extend(conv.handle_chunk(&chunk));
        if msg_type == "message_stop" {
            return (events, true);
        }
    }
    (events, false)
}

/// 上游结束但没有 message_stop：有 stop_reason 或已有产出按（截断）收尾，否则报失败。
fn on_end(conv: &mut StreamConverter) -> Vec<Bytes> {
    if conv.completed {
        Vec::new()
    } else if conv.stop_reason.is_some() {
        conv.finish()
    } else if conv.has_output() {
        conv.stop_reason = Some("max_tokens".into());
        conv.stream_truncated = true;
        conv.finish()
    } else {
        vec![conv.fail("上游 Anthropic 流在 message_stop 之前结束")]
    }
}

struct StreamState<S> {
    upstream: S,
    conv: StreamConverter,
    buffer: String,
    remainder: Vec<u8>,
    ended: bool,
}

/// Anthropic Messages SSE 字节流 → Responses SSE 字节流。
pub(crate) fn convert_stream<S, E>(
    upstream: S,
    ctx: ToolContext,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send
where
    S: Stream<Item = Result<Bytes, E>> + Send + Unpin + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let state = StreamState {
        upstream,
        conv: StreamConverter::new(ctx),
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
                let (events, done) =
                    on_bytes(&mut st.conv, &mut st.buffer, &mut st.remainder, &bytes);
                st.ended = done;
                events
            }
            Some(Err(error)) => {
                st.ended = true;
                vec![st.conv.fail(&format!("上游流中断：{error}"))]
            }
            None => {
                st.ended = true;
                on_end(&mut st.conv)
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

    /// 覆盖 function/custom/namespace 三种工具声明，供多个测试复用。
    fn sample_request() -> Value {
        json!({
            "model": "claude-x",
            "instructions": "You are Codex.",
            "tools": [
                { "type": "function", "name": "shell", "description": "run",
                  "parameters": { "type": "object", "properties": { "cmd": { "type": "string" } } } },
                { "type": "custom", "name": "apply_patch", "format": { "type": "grammar" } },
                { "type": "namespace", "name": "mcp__fs", "tools": [
                    { "type": "function", "name": "read", "parameters": null }
                ] }
            ],
            "input": [
                { "type": "message", "role": "developer", "content": [{ "type": "input_text", "text": "Be terse." }] },
                { "type": "message", "role": "user", "content": [
                    { "type": "input_text", "text": "list files" },
                    { "type": "input_image", "image_url": "data:image/png;base64,AAAA" }
                ] },
                { "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "Running ls." }] },
                { "type": "function_call", "call_id": "c1", "name": "shell", "arguments": "{\"cmd\":\"ls\"}" },
                { "type": "function_call_output", "call_id": "c1", "output": "a.txt" },
                { "type": "function_call", "call_id": "c2", "name": "read", "namespace": "mcp__fs", "arguments": "{}" },
                { "type": "function_call_output", "call_id": "c2", "output": [{ "type": "input_text", "text": "hello" }] },
                { "type": "custom_tool_call", "call_id": "c3", "name": "apply_patch", "input": "*** Begin Patch" },
                { "type": "custom_tool_call_output", "call_id": "c3", "output": "applied" }
            ]
        })
    }

    #[test]
    fn request_converts_system_and_maintains_alternation_with_tool_grouping() {
        let (anth, _ctx) = request_to_anthropic(&sample_request());
        // system 会被 prompt cache 注入转成数组（见下方 cache 测试），这里只核对文本本身。
        assert_eq!(anth["system"][0]["text"], "You are Codex.\n\nBe terse.");
        let messages = anth["messages"].as_array().unwrap();

        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["text"], "list files");
        assert_eq!(messages[0]["content"][1]["type"], "image");
        assert_eq!(messages[0]["content"][1]["source"]["data"], "AAAA");

        // 文本消息与随后的 function_call 同属一个模型回合，合并进同一条 assistant。
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"][0]["text"], "Running ls.");
        assert_eq!(messages[1]["content"][1]["type"], "tool_use");
        assert_eq!(messages[1]["content"][1]["name"], "shell");
        assert_eq!(messages[1]["content"][1]["input"]["cmd"], "ls");

        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "c1");
        assert_eq!(messages[2]["content"][0]["content"], "a.txt");

        // namespace 工具拍平命名并在独立回合出现。
        assert_eq!(messages[3]["role"], "assistant");
        assert_eq!(messages[3]["content"][0]["name"], "mcp__fs__read");
        assert_eq!(messages[4]["content"][0]["tool_use_id"], "c2");
        assert_eq!(messages[4]["content"][0]["content"][0]["text"], "hello");

        // 自由格式工具：input 包成 {"input": <string>}。
        assert_eq!(messages[5]["content"][0]["name"], "apply_patch");
        assert_eq!(
            messages[5]["content"][0]["input"]["input"],
            "*** Begin Patch"
        );
        assert_eq!(messages[6]["content"][0]["tool_use_id"], "c3");
        assert_eq!(messages[6]["content"][0]["content"], "applied");
    }

    #[test]
    fn request_images_support_data_url_and_remote_url() {
        let body = json!({
            "model": "claude-x",
            "input": [
                { "type": "message", "role": "user", "content": [
                    { "type": "input_image", "image_url": "data:image/png;base64,QUJD" },
                    { "type": "input_image", "image_url": "https://example.com/a.png" }
                ] }
            ]
        });
        let (anth, _) = request_to_anthropic(&body);
        let content = anth["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content[0]["source"]["type"], "base64");
        assert_eq!(content[0]["source"]["media_type"], "image/png");
        assert_eq!(content[0]["source"]["data"], "QUJD");
        assert_eq!(
            content[1]["source"],
            json!({ "type": "url", "url": "https://example.com/a.png" })
        );
    }

    #[test]
    fn request_reasoning_signature_round_trips_and_drops_foreign() {
        let ours = json!({ "type": "thinking", "thinking": "plan it", "signature": "sig-abc" });
        let encoded = encode_thinking_block(&ours).unwrap();
        let mut body = sample_request();
        let input = body["input"].as_array_mut().unwrap();
        input.insert(
            2,
            json!({ "type": "reasoning", "id": "rs_1", "encrypted_content": encoded }),
        );
        input.insert(
            3,
            json!({ "type": "reasoning", "id": "rs_foreign", "encrypted_content": "other-vendor-v1:whatever" }),
        );
        let (anth, _) = request_to_anthropic(&body);
        let messages = anth["messages"].as_array().unwrap();
        assert_eq!(messages[1]["content"][0], ours);
        assert_eq!(messages[1]["content"][1]["type"], "text");
        assert_eq!(messages[1]["content"][1]["text"], "Running ls.");
    }

    #[test]
    fn request_thinking_budget_from_effort_raises_max_tokens_and_drops_sampling_params() {
        let mut body = sample_request();
        body["reasoning"] = json!({ "effort": "xhigh" });
        body["temperature"] = json!(0.7);
        body["top_p"] = json!(0.9);
        let (anth, _) = request_to_anthropic(&body);
        assert_eq!(
            anth["thinking"],
            json!({ "type": "enabled", "budget_tokens": 32000 })
        );
        // 默认 max_tokens(32000) <= 预算(32000)，需抬高到 预算 + 8192。
        assert_eq!(anth["max_tokens"], 40192);
        assert!(anth.get("temperature").is_none());
        assert!(anth.get("top_p").is_none());
    }

    #[test]
    fn request_effort_none_disables_thinking_and_keeps_sampling_params() {
        let mut body = sample_request();
        body["reasoning"] = json!({ "effort": "none" });
        body["temperature"] = json!(0.5);
        let (anth, _) = request_to_anthropic(&body);
        assert!(anth.get("thinking").is_none());
        assert_eq!(anth["temperature"], 0.5);
        assert_eq!(anth["max_tokens"], 32000);
    }

    #[test]
    fn request_max_output_tokens_passes_through_when_above_budget() {
        let mut body = sample_request();
        body["max_output_tokens"] = json!(50000);
        body["reasoning"] = json!({ "effort": "low" });
        let (anth, _) = request_to_anthropic(&body);
        assert_eq!(anth["max_tokens"], 50000);
        assert_eq!(anth["thinking"]["budget_tokens"], 4096);
    }

    #[test]
    fn request_explicit_max_output_tokens_clamps_thinking_instead_of_raising_max_tokens() {
        let mut body = sample_request();
        body["max_output_tokens"] = json!(6000);
        body["reasoning"] = json!({ "effort": "xhigh" }); // 预算 32000，钳到上限一半 3000。
        let (anth, _) = request_to_anthropic(&body);
        assert_eq!(anth["max_tokens"], 6000);
        assert_eq!(
            anth["thinking"],
            json!({ "type": "enabled", "budget_tokens": 3000 })
        );
    }

    #[test]
    fn request_explicit_max_output_tokens_below_thinking_floor_disables_thinking() {
        let mut body = sample_request();
        body["max_output_tokens"] = json!(1500); // 一半 750 < 1024 下限。
        body["reasoning"] = json!({ "effort": "low" });
        body["temperature"] = json!(0.3);
        let (anth, _) = request_to_anthropic(&body);
        assert_eq!(anth["max_tokens"], 1500);
        assert!(anth.get("thinking").is_none());
        assert_eq!(anth["temperature"], 0.3);
    }

    #[test]
    fn request_unanswered_trailing_tool_use_is_dropped() {
        let input = json!({
            "model": "c",
            "max_output_tokens": 100,
            "input": [
                { "role": "user", "content": "run it" },
                { "type": "function_call", "call_id": "c1", "name": "t", "arguments": "{}" }
            ]
        });
        let (anth, _) = request_to_anthropic(&input);
        let messages = anth["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["text"], "run it");
    }

    #[test]
    fn request_partial_parallel_tool_turn_is_dropped_as_a_unit() {
        let input = json!({
            "model": "c",
            "max_output_tokens": 100,
            "input": [
                { "role": "user", "content": "run both" },
                { "type": "function_call", "call_id": "c1", "name": "t", "arguments": "{}" },
                { "type": "function_call", "call_id": "c2", "name": "t", "arguments": "{}" },
                { "type": "function_call_output", "call_id": "c1", "output": "one" },
                { "role": "user", "content": [{ "type": "input_text", "text": "continue" }] }
            ]
        });
        let (anth, _) = request_to_anthropic(&input);
        let messages = anth["messages"].as_array().unwrap();
        assert!(
            messages
                .iter()
                .all(|m| m["content"].as_array().unwrap().iter().all(|b| {
                    !matches!(
                        b.get("type").and_then(Value::as_str),
                        Some("tool_use" | "tool_result")
                    )
                }))
        );
        assert_eq!(messages.last().unwrap()["content"][0]["text"], "continue");
    }

    #[test]
    fn request_orphan_tool_result_dropped() {
        // function_call 已被压缩掉、只剩孤儿 function_call_output：必须整体丢弃，否则 400。
        let input = json!({
            "model": "c",
            "max_output_tokens": 100,
            "input": [
                { "type": "function_call_output", "call_id": "ghost", "output": "X" },
                { "role": "user", "content": [{ "type": "input_text", "text": "hello" }] }
            ]
        });
        let (anth, _) = request_to_anthropic(&input);
        let messages = anth["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "hello");
    }

    #[test]
    fn request_paired_tool_result_kept() {
        let input = json!({
            "model": "c",
            "max_output_tokens": 100,
            "input": [
                { "type": "function_call", "call_id": "c1", "name": "t", "arguments": "{}" },
                { "type": "function_call_output", "call_id": "c1", "output": "ok" }
            ]
        });
        let (anth, _) = request_to_anthropic(&input);
        let last = anth["messages"].as_array().unwrap().last().unwrap();
        assert_eq!(last["role"], "user");
        assert_eq!(last["content"][0]["type"], "tool_result");
        assert_eq!(last["content"][0]["tool_use_id"], "c1");
    }

    #[test]
    fn request_incomplete_status_tool_call_and_its_output_are_dropped() {
        let input = json!({
            "model": "c",
            "input": [
                { "role": "user", "content": [{ "type": "input_text", "text": "run it" }] },
                { "type": "function_call", "call_id": "c1", "name": "exec",
                  "arguments": "{\"cmd\":", "status": "incomplete" },
                { "type": "function_call_output", "call_id": "c1", "output": "never ran" }
            ]
        });
        let (anth, _) = request_to_anthropic(&input);
        let serialized = anth["messages"].to_string();
        assert!(!serialized.contains("tool_use"));
        assert!(!serialized.contains("tool_result"));
        assert!(serialized.contains("run it"));
    }

    #[test]
    fn request_trailing_whitespace_only_assistant_text_is_trimmed() {
        let input = json!({
            "model": "c",
            "input": [
                { "role": "user", "content": [{ "type": "input_text", "text": "hi" }] },
                { "role": "assistant", "content": [{ "type": "output_text", "text": "answer  " }] }
            ]
        });
        let (anth, _) = request_to_anthropic(&input);
        let last = anth["messages"].as_array().unwrap().last().unwrap();
        assert_eq!(
            last["content"].as_array().unwrap().last().unwrap()["text"],
            "answer"
        );
    }

    #[test]
    fn status_from_stop_reason_maps_refusal_and_context_overflow_to_incomplete() {
        assert_eq!(
            status_from_stop_reason(Some("refusal")),
            ("incomplete", Some("content_filter"))
        );
        assert_eq!(
            status_from_stop_reason(Some("model_context_window_exceeded")),
            ("incomplete", Some("max_output_tokens"))
        );
        assert_eq!(
            status_from_stop_reason(Some("pause_turn")),
            ("completed", None)
        );
        assert_eq!(
            status_from_stop_reason(Some("end_turn")),
            ("completed", None)
        );
    }

    #[test]
    fn prompt_cache_injects_tools_system_and_latest_message_breakpoints() {
        let mut body = json!({
            "model": "claude-x",
            "tools": [{ "name": "t1" }, { "name": "t2" }],
            "system": "sys prompt",
            "messages": [
                { "role": "user", "content": [{ "type": "text", "text": "hi" }] },
                { "role": "assistant", "content": [{ "type": "text", "text": "hello" }] }
            ]
        });
        inject_prompt_cache(&mut body);

        assert!(body["tools"][0].get("cache_control").is_none());
        assert_eq!(body["tools"][1]["cache_control"]["type"], "ephemeral");
        assert!(body["tools"][1]["cache_control"].get("ttl").is_none());

        // 字符串 system 转数组后打断点，默认 TTL（5m）不写 ttl 字段。
        assert_eq!(body["system"][0]["text"], "sys prompt");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");

        assert!(
            body["messages"][1]["content"][0]
                .get("cache_control")
                .is_some()
        );
    }

    #[test]
    fn prompt_cache_existing_breakpoints_count_against_budget_and_cap_at_four() {
        let mut body = json!({
            "model": "claude-x",
            "tools": [
                { "name": "t1", "cache_control": { "type": "ephemeral" } },
                { "name": "t2", "cache_control": { "type": "ephemeral" } }
            ],
            "system": [{ "type": "text", "text": "sys", "cache_control": { "type": "ephemeral" } }],
            "messages": [{
                "role": "user",
                "content": [{ "type": "text", "text": "hi", "cache_control": { "type": "ephemeral" } }]
            }]
        });
        // 已有 4 个断点，预算耗尽，不应再新增。
        inject_prompt_cache(&mut body);
        assert_eq!(count_existing_cache_breakpoints(&body), 4);
    }

    #[test]
    fn prompt_cache_skips_thinking_blocks_when_picking_latest_breakpoint() {
        let mut body = json!({
            "model": "claude-x",
            "messages": [{
                "role": "assistant",
                "content": [
                    { "type": "text", "text": "answer" },
                    { "type": "thinking", "thinking": "plan" }
                ]
            }]
        });
        inject_prompt_cache(&mut body);
        assert!(
            body["messages"][0]["content"][0]
                .get("cache_control")
                .is_some()
        );
        assert!(
            body["messages"][0]["content"][1]
                .get("cache_control")
                .is_none()
        );
    }

    #[test]
    fn non_stream_response_converts_thinking_text_tool_use_and_usage() {
        let (_, ctx) = request_to_anthropic(&sample_request());
        let body = json!({
            "id": "msg_1", "model": "claude-x",
            "content": [
                { "type": "thinking", "thinking": "analyzing", "signature": "sig1" },
                { "type": "text", "text": "Here you go." },
                { "type": "tool_use", "id": "call1", "name": "mcp__fs__read", "input": { "path": "a" } }
            ],
            "stop_reason": "tool_use",
            "usage": { "input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 4 }
        });
        let resp = response_from_anthropic(&body, &ctx).unwrap();
        assert_eq!(resp["id"], "resp_msg_1");
        assert_eq!(resp["status"], "completed");
        let output = resp["output"].as_array().unwrap();
        assert_eq!(output[0]["type"], "reasoning");
        assert_eq!(output[0]["summary"][0]["text"], "analyzing");
        assert!(
            output[0]["encrypted_content"]
                .as_str()
                .unwrap()
                .starts_with(REASONING_ENCRYPTED_PREFIX)
        );
        assert_eq!(output[1]["content"][0]["text"], "Here you go.");
        assert_eq!(output[2]["type"], "function_call");
        assert_eq!(output[2]["name"], "read");
        assert_eq!(output[2]["namespace"], "mcp__fs");
        assert_eq!(resp["usage"]["input_tokens"], 14);
        assert_eq!(resp["usage"]["input_tokens_details"]["cached_tokens"], 4);
        assert_eq!(resp["usage"]["total_tokens"], 19);

        let mut incomplete_body = body.clone();
        incomplete_body["stop_reason"] = json!("max_tokens");
        let resp2 = response_from_anthropic(&incomplete_body, &ctx).unwrap();
        assert_eq!(resp2["status"], "incomplete");
        assert_eq!(resp2["incomplete_details"]["reason"], "max_output_tokens");
    }

    fn events_of(bytes: &[Bytes]) -> Vec<Value> {
        bytes
            .iter()
            .flat_map(|b| {
                String::from_utf8(b.to_vec())
                    .unwrap()
                    .lines()
                    .filter_map(|l| l.strip_prefix("data: ").map(str::to_owned))
                    .collect::<Vec<_>>()
            })
            .map(|d| serde_json::from_str::<Value>(&d).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn stream_converts_text_thinking_tool_use_and_usage() {
        let (_, ctx) = request_to_anthropic(&sample_request());
        let chunks = [
            r#"{"type":"message_start","message":{"id":"msg_01","model":"claude-3-x","usage":{"input_tokens":3}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"深度思考中"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig-xyz"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hi there"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"call1","name":"shell","input":{}}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"cmd\""}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":":\"ls\"}"}}"#,
            r#"{"type":"content_block_stop","index":2}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}"#,
            r#"{"type":"message_stop"}"#,
        ];
        let mut raw = String::new();
        for chunk in chunks {
            raw.push_str(&format!("event: x\ndata: {chunk}\n\n"));
        }
        // 故意在多字节字符中间切开，验证 UTF-8 跨块拼接。
        let split_at = raw.find("深").unwrap() + 2;
        let bytes = raw.into_bytes();
        let (a, b) = bytes.split_at(split_at);
        let upstream = futures_util::stream::iter(vec![
            Ok::<_, std::io::Error>(Bytes::copy_from_slice(a)),
            Ok(Bytes::copy_from_slice(b)),
        ]);
        let out: Vec<Bytes> = convert_stream(upstream, ctx)
            .map(|r| r.unwrap())
            .collect()
            .await;
        let events = events_of(&out);
        let types: Vec<&str> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
        assert_eq!(types.first(), Some(&"response.created"));
        assert!(types.contains(&"response.reasoning_summary_text.delta"));
        assert!(types.contains(&"response.output_text.delta"));
        assert!(types.contains(&"response.function_call_arguments.delta"));
        let completed = events.last().unwrap();
        assert_eq!(completed["type"], "response.completed");
        let output = completed["response"]["output"].as_array().unwrap();
        assert_eq!(output[0]["type"], "reasoning");
        assert_eq!(output[0]["summary"][0]["text"], "深度思考中");
        assert_eq!(output[1]["content"][0]["text"], "Hi there");
        assert_eq!(output[2]["arguments"], "{\"cmd\":\"ls\"}");
        assert_eq!(output[2]["call_id"], "call1");
        assert_eq!(completed["response"]["usage"]["input_tokens"], 3);
        assert_eq!(completed["response"]["usage"]["output_tokens"], 7);
    }

    #[tokio::test]
    async fn stream_error_event_becomes_response_failed() {
        let raw = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"quota exceeded\"}}\n\n";
        let upstream = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(raw))]);
        let out: Vec<Bytes> = convert_stream(upstream, ToolContext::default())
            .map(|r| r.unwrap())
            .collect()
            .await;
        let events = events_of(&out);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "response.failed");
        assert_eq!(events[0]["response"]["error"]["message"], "quota exceeded");
    }

    #[tokio::test]
    async fn stream_without_message_stop_but_with_output_completes_as_incomplete() {
        let raw = "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
                   data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n";
        let upstream = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(raw))]);
        let out: Vec<Bytes> = convert_stream(upstream, ToolContext::default())
            .map(|r| r.unwrap())
            .collect()
            .await;
        let events = events_of(&out);
        let completed = events.last().unwrap();
        assert_eq!(completed["response"]["status"], "incomplete");
        assert_eq!(
            completed["response"]["output"][0]["content"][0]["text"],
            "partial"
        );

        let empty = futures_util::stream::iter(Vec::<Result<Bytes, std::io::Error>>::new());
        let out: Vec<Bytes> = convert_stream(empty, ToolContext::default())
            .map(|r| r.unwrap())
            .collect()
            .await;
        assert_eq!(events_of(&out).last().unwrap()["type"], "response.failed");
    }

    #[tokio::test]
    async fn stream_truncated_tool_call_is_not_marked_completed_or_executable() {
        // 看门中止/断连发生在工具调用参数发完之前：不能标 completed，也不能发
        // function_call_arguments.done——否则 Codex 会拿半截 JSON 去执行。
        let raw = "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call1\",\"name\":\"shell\",\"input\":{}}}\n\n\
                   data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"cmd\\\"\"}}\n\n";
        let upstream = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(raw))]);
        let out: Vec<Bytes> = convert_stream(upstream, ToolContext::default())
            .map(|r| r.unwrap())
            .collect()
            .await;
        let events = events_of(&out);
        assert!(
            !events
                .iter()
                .any(|e| e["type"] == "response.function_call_arguments.done")
        );
        let completed = events.last().unwrap();
        assert_eq!(completed["response"]["status"], "incomplete");
        assert_eq!(completed["response"]["output"][0]["status"], "incomplete");
    }
}
