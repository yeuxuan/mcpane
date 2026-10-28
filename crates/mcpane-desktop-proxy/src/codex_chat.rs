//! Codex（Responses API）↔ Chat Completions 协议转换（ADR-022 二期）。
//!
//! Codex 2026-02 移除了 `wire_api = "chat"`，只给 Chat 端点的厂商（DeepSeek/GLM/Kimi…）
//! 因此无法直连。路由接管时 live 恒写 responses，由这里把请求转成 Chat、把响应
//! （含 SSE）转回 Responses 事件流。不做厂商特化分支与跨轮 reasoning 缓存；
//! 工具结果图片搬运见 `media`。

use std::collections::{BTreeMap, HashMap};

use axum::body::Bytes;
use futures_util::{Stream, StreamExt};
use serde_json::{Map, Value, json};

const TOOL_SEARCH_NAME: &str = "tool_search";
const CUSTOM_INPUT_FIELD: &str = "input";
const CHAT_TOOL_NAME_MAX_LEN: usize = 64;
const PASSTHROUGH_FIELDS: &[&str] = &[
    "temperature",
    "top_p",
    "stream",
    "parallel_tool_calls",
    "response_format",
    "seed",
    "stop",
    "user",
];

// ---------------------------------------------------------------------------
// 工具目录：Responses 的 function / namespace / custom / tool_search → Chat function
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolKind {
    Function,
    Custom,
    ToolSearch,
}

#[derive(Debug, Clone)]
struct ToolSpec {
    kind: ToolKind,
    /// Responses 侧原名。
    name: String,
    namespace: Option<String>,
}

/// 请求里声明的工具与 Chat 名的双向映射（响应转换时还原 namespace / custom 形态）。
#[derive(Debug, Clone, Default)]
pub(crate) struct ToolContext {
    chat_tools: Vec<Value>,
    by_chat_name: HashMap<String, ToolSpec>,
}

impl ToolContext {
    /// 请求里声明的工具（已转成 Chat function 形态）。
    pub(crate) fn chat_tools(&self) -> &[Value] {
        &self.chat_tools
    }

    pub(crate) fn from_request(body: &Value) -> Self {
        let mut ctx = Self::default();
        for tool in body
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            ctx.add_tool(tool);
        }
        if let Some(input) = body.get("input") {
            ctx.collect_input_declared(input);
        }
        ctx
    }

    fn add(&mut self, chat_name: String, spec: ToolSpec, chat_tool: Value) {
        if chat_name.trim().is_empty() || self.by_chat_name.contains_key(&chat_name) {
            return;
        }
        self.by_chat_name.insert(chat_name, spec);
        self.chat_tools.push(chat_tool);
    }

    fn add_tool(&mut self, tool: &Value) {
        match tool.get("type").and_then(Value::as_str) {
            Some("function") => self.add_function(tool, None),
            Some("custom") => self.add_custom(tool),
            Some("tool_search") => self.add_tool_search(),
            Some("namespace") => {
                let Some(namespace) = tool.get("name").and_then(Value::as_str) else {
                    return;
                };
                let children = tool
                    .get("tools")
                    .or_else(|| tool.get("children"))
                    .and_then(Value::as_array);
                for child in children.into_iter().flatten() {
                    if child.get("type").and_then(Value::as_str) == Some("function") {
                        self.add_function(child, Some(namespace));
                    }
                }
            }
            _ => {}
        }
    }

    fn add_function(&mut self, tool: &Value, namespace: Option<&str>) {
        let Some(name) = tool_name(tool) else {
            return;
        };
        let chat_name = namespace.map_or_else(|| name.clone(), |ns| flatten_name(ns, &name));
        let source = tool.get("function").unwrap_or(tool);
        let mut function = json!({ "name": chat_name });
        if let Some(description) = source.get("description").filter(|d| !d.is_null()) {
            function["description"] = description.clone();
        }
        function["parameters"] = normalize_parameters(source.get("parameters"));
        if let Some(strict) = source.get("strict").or_else(|| tool.get("strict")) {
            function["strict"] = strict.clone();
        }
        let spec = ToolSpec {
            kind: ToolKind::Function,
            name,
            namespace: namespace.map(str::to_owned),
        };
        self.add(
            chat_name,
            spec,
            json!({ "type": "function", "function": function }),
        );
    }

    /// 自由格式工具（如 apply_patch）：包成单字符串参数的 function，原定义嵌进描述。
    fn add_custom(&mut self, tool: &Value) {
        let Some(name) = tool_name(tool) else {
            return;
        };
        let description = format!(
            "Original tool definition:\n```json\n{}\n```",
            serde_json::to_string(tool).unwrap_or_default()
        );
        let chat_tool = json!({
            "type": "function",
            "function": {
                "name": name,
                "description": description,
                "parameters": {
                    "type": "object",
                    "properties": {
                        CUSTOM_INPUT_FIELD: {
                            "type": "string",
                            "description": "Raw string input for the original custom tool. Preserve formatting exactly."
                        }
                    },
                    "required": [CUSTOM_INPUT_FIELD]
                }
            }
        });
        let spec = ToolSpec {
            kind: ToolKind::Custom,
            name: name.clone(),
            namespace: None,
        };
        self.add(name, spec, chat_tool);
    }

    fn add_tool_search(&mut self) {
        let chat_tool = json!({
            "type": "function",
            "function": {
                "name": TOOL_SEARCH_NAME,
                "description": "Search and load Codex tools, plugins, connectors, and MCP namespaces for the current task.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search query for tools or connectors to load." },
                        "limit": { "type": "integer", "description": "Maximum number of tool groups to return." }
                    },
                    "required": ["query"]
                }
            }
        });
        let spec = ToolSpec {
            kind: ToolKind::ToolSearch,
            name: TOOL_SEARCH_NAME.into(),
            namespace: None,
        };
        self.add(TOOL_SEARCH_NAME.into(), spec, chat_tool);
    }

    /// `tool_search_output` / `additional_tools` 在 input 里动态声明的工具。
    fn collect_input_declared(&mut self, value: &Value) {
        match value {
            Value::Array(items) => items
                .iter()
                .for_each(|item| self.collect_input_declared(item)),
            Value::Object(obj) => {
                if matches!(
                    obj.get("type").and_then(Value::as_str),
                    Some("tool_search_output" | "additional_tools")
                ) {
                    for tool in obj
                        .get("tools")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        self.add_tool(tool);
                    }
                }
            }
            _ => {}
        }
    }

    pub(crate) fn chat_name_for(&self, name: &str, namespace: Option<&str>) -> String {
        match namespace.filter(|ns| !ns.is_empty()) {
            Some(ns) => self
                .by_chat_name
                .iter()
                .find(|(_, spec)| spec.name == name && spec.namespace.as_deref() == Some(ns))
                .map_or_else(|| flatten_name(ns, name), |(chat, _)| chat.clone()),
            None => name.to_owned(),
        }
    }

    pub(crate) fn is_custom(&self, chat_name: &str) -> bool {
        self.by_chat_name
            .get(chat_name)
            .is_some_and(|spec| spec.kind == ToolKind::Custom)
    }

    /// Chat 工具调用 → Responses 输出项（按声明还原 namespace / custom / tool_search）。
    pub(crate) fn response_item(
        &self,
        status: &str,
        call_id: &str,
        chat_name: &str,
        arguments: &str,
    ) -> Value {
        match self.by_chat_name.get(chat_name) {
            Some(spec) if spec.kind == ToolKind::ToolSearch => json!({
                "type": "tool_search_call",
                "call_id": call_id,
                "status": status,
                "execution": "client",
                "arguments": parse_arguments_object(arguments),
            }),
            Some(spec) if spec.kind == ToolKind::Custom => json!({
                "id": format!("ctc_{call_id}"),
                "type": "custom_tool_call",
                "status": status,
                "call_id": call_id,
                "name": spec.name,
                "input": custom_input_from_arguments(arguments),
            }),
            spec => {
                let mut item = json!({
                    "id": format!("fc_{call_id}"),
                    "type": "function_call",
                    "status": status,
                    "call_id": call_id,
                    "name": spec.map_or(chat_name, |s| s.name.as_str()),
                    "arguments": arguments,
                });
                if let Some(ns) = spec.and_then(|s| s.namespace.as_deref()) {
                    item["namespace"] = json!(ns);
                }
                item
            }
        }
    }

    pub(crate) fn item_id(&self, call_id: &str, chat_name: &str) -> String {
        if self.is_custom(chat_name) {
            format!("ctc_{call_id}")
        } else {
            format!("fc_{call_id}")
        }
    }
}

fn tool_name(tool: &Value) -> Option<String> {
    tool.pointer("/function/name")
        .or_else(|| tool.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

/// namespace 工具拍平为 `ns__name`；超过 Chat 64 字符上限时截断并附短哈希保唯一。
pub(crate) fn flatten_name(namespace: &str, name: &str) -> String {
    let full = format!("{namespace}__{name}");
    if full.len() <= CHAT_TOOL_NAME_MAX_LEN {
        return full;
    }
    let suffix = format!("__{:08x}", fnv1a(full.as_bytes()));
    let mut prefix = String::new();
    for ch in full.chars() {
        if prefix.len() + ch.len_utf8() + suffix.len() > CHAT_TOOL_NAME_MAX_LEN {
            break;
        }
        prefix.push(ch);
    }
    format!("{prefix}{suffix}")
}

/// 稳定短哈希（名称去重用，非安全用途；避免为此引入 sha2）。
fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(*byte)).wrapping_mul(0x0100_0193)
    })
}

/// Chat 严格要求 `parameters.type == "object"`。
fn normalize_parameters(params: Option<&Value>) -> Value {
    let mut params = match params {
        Some(Value::Object(obj)) => Value::Object(obj.clone()),
        _ => json!({ "type": "object", "properties": {} }),
    };
    if params.get("type").and_then(Value::as_str) != Some("object") {
        params["type"] = json!("object");
    }
    params
}

fn parse_arguments_object(arguments: &str) -> Value {
    if arguments.trim().is_empty() {
        return json!({});
    }
    serde_json::from_str::<Value>(arguments)
        .ok()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({ "query": arguments }))
}

pub(crate) fn custom_input_from_arguments(arguments: &str) -> String {
    match serde_json::from_str::<Value>(arguments) {
        Ok(Value::Object(obj)) => obj
            .get(CUSTOM_INPUT_FIELD)
            .and_then(Value::as_str)
            .unwrap_or(arguments)
            .to_owned(),
        _ => arguments.to_owned(),
    }
}

fn arguments_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => "{}".into(),
        Some(other) => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// 请求：Responses → Chat Completions
// ---------------------------------------------------------------------------

/// 转换请求体；同时返回工具上下文供响应侧还原。
pub(crate) fn request_to_chat(body: &Value) -> (Value, ToolContext) {
    let ctx = ToolContext::from_request(body);
    let mut out = json!({});
    let model = body.get("model").and_then(Value::as_str).unwrap_or("");
    if let Some(model) = body.get("model") {
        out["model"] = model.clone();
    }

    let mut messages = Vec::new();
    if let Some(instructions) = body.get("instructions") {
        let text = instruction_text(instructions);
        if !text.is_empty() {
            messages.push(json!({ "role": "system", "content": text }));
        }
    }
    if let Some(input) = body.get("input") {
        InputConverter::new(&ctx, &mut messages).convert(input);
    }
    out["messages"] = Value::Array(pair_tool_messages(collapse_system_messages(messages)));

    if let Some(max) = body.get("max_output_tokens") {
        // o 系列 / GPT-5+ 严格拒收 max_tokens，只认 max_completion_tokens；
        // Grok 4.5+ 虽也支持 reasoning effort，但沿用 Chat 传统的 max_tokens。
        let field = if crate::effort::uses_max_completion_tokens(model) {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        out[field] = max.clone();
    }
    // Codex 的思考强度：仅转发给认得该参数的模型，避免 DeepSeek/Kimi 等被未知字段拒收。
    if crate::effort::supports_reasoning_effort(model)
        && let Some(effort) = body.pointer("/reasoning/effort")
    {
        out["reasoning_effort"] = effort.clone();
    }
    for key in PASSTHROUGH_FIELDS {
        if let Some(value) = body.get(*key) {
            out[*key] = value.clone();
        }
    }
    if !ctx.chat_tools.is_empty() {
        out["tools"] = Value::Array(ctx.chat_tools.clone());
        if let Some(choice) = body.get("tool_choice") {
            out["tool_choice"] = tool_choice_to_chat(choice, &ctx);
        }
    } else if let Some(obj) = out.as_object_mut() {
        // 严格的 OpenAI 兼容网关拒收无 tools 时的这两个字段。
        obj.remove("parallel_tool_calls");
    }
    // 流式下 OpenAI 兼容上游默认不回 usage，显式要求末尾带上。
    if out.get("stream").and_then(Value::as_bool) == Some(true) {
        out["stream_options"] = json!({ "include_usage": true });
    }
    (out, ctx)
}

fn instruction_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str).or_else(|| p.as_str()))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

/// 多数 Chat 厂商只认开头的一条 system；developer/system 消息合并到首位。
fn collapse_system_messages(messages: Vec<Value>) -> Vec<Value> {
    let mut system = Vec::new();
    let mut rest = Vec::with_capacity(messages.len());
    for msg in messages {
        if msg.get("role").and_then(Value::as_str) == Some("system")
            && let Some(text) = msg.get("content").and_then(Value::as_str)
        {
            if !text.trim().is_empty() {
                system.push(text.to_owned());
            }
            continue;
        }
        rest.push(msg);
    }
    let mut out = Vec::with_capacity(rest.len() + 1);
    if !system.is_empty() {
        out.push(json!({ "role": "system", "content": system.join("\n\n") }));
    }
    out.extend(rest);
    out
}

/// 未应答的工具调用补的占位结果。
const INTERRUPTED_RESULT: &str = "unavailable: the turn was interrupted";

fn role_of(message: &Value) -> &str {
    message.get("role").and_then(Value::as_str).unwrap_or("")
}

/// 严格的 Chat 上游（Kimi 等）逐条核对工具回合：每个 `tool` 消息的 id 必须对得上紧邻
/// 前一条 assistant 的调用，每个调用都得有应答，否则整请求 400（`tool_call_id is not found`），
/// 而 Codex 每次重试都重发同一段历史，会话就此卡死。压缩与中断会留下孤立的输出和没应答的调用，
/// 这里在发出前修补：
/// - assistant 之后的 `tool` 消息按其调用顺序排列；
/// - 同一调用的重复应答只留第一条；
/// - 应答不存在 / 已关闭 / 空 id 调用的输出降级为 user 文本（不凭空造调用、不重复用 id）；
/// - 没有应答的调用补一条占位结果。
///
/// 良构的回合逐字节不变。
fn pair_tool_messages(messages: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(messages.len());
    let mut iter = messages.into_iter().peekable();
    while let Some(message) = iter.next() {
        if role_of(&message) == "tool" {
            push_orphaned_output(&mut out, &message);
            continue;
        }
        let call_ids: Vec<String> = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .map(|calls| {
                calls
                    .iter()
                    .map(|call| {
                        call.get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned()
                    })
                    .collect()
            })
            .unwrap_or_default();
        if role_of(&message) != "assistant" || call_ids.is_empty() {
            out.push(message);
            continue;
        }
        out.push(message);
        // 这条 assistant 的回合：到下一条 assistant 为止的全部消息。
        let mut answers: Vec<Option<Value>> = vec![None; call_ids.len()];
        let mut others = Vec::new();
        let mut orphans = Vec::new();
        while iter.peek().is_some_and(|next| role_of(next) != "assistant") {
            let Some(next) = iter.next() else { break };
            if role_of(&next) != "tool" {
                others.push(next);
                continue;
            }
            let id = next
                .get("tool_call_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            match call_ids.iter().position(|call_id| call_id == id) {
                Some(index) if !id.is_empty() && answers[index].is_none() => {
                    answers[index] = Some(next);
                }
                _ => orphans.push(next),
            }
        }
        for (id, answer) in call_ids.iter().zip(answers) {
            out.push(answer.unwrap_or_else(
                || json!({ "role": "tool", "tool_call_id": id, "content": INTERRUPTED_RESULT }),
            ));
        }
        out.extend(others);
        for orphan in &orphans {
            push_orphaned_output(&mut out, orphan);
        }
    }
    out
}

/// 找不到对应调用的工具输出：作为文字并入前一条 user 消息（没有就新开一条）。
fn push_orphaned_output(out: &mut Vec<Value>, tool_message: &Value) {
    let text = match tool_message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    if text.trim().is_empty() {
        return;
    }
    let text = format!("[output of an earlier tool call]\n{text}");
    if let Some(last) = out.last_mut()
        && role_of(last) == "user"
        && let Some(existing) = last.get("content").and_then(Value::as_str)
    {
        let merged = format!("{existing}\n\n{text}");
        last["content"] = Value::String(merged);
        return;
    }
    out.push(json!({ "role": "user", "content": text }));
}

fn tool_choice_to_chat(choice: &Value, ctx: &ToolContext) -> Value {
    let chat_name = match choice.get("type").and_then(Value::as_str) {
        Some("function") => ctx.chat_name_for(
            choice.get("name").and_then(Value::as_str).unwrap_or(""),
            choice.get("namespace").and_then(Value::as_str),
        ),
        Some("custom") => choice
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        Some("tool_search") => TOOL_SEARCH_NAME.to_owned(),
        _ => return choice.clone(),
    };
    json!({ "type": "function", "function": { "name": chat_name } })
}

/// input 项 → Chat 消息。同一模型回合的「文本 + 工具调用」合并为一条 assistant，
/// reasoning 前向附挂到其后的 assistant（DeepSeek/Kimi 等思考模型要求工具调用
/// 消息回传 `reasoning_content`）。
struct InputConverter<'a> {
    ctx: &'a ToolContext,
    messages: &'a mut Vec<Value>,
    pending_calls: Vec<Value>,
    pending_reasoning: Option<String>,
    last_assistant: Option<usize>,
    /// 连续 tool 输出消息搬运出的图片；遇到非 tool 输出 item 或收尾时落地。
    pending_images: crate::media::PendingToolImages,
}

impl<'a> InputConverter<'a> {
    fn new(ctx: &'a ToolContext, messages: &'a mut Vec<Value>) -> Self {
        Self {
            ctx,
            messages,
            pending_calls: Vec::new(),
            pending_reasoning: None,
            last_assistant: None,
            pending_images: crate::media::PendingToolImages::default(),
        }
    }

    fn convert(mut self, input: &Value) {
        match input {
            Value::String(text) => self
                .messages
                .push(json!({ "role": "user", "content": text })),
            Value::Array(items) => items.iter().for_each(|item| self.item(item)),
            Value::Object(_) => self.item(input),
            _ => {}
        }
        self.flush_calls();
        self.flush_pending_images();
        self.attach_reasoning_to_last_assistant();
        // 思考模型要求每条带 tool_calls 的 assistant 都有非空 reasoning_content。
        for msg in self.messages.iter_mut() {
            let has_calls = msg
                .get("tool_calls")
                .and_then(Value::as_array)
                .is_some_and(|calls| !calls.is_empty());
            let has_reasoning = msg
                .get("reasoning_content")
                .and_then(Value::as_str)
                .is_some_and(|r| !r.trim().is_empty());
            if has_calls && !has_reasoning {
                msg["reasoning_content"] = json!("tool call");
            }
        }
    }

    fn item(&mut self, item: &Value) {
        let item_type = item.get("type").and_then(Value::as_str);
        // tool 输出消息必须紧跟在一起；一旦出现非 tool 输出 item，先把攒的
        // 图片落地成合成 user 消息，再处理当前 item。
        if !matches!(
            item_type,
            Some("function_call_output" | "custom_tool_call_output" | "tool_search_output")
        ) {
            self.flush_pending_images();
        }
        match item_type {
            Some("function_call") => {
                let chat_name = self.ctx.chat_name_for(
                    item.get("name").and_then(Value::as_str).unwrap_or(""),
                    item.get("namespace").and_then(Value::as_str),
                );
                self.pending_calls.push(chat_tool_call(
                    item,
                    &chat_name,
                    arguments_string(item.get("arguments")),
                ));
            }
            Some("custom_tool_call") => {
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let input = item.get("input").cloned().unwrap_or_else(|| json!(""));
                let arguments = json!({ CUSTOM_INPUT_FIELD: input }).to_string();
                self.pending_calls
                    .push(chat_tool_call(item, &name, arguments));
            }
            Some("tool_search_call") => {
                let arguments = item
                    .get("arguments")
                    .map_or_else(|| "{}".into(), Value::to_string);
                self.pending_calls
                    .push(chat_tool_call(item, TOOL_SEARCH_NAME, arguments));
            }
            Some("function_call_output" | "custom_tool_call_output" | "tool_search_output") => {
                self.flush_calls();
                let call_id = item.get("call_id").and_then(Value::as_str).unwrap_or("");
                let content = match item.get("output") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Array(parts)) => {
                        let (text, images) = crate::media::split_responses_tool_output_parts(parts);
                        self.pending_images.push(call_id, images);
                        text
                    }
                    Some(other) if item_type == Some("function_call_output") => other.to_string(),
                    _ => item.to_string(),
                };
                self.messages
                    .push(json!({ "role": "tool", "tool_call_id": call_id, "content": content }));
            }
            Some("reasoning") => {
                if let Some(text) = reasoning_item_text(item) {
                    append_text(&mut self.pending_reasoning, &text);
                }
            }
            Some("additional_tools") => {}
            _ => {
                if item.get("role").is_none() && item.get("content").is_none() {
                    self.flush_calls();
                    return;
                }
                self.flush_calls();
                let role = chat_role(item.get("role").and_then(Value::as_str).unwrap_or("user"));
                let content = match item.get("content") {
                    Some(content) => content_to_chat(content),
                    // input_text 等裸内容项。
                    None => content_to_chat(&Value::Array(vec![item.clone()])),
                };
                let mut msg = json!({ "role": role, "content": content });
                if role == "assistant" {
                    if let Some(reasoning) = self.pending_reasoning.take()
                        && let Some(obj) = msg.as_object_mut()
                    {
                        append_reasoning(obj, &reasoning);
                    }
                    self.last_assistant = Some(self.messages.len());
                } else {
                    // 回合边界：未消费的 reasoning 归上一条 assistant，不跨回合泄漏。
                    self.attach_reasoning_to_last_assistant();
                    self.last_assistant = None;
                }
                self.messages.push(msg);
            }
        }
    }

    fn flush_calls(&mut self) {
        if self.pending_calls.is_empty() {
            return;
        }
        let calls = std::mem::take(&mut self.pending_calls);
        // 紧邻的纯文本 assistant 与工具调用同属一个模型回合，合并为一条。
        if let Some(last) = self.messages.last_mut()
            && last.get("role").and_then(Value::as_str) == Some("assistant")
            && last.get("tool_calls").is_none()
            && let Some(obj) = last.as_object_mut()
        {
            obj.insert("tool_calls".into(), Value::Array(calls));
            if let Some(reasoning) = self.pending_reasoning.take() {
                append_reasoning(obj, &reasoning);
            }
            self.last_assistant = Some(self.messages.len() - 1);
            return;
        }
        let mut msg = json!({ "role": "assistant", "content": null, "tool_calls": calls });
        if let Some(reasoning) = self.pending_reasoning.take()
            && let Some(obj) = msg.as_object_mut()
        {
            append_reasoning(obj, &reasoning);
        }
        self.last_assistant = Some(self.messages.len());
        self.messages.push(msg);
    }

    fn flush_pending_images(&mut self) {
        if let Some(msg) = self.pending_images.take_message() {
            self.messages.push(msg);
        }
    }

    fn attach_reasoning_to_last_assistant(&mut self) {
        let Some(reasoning) = self.pending_reasoning.take() else {
            return;
        };
        if let Some(obj) = self
            .last_assistant
            .and_then(|index| self.messages.get_mut(index))
            .and_then(Value::as_object_mut)
        {
            append_reasoning(obj, &reasoning);
        }
    }
}

fn chat_tool_call(item: &Value, chat_name: &str, arguments: String) -> Value {
    let call_id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("");
    json!({
        "id": call_id,
        "type": "function",
        "function": { "name": chat_name, "arguments": arguments }
    })
}

fn chat_role(role: &str) -> &'static str {
    match role {
        "system" | "developer" => "system",
        "assistant" => "assistant",
        _ => "user",
    }
}

fn text_of_parts(parts: &[Value]) -> String {
    parts
        .iter()
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Responses 内容数组 → Chat content：纯文本合并为字符串，含图片时保留分段数组。
fn content_to_chat(content: &Value) -> Value {
    let Some(parts) = content.as_array() else {
        return content.clone();
    };
    let mut chat_parts = Vec::new();
    let mut has_media = false;
    for part in parts {
        match part.get("type").and_then(Value::as_str).unwrap_or("") {
            "input_text" | "output_text" | "text" => {
                if let Some(text) = part
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                {
                    chat_parts.push(json!({ "type": "text", "text": text }));
                }
            }
            "refusal" => {
                if let Some(text) = part.get("refusal").and_then(Value::as_str) {
                    chat_parts.push(json!({ "type": "text", "text": text }));
                }
            }
            "input_image" => {
                if let Some(url) = part.get("image_url") {
                    let mut url = match url {
                        Value::Object(obj) => obj.clone(),
                        other => {
                            let mut obj = Map::new();
                            obj.insert("url".into(), json!(other.as_str().unwrap_or_default()));
                            obj
                        }
                    };
                    merge_top_level_detail(part, &mut url);
                    chat_parts
                        .push(json!({ "type": "image_url", "image_url": Value::Object(url) }));
                    has_media = true;
                }
            }
            "input_file" => {
                if let Some(file) = chat_file_from_input_file(part) {
                    chat_parts.push(json!({ "type": "file", "file": file }));
                    has_media = true;
                }
            }
            _ => {}
        }
    }
    if has_media {
        Value::Array(chat_parts)
    } else {
        Value::String(text_of_parts(&chat_parts))
    }
}

/// 把 part 顶层的 `detail` 并入 `image_url` 对象；严格 Chat 网关只认
/// auto/low/high，Codex 的 `original`（原图）会被 400 拒收，降级为 auto。
fn merge_top_level_detail(part: &Value, image_url: &mut Map<String, Value>) {
    if !image_url.contains_key("detail")
        && let Some(detail) = part.get("detail")
    {
        image_url.insert("detail".into(), detail.clone());
    }
    if image_url.get("detail").and_then(Value::as_str) == Some("original") {
        image_url.insert("detail".into(), json!("auto"));
    }
}

/// Responses `input_file` → Chat file 载荷；必须有 file_id 或 file_data 才有意义。
fn chat_file_from_input_file(part: &Value) -> Option<Value> {
    let mut file = Map::new();
    if part.get("file_id").is_none() && part.get("file_data").is_none() {
        return None;
    }
    for key in ["file_id", "file_data", "filename"] {
        if let Some(value) = part.get(key) {
            file.insert(key.to_owned(), value.clone());
        }
    }
    Some(Value::Object(file))
}

fn reasoning_item_text(item: &Value) -> Option<String> {
    let from_parts = |key: &str| {
        let text = item
            .get(key)?
            .as_array()?
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        (!text.is_empty()).then_some(text)
    };
    from_parts("content").or_else(|| from_parts("summary"))
}

fn append_text(slot: &mut Option<String>, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    match slot {
        Some(existing) if !existing.is_empty() => {
            existing.push_str("\n\n");
            existing.push_str(text);
        }
        _ => *slot = Some(text.to_owned()),
    }
}

fn append_reasoning(msg: &mut Map<String, Value>, reasoning: &str) {
    let mut slot = msg
        .get("reasoning_content")
        .and_then(Value::as_str)
        .map(str::to_owned);
    append_text(&mut slot, reasoning);
    if let Some(text) = slot {
        msg.insert("reasoning_content".into(), Value::String(text));
    }
}

// ---------------------------------------------------------------------------
// 响应：Chat Completions → Responses（非流式）
// ---------------------------------------------------------------------------

/// 上游 reasoning 字段（各家写法不一）。
fn reasoning_field(value: &Value) -> Option<String> {
    for key in ["reasoning_content", "reasoning"] {
        if let Some(text) = value
            .get(key)
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
        {
            return Some(text.to_owned());
        }
    }
    value
        .pointer("/reasoning/content")
        .or_else(|| value.pointer("/reasoning/text"))
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
}

/// 部分厂商把思考写在正文开头的 `<think>…</think>` 里。
fn split_think(text: &str) -> Option<(String, String)> {
    let body = text.trim_start().strip_prefix("<think>")?;
    let (reasoning, answer) = body.split_once("</think>")?;
    Some((reasoning.trim().to_owned(), answer.trim_start().to_owned()))
}

fn response_id(chat_id: Option<&str>) -> String {
    let id = chat_id.unwrap_or("mcpane");
    if id.starts_with("resp_") {
        id.to_owned()
    } else {
        format!("resp_{id}")
    }
}

fn status_from_finish(finish_reason: Option<&str>) -> &'static str {
    if finish_reason == Some("length") {
        "incomplete"
    } else {
        "completed"
    }
}

pub(crate) fn usage_to_responses(usage: Option<&Value>) -> Value {
    let usage = usage.filter(|u| u.is_object());
    let get = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| usage.and_then(|u| u.pointer(k)).and_then(Value::as_u64))
            .unwrap_or(0)
    };
    let input = get(&["/prompt_tokens", "/input_tokens"]);
    let output = get(&["/completion_tokens", "/output_tokens"]);
    let cached = get(&[
        "/prompt_tokens_details/cached_tokens",
        "/prompt_cache_hit_tokens",
        "/cache_read_input_tokens",
    ]);
    let cache_write = get(&[
        "/prompt_tokens_details/cache_write_tokens",
        "/cache_creation_input_tokens",
    ]);
    let reasoning = get(&["/completion_tokens_details/reasoning_tokens"]);
    let total = usage
        .and_then(|u| u.get("total_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(input + output);
    let mut input_tokens_details = json!({ "cached_tokens": cached });
    if cache_write > 0
        && let Some(obj) = input_tokens_details.as_object_mut()
    {
        obj.insert("cache_write_tokens".into(), json!(cache_write));
    }
    json!({
        "input_tokens": input,
        "input_tokens_details": input_tokens_details,
        "output_tokens": output,
        "output_tokens_details": { "reasoning_tokens": reasoning },
        "total_tokens": total,
    })
}

pub(crate) fn response_from_chat(body: &Value, ctx: &ToolContext) -> Result<Value, String> {
    let choice = body
        .pointer("/choices/0")
        .ok_or("上游 Chat 响应缺少 choices")?;
    let message = choice.get("message").ok_or("上游 Chat 响应缺少 message")?;
    let id = response_id(body.get("id").and_then(Value::as_str));
    let finish = choice.get("finish_reason").and_then(Value::as_str);

    let raw_text = message.get("content").and_then(Value::as_str).unwrap_or("");
    let (inline_reasoning, text) = match split_think(raw_text) {
        Some((reasoning, answer)) => (Some(reasoning).filter(|r| !r.is_empty()), answer),
        None => (None, raw_text.to_owned()),
    };
    let mut output = Vec::new();
    if let Some(reasoning) = reasoning_field(message).or(inline_reasoning) {
        output.push(reasoning_item(&format!("rs_{id}"), &reasoning));
    }
    if !text.is_empty() {
        output.push(message_item(&format!("{id}_msg"), &text));
    }
    for (index, call) in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let name = call
            .pointer("/function/name")
            .and_then(Value::as_str)
            .unwrap_or("");
        if name.trim().is_empty() {
            continue;
        }
        let call_id = call
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map_or_else(|| format!("call_{index}"), str::to_owned);
        let arguments = arguments_string(call.pointer("/function/arguments"));
        output.push(ctx.response_item("completed", &call_id, name, &arguments));
    }

    let mut response = json!({
        "id": id,
        "object": "response",
        "created_at": body.get("created").and_then(Value::as_u64).unwrap_or(0),
        "status": status_from_finish(finish),
        "model": body.get("model").cloned().unwrap_or(Value::Null),
        "output": output,
        "usage": usage_to_responses(body.get("usage")),
    });
    if finish == Some("length") {
        response["incomplete_details"] = json!({ "reason": "max_output_tokens" });
    }
    Ok(response)
}

/// 上游 Chat 错误体 → Responses 错误形态（Codex 只识别 `{"error":{"message"}}`）。
pub(crate) fn error_message_from_chat(body: &[u8]) -> String {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return String::from_utf8_lossy(body)
            .trim()
            .chars()
            .take(500)
            .collect();
    };
    value
        .pointer("/error/message")
        .or_else(|| value.get("message"))
        .or_else(|| value.get("detail"))
        .or_else(|| value.pointer("/base_resp/status_msg"))
        .or_else(|| value.get("error"))
        .and_then(Value::as_str)
        .map_or_else(|| value.to_string(), str::to_owned)
}

fn message_item(item_id: &str, text: &str) -> Value {
    json!({
        "id": item_id,
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "content": [{ "type": "output_text", "text": text, "annotations": [] }]
    })
}

fn reasoning_item(item_id: &str, text: &str) -> Value {
    json!({
        "id": item_id,
        "type": "reasoning",
        "summary": [{ "type": "summary_text", "text": text }]
    })
}

// ---------------------------------------------------------------------------
// 响应：Chat SSE → Responses SSE（流式）
// ---------------------------------------------------------------------------

fn sse(event: &str, data: Value) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {data}\n\n"))
}

#[derive(Default)]
struct PartState {
    output_index: u32,
    item_id: String,
    text: String,
    added: bool,
    done: bool,
}

#[derive(Default)]
struct ToolState {
    output_index: u32,
    item_id: String,
    call_id: String,
    name: String,
    arguments: String,
    added: bool,
    done: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum ThinkMode {
    #[default]
    Detecting,
    Reasoning,
    Text,
}

/// Chat chunk 序列 → Responses 事件序列的状态机。
pub(crate) struct StreamConverter {
    ctx: ToolContext,
    started: bool,
    completed: bool,
    response_id: String,
    model: String,
    created_at: u64,
    next_index: u32,
    text: PartState,
    reasoning: PartState,
    think_mode: ThinkMode,
    think_buffer: String,
    tools: BTreeMap<usize, ToolState>,
    next_tool_to_add: usize,
    output: Vec<(u32, Value)>,
    usage: Option<Value>,
    finish_reason: Option<String>,
}

impl StreamConverter {
    pub(crate) fn new(ctx: ToolContext) -> Self {
        Self {
            ctx,
            started: false,
            completed: false,
            response_id: "resp_mcpane".into(),
            model: String::new(),
            created_at: 0,
            next_index: 0,
            text: PartState::default(),
            reasoning: PartState::default(),
            think_mode: ThinkMode::Detecting,
            think_buffer: String::new(),
            tools: BTreeMap::new(),
            next_tool_to_add: 0,
            output: Vec::new(),
            usage: None,
            finish_reason: None,
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
            "id": self.response_id,
            "object": "response",
            "created_at": self.created_at,
            "status": status,
            "model": self.model,
            "output": output.into_iter().map(|(_, item)| item).collect::<Vec<_>>(),
            "usage": self.usage.clone().unwrap_or_else(|| usage_to_responses(None)),
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

    pub(crate) fn handle_chunk(&mut self, chunk: &Value) -> Vec<Bytes> {
        if let Some(id) = chunk.get("id").and_then(Value::as_str) {
            self.response_id = response_id(Some(id));
        }
        if let Some(model) = chunk
            .get("model")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
        {
            model.clone_into(&mut self.model);
        }
        if let Some(created) = chunk.get("created").and_then(Value::as_u64) {
            self.created_at = created;
        }
        let mut events = self.ensure_started();
        if let Some(usage) = chunk.get("usage").filter(|u| !u.is_null()) {
            self.usage = Some(usage_to_responses(Some(usage)));
        }
        let Some(choice) = chunk.pointer("/choices/0") else {
            return events;
        };
        if let Some(delta) = choice.get("delta") {
            if let Some(reasoning) = reasoning_field(delta) {
                events.extend(self.reasoning_delta(&reasoning));
            }
            if let Some(content) = delta
                .get("content")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty())
            {
                events.extend(self.content_delta(content));
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                events.extend(self.flush_think_buffer());
                events.extend(self.close_reasoning());
                for call in calls {
                    events.extend(self.tool_delta(call));
                }
            }
        }
        if let Some(finish) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish_reason = Some(finish.to_owned());
        }
        events
    }

    fn reasoning_delta(&mut self, delta: &str) -> Vec<Bytes> {
        let mut events = Vec::new();
        if !self.reasoning.added {
            let index = self.alloc_index();
            self.reasoning = PartState {
                output_index: index,
                item_id: format!("rs_{}", self.response_id),
                added: true,
                ..PartState::default()
            };
            let id = &self.reasoning.item_id;
            events.push(sse(
                "response.output_item.added",
                json!({ "type": "response.output_item.added", "output_index": index,
                        "item": { "id": id, "type": "reasoning", "status": "in_progress", "summary": [] } }),
            ));
            events.push(sse(
                "response.reasoning_summary_part.added",
                json!({ "type": "response.reasoning_summary_part.added", "item_id": id,
                        "output_index": index, "summary_index": 0,
                        "part": { "type": "summary_text", "text": "" } }),
            ));
        }
        self.reasoning.text.push_str(delta);
        events.push(sse(
            "response.reasoning_summary_text.delta",
            json!({ "type": "response.reasoning_summary_text.delta", "item_id": self.reasoning.item_id,
                    "output_index": self.reasoning.output_index, "summary_index": 0, "delta": delta }),
        ));
        events
    }

    fn close_reasoning(&mut self) -> Vec<Bytes> {
        if !self.reasoning.added || self.reasoning.done {
            return Vec::new();
        }
        self.reasoning.done = true;
        let (index, id, text) = (
            self.reasoning.output_index,
            self.reasoning.item_id.clone(),
            self.reasoning.text.clone(),
        );
        let item = reasoning_item(&id, &text);
        self.output.push((index, item.clone()));
        vec![
            sse(
                "response.reasoning_summary_text.done",
                json!({ "type": "response.reasoning_summary_text.done", "item_id": id,
                        "output_index": index, "summary_index": 0, "text": text }),
            ),
            sse(
                "response.reasoning_summary_part.done",
                json!({ "type": "response.reasoning_summary_part.done", "item_id": id,
                        "output_index": index, "summary_index": 0,
                        "part": { "type": "summary_text", "text": text } }),
            ),
            sse(
                "response.output_item.done",
                json!({ "type": "response.output_item.done", "output_index": index, "item": item }),
            ),
        ]
    }

    /// 正文增量：先判定是否以 `<think>` 开头（内联思考），判定前缓冲。
    fn content_delta(&mut self, delta: &str) -> Vec<Bytes> {
        match self.think_mode {
            ThinkMode::Text => {
                let mut events = self.close_reasoning();
                events.extend(self.text_delta(delta));
                events
            }
            ThinkMode::Detecting => {
                self.think_buffer.push_str(delta);
                let trimmed = self.think_buffer.trim_start();
                if trimmed.is_empty() || ("<think>".starts_with(trimmed) && trimmed.len() < 7) {
                    Vec::new()
                } else if trimmed.starts_with("<think>") {
                    self.think_mode = ThinkMode::Reasoning;
                    self.drain_think()
                } else {
                    self.think_mode = ThinkMode::Text;
                    let text = std::mem::take(&mut self.think_buffer);
                    let mut events = self.close_reasoning();
                    events.extend(self.text_delta(&text));
                    events
                }
            }
            ThinkMode::Reasoning => {
                self.think_buffer.push_str(delta);
                self.drain_think()
            }
        }
    }

    fn drain_think(&mut self) -> Vec<Bytes> {
        let Some((reasoning, answer)) = split_think(&self.think_buffer) else {
            return Vec::new();
        };
        self.think_mode = ThinkMode::Text;
        self.think_buffer.clear();
        let mut events = Vec::new();
        if !reasoning.is_empty() {
            events.extend(self.reasoning_delta(&reasoning));
        }
        events.extend(self.close_reasoning());
        if !answer.is_empty() {
            events.extend(self.text_delta(&answer));
        }
        events
    }

    /// 边界（工具调用 / 结束）时把未决的缓冲按现状落地。
    fn flush_think_buffer(&mut self) -> Vec<Bytes> {
        let mode = std::mem::replace(&mut self.think_mode, ThinkMode::Text);
        let buffered = std::mem::take(&mut self.think_buffer);
        if buffered.trim().is_empty() {
            return Vec::new();
        }
        match mode {
            ThinkMode::Text => Vec::new(),
            ThinkMode::Detecting => {
                let mut events = self.close_reasoning();
                events.extend(self.text_delta(&buffered));
                events
            }
            ThinkMode::Reasoning => {
                let reasoning = buffered
                    .trim_start()
                    .strip_prefix("<think>")
                    .unwrap_or(&buffered)
                    .trim()
                    .to_owned();
                let mut events = self.reasoning_delta(&reasoning);
                events.extend(self.close_reasoning());
                events
            }
        }
    }

    fn text_delta(&mut self, delta: &str) -> Vec<Bytes> {
        let mut events = Vec::new();
        if !self.text.added {
            let index = self.alloc_index();
            self.text = PartState {
                output_index: index,
                item_id: format!("{}_msg", self.response_id),
                added: true,
                ..PartState::default()
            };
            let id = &self.text.item_id;
            events.push(sse(
                "response.output_item.added",
                json!({ "type": "response.output_item.added", "output_index": index,
                        "item": { "id": id, "type": "message", "status": "in_progress",
                                  "role": "assistant", "content": [] } }),
            ));
            events.push(sse(
                "response.content_part.added",
                json!({ "type": "response.content_part.added", "item_id": id, "output_index": index,
                        "content_index": 0,
                        "part": { "type": "output_text", "text": "", "annotations": [] } }),
            ));
        }
        self.text.text.push_str(delta);
        events.push(sse(
            "response.output_text.delta",
            json!({ "type": "response.output_text.delta", "item_id": self.text.item_id,
                    "output_index": self.text.output_index, "content_index": 0, "delta": delta }),
        ));
        events
    }

    fn close_text(&mut self) -> Vec<Bytes> {
        if !self.text.added || self.text.done {
            return Vec::new();
        }
        self.text.done = true;
        let (index, id, text) = (
            self.text.output_index,
            self.text.item_id.clone(),
            self.text.text.clone(),
        );
        let item = message_item(&id, &text);
        self.output.push((index, item.clone()));
        vec![
            sse(
                "response.output_text.done",
                json!({ "type": "response.output_text.done", "item_id": id, "output_index": index,
                        "content_index": 0, "text": text }),
            ),
            sse(
                "response.content_part.done",
                json!({ "type": "response.content_part.done", "item_id": id, "output_index": index,
                        "content_index": 0,
                        "part": { "type": "output_text", "text": text, "annotations": [] } }),
            ),
            sse(
                "response.output_item.done",
                json!({ "type": "response.output_item.done", "output_index": index, "item": item }),
            ),
        ]
    }

    fn tool_delta(&mut self, call: &Value) -> Vec<Bytes> {
        let key = match call.get("index").and_then(Value::as_u64) {
            Some(index) => usize::try_from(index).unwrap_or(usize::MAX),
            // 缺 index 的网关：只有出现新的非空 id 才算新调用，否则归入最后一个。
            None => {
                let last = self.tools.keys().next_back().copied();
                match call
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                {
                    Some(id) => self
                        .tools
                        .iter()
                        .find(|(_, t)| t.call_id == id)
                        .map(|(k, _)| *k)
                        .unwrap_or_else(|| last.map_or(0, |k| k.saturating_add(1))),
                    None => last.unwrap_or(0),
                }
            }
        };
        let state = self.tools.entry(key).or_default();
        if let Some(id) = call
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            id.clone_into(&mut state.call_id);
        }
        if let Some(name) = call
            .pointer("/function/name")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty())
        {
            name.clone_into(&mut state.name);
        }
        let args = call
            .pointer("/function/arguments")
            .and_then(Value::as_str)
            .unwrap_or("");
        state.arguments.push_str(args);
        let mut events = Vec::new();
        if state.added && !args.is_empty() && !self.ctx.is_custom(&state.name) {
            events.push(sse(
                "response.function_call_arguments.delta",
                json!({ "type": "response.function_call_arguments.delta", "item_id": state.item_id,
                        "output_index": state.output_index, "delta": args }),
            ));
        }
        events.extend(self.announce_ready_tools());
        events
    }

    /// 按 Chat index 顺序公布已拿到 id+name 的调用（迟到的身份片段不打乱顺序）。
    fn announce_ready_tools(&mut self) -> Vec<Bytes> {
        let mut events = Vec::new();
        while let Some(state) = self.tools.get(&self.next_tool_to_add) {
            if state.added || state.done {
                self.next_tool_to_add += 1;
                continue;
            }
            if state.call_id.is_empty() || state.name.is_empty() {
                break;
            }
            let index = self.alloc_index();
            let key = self.next_tool_to_add;
            events.extend(self.add_tool_item(key, index));
            self.next_tool_to_add += 1;
        }
        events
    }

    fn add_tool_item(&mut self, key: usize, index: u32) -> Vec<Bytes> {
        let Some(state) = self.tools.get_mut(&key) else {
            return Vec::new();
        };
        if state.call_id.is_empty() {
            state.call_id = format!("call_{key}");
        }
        state.added = true;
        state.output_index = index;
        state.item_id = self.ctx.item_id(&state.call_id, &state.name);
        let item = self
            .ctx
            .response_item("in_progress", &state.call_id, &state.name, "");
        let mut events = vec![sse(
            "response.output_item.added",
            json!({ "type": "response.output_item.added", "output_index": index, "item": item }),
        )];
        if !state.arguments.is_empty() && !self.ctx.is_custom(&state.name) {
            events.push(sse(
                "response.function_call_arguments.delta",
                json!({ "type": "response.function_call_arguments.delta", "item_id": state.item_id,
                        "output_index": index, "delta": state.arguments }),
            ));
        }
        events
    }

    fn close_tools(&mut self) -> Vec<Bytes> {
        let mut events = Vec::new();
        let keys: Vec<usize> = self.tools.keys().copied().collect();
        for key in keys {
            let Some(state) = self.tools.get(&key) else {
                continue;
            };
            if state.done {
                continue;
            }
            if state.name.trim().is_empty() {
                tracing::warn!("本地路由：丢弃缺少函数名的 Chat 工具调用（index={key}）");
                if let Some(state) = self.tools.get_mut(&key) {
                    state.done = true;
                }
                continue;
            }
            if !state.added {
                let index = self.alloc_index();
                events.extend(self.add_tool_item(key, index));
            }
            let Some(state) = self.tools.get_mut(&key) else {
                continue;
            };
            state.done = true;
            let arguments = if state.arguments.trim().is_empty() {
                "{}".to_owned()
            } else {
                state.arguments.clone()
            };
            let item = self
                .ctx
                .response_item("completed", &state.call_id, &state.name, &arguments);
            let (index, id) = (state.output_index, state.item_id.clone());
            if self.ctx.is_custom(&state.name) {
                let input = custom_input_from_arguments(&arguments);
                events.push(sse(
                    "response.custom_tool_call_input.done",
                    json!({ "type": "response.custom_tool_call_input.done", "item_id": id,
                            "output_index": index, "input": input }),
                ));
            } else {
                events.push(sse(
                    "response.function_call_arguments.done",
                    json!({ "type": "response.function_call_arguments.done", "item_id": id,
                            "output_index": index, "arguments": arguments }),
                ));
            }
            events.push(sse(
                "response.output_item.done",
                json!({ "type": "response.output_item.done", "output_index": index, "item": item }),
            ));
            self.output.push((index, item));
        }
        events
    }

    pub(crate) fn finish(&mut self) -> Vec<Bytes> {
        if self.completed {
            return Vec::new();
        }
        let mut events = self.ensure_started();
        events.extend(self.flush_think_buffer());
        events.extend(self.close_reasoning());
        events.extend(self.close_text());
        events.extend(self.close_tools());
        let status = status_from_finish(self.finish_reason.as_deref());
        let mut response = self.base_response(status);
        if status == "incomplete" {
            response["incomplete_details"] = json!({ "reason": "max_output_tokens" });
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
        !self.text.text.is_empty()
            || !self.reasoning.text.is_empty()
            || !self.think_buffer.trim().is_empty()
            || !self.tools.is_empty()
    }
}

/// 处理一块上游字节；返回待发事件与是否已终结。
fn on_bytes(
    conv: &mut StreamConverter,
    buffer: &mut String,
    remainder: &mut Vec<u8>,
    bytes: &[u8],
) -> (Vec<Bytes>, bool) {
    crate::sse::append_utf8(buffer, remainder, bytes);
    let mut events = Vec::new();
    while let Some(data) = crate::sse::take_data(buffer) {
        if data.trim() == "[DONE]" {
            events.extend(conv.finish());
            return (events, true);
        }
        let Ok(chunk) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        if chunk.get("error").is_some() {
            events.push(conv.fail(&error_message_from_chat(data.as_bytes())));
            return (events, true);
        }
        events.extend(conv.handle_chunk(&chunk));
    }
    (events, false)
}

/// 上游结束但没有 `[DONE]`：有 finish_reason 或已有产出按（截断）收尾，否则报失败。
fn on_end(conv: &mut StreamConverter) -> Vec<Bytes> {
    if conv.completed {
        Vec::new()
    } else if conv.finish_reason.is_some() {
        conv.finish()
    } else if conv.has_output() {
        conv.finish_reason = Some("length".into());
        conv.finish()
    } else {
        vec![conv.fail("上游 Chat 流在返回 finish_reason 之前结束")]
    }
}

struct StreamState<S> {
    upstream: S,
    conv: StreamConverter,
    buffer: String,
    remainder: Vec<u8>,
    ended: bool,
}

/// 把上游 Chat SSE 字节流转换为 Responses SSE 字节流。
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

    fn call(id: &str) -> Value {
        json!({ "id": id, "type": "function", "function": { "name": "run", "arguments": "{}" } })
    }

    fn assistant_calls(ids: &[&str]) -> Value {
        json!({ "role": "assistant", "content": null,
                "tool_calls": ids.iter().map(|id| call(id)).collect::<Vec<_>>() })
    }

    fn tool(id: &str, content: &str) -> Value {
        json!({ "role": "tool", "tool_call_id": id, "content": content })
    }

    fn user(text: &str) -> Value {
        json!({ "role": "user", "content": text })
    }

    #[test]
    fn well_formed_exchange_is_byte_identical() {
        let messages = vec![
            user("go"),
            assistant_calls(&["a", "b"]),
            tool("a", "1"),
            tool("b", "2"),
            json!({ "role": "user", "content": "images of the results" }),
            json!({ "role": "assistant", "content": "done" }),
        ];
        assert_eq!(pair_tool_messages(messages.clone()), messages);
    }

    #[test]
    fn results_follow_their_calls_in_call_order() {
        let paired = pair_tool_messages(vec![
            assistant_calls(&["a", "b"]),
            tool("b", "2"),
            tool("a", "1"),
        ]);
        assert_eq!(paired[1], tool("a", "1"));
        assert_eq!(paired[2], tool("b", "2"));
    }

    #[test]
    fn unanswered_call_gets_a_placeholder_result() {
        let paired = pair_tool_messages(vec![assistant_calls(&["a", "b"]), tool("a", "1")]);
        assert_eq!(paired.len(), 3);
        assert_eq!(paired[2]["tool_call_id"], "b");
        assert_eq!(paired[2]["content"], INTERRUPTED_RESULT);
    }

    #[test]
    fn duplicate_answer_keeps_the_first() {
        let paired = pair_tool_messages(vec![
            assistant_calls(&["a"]),
            tool("a", "first"),
            tool("a", "second"),
            json!({ "role": "assistant", "content": "ok" }),
        ]);
        let answers: Vec<_> = paired.iter().filter(|m| role_of(m) == "tool").collect();
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0]["content"], "first");
        assert!(
            paired
                .iter()
                .any(|m| m["content"].as_str().is_some_and(|c| c.contains("second"))),
            "重复的输出内容不丢，降级为文字"
        );
    }

    #[test]
    fn orphaned_output_becomes_user_text_without_an_id() {
        // 已被压缩掉的调用留下的输出，前面没有任何 assistant 调用。
        let paired = pair_tool_messages(vec![user("hi"), tool("gone", "stale result")]);
        assert_eq!(paired.len(), 1, "并入前一条 user 消息");
        let text = paired[0]["content"].as_str().unwrap();
        assert!(text.starts_with("hi\n\n"), "{text}");
        assert!(text.contains("stale result"));
        assert!(paired.iter().all(|m| role_of(m) != "tool"));
    }

    #[test]
    fn empty_id_and_late_result_are_orphans_not_answers() {
        let paired = pair_tool_messages(vec![
            assistant_calls(&["a"]),
            tool("", "no id"),
            tool("a", "1"),
            json!({ "role": "assistant", "content": "next" }),
            tool("a", "late duplicate of a closed call"),
        ]);
        let ids: Vec<_> = paired
            .iter()
            .filter(|m| role_of(m) == "tool")
            .map(|m| m["tool_call_id"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(ids, vec!["a"], "只剩一条真正的应答");
    }

    #[test]
    fn request_with_dirty_history_is_mended_end_to_end() {
        let (chat, _ctx) = request_to_chat(&json!({
            "model": "kimi",
            "input": [
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "start" }] },
                { "type": "function_call_output", "call_id": "compacted-away", "output": "stale" },
                { "type": "function_call", "call_id": "c1", "name": "run", "arguments": "{}" },
            ]
        }));
        let messages = chat["messages"].as_array().unwrap();
        // 孤立输出降级、没应答的 c1 补了占位：每个 tool 消息都对得上前一条 assistant 的调用。
        let mut open: Vec<String> = Vec::new();
        for m in messages {
            match role_of(m) {
                "assistant" => {
                    open = m["tool_calls"]
                        .as_array()
                        .map(|calls| {
                            calls
                                .iter()
                                .map(|c| c["id"].as_str().unwrap().to_owned())
                                .collect()
                        })
                        .unwrap_or_default();
                }
                "tool" => {
                    let id = m["tool_call_id"].as_str().unwrap();
                    assert!(open.iter().any(|o| o == id), "孤立的 tool 消息：{id}");
                    open.retain(|o| o != id);
                }
                _ => assert!(open.is_empty(), "有调用没应答：{open:?}"),
            }
        }
        assert!(open.is_empty(), "末尾有调用没应答：{open:?}");
    }

    fn codex_request() -> Value {
        json!({
            "model": "deepseek-v4",
            "instructions": "You are Codex.",
            "stream": true,
            "parallel_tool_calls": true,
            "tools": [
                { "type": "function", "name": "shell", "description": "run", "parameters": { "type": "object", "properties": { "cmd": { "type": "string" } } } },
                { "type": "custom", "name": "apply_patch", "format": { "type": "grammar" } },
                { "type": "namespace", "name": "mcp__fs", "tools": [
                    { "type": "function", "name": "read", "parameters": null }
                ] }
            ],
            "input": [
                { "type": "message", "role": "developer", "content": [{ "type": "input_text", "text": "Be terse." }] },
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "list files" }] },
                { "type": "reasoning", "summary": [{ "type": "summary_text", "text": "need ls" }] },
                { "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "Running ls." }] },
                { "type": "function_call", "call_id": "c1", "name": "shell", "arguments": "{\"cmd\":\"ls\"}" },
                { "type": "function_call_output", "call_id": "c1", "output": "a.txt" },
                { "type": "function_call", "call_id": "c2", "name": "read", "namespace": "mcp__fs", "arguments": "{}" },
                { "type": "function_call_output", "call_id": "c2", "output": [{ "type": "input_text", "text": "hello" }] }
            ]
        })
    }

    #[test]
    fn request_converts_messages_tools_and_reasoning() {
        let (chat, _) = request_to_chat(&codex_request());
        let messages = chat["messages"].as_array().unwrap();
        assert_eq!(
            messages[0],
            json!({ "role": "system", "content": "You are Codex.\n\nBe terse." })
        );
        assert_eq!(
            messages[1],
            json!({ "role": "user", "content": "list files" })
        );
        // 文本与同回合工具调用合并为一条 assistant，reasoning 前向附挂。
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["content"], "Running ls.");
        assert_eq!(messages[2]["reasoning_content"], "need ls");
        assert_eq!(messages[2]["tool_calls"][0]["function"]["name"], "shell");
        assert_eq!(
            messages[3],
            json!({ "role": "tool", "tool_call_id": "c1", "content": "a.txt" })
        );
        // namespace 工具拍平；思考模型所需的 reasoning_content 占位补齐。
        assert_eq!(
            messages[4]["tool_calls"][0]["function"]["name"],
            "mcp__fs__read"
        );
        assert_eq!(messages[4]["reasoning_content"], "tool call");
        assert_eq!(messages[5]["content"], "hello");

        let tools = chat["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[1]["function"]["parameters"]["required"][0], "input");
        assert_eq!(tools[2]["function"]["parameters"]["type"], "object");
        assert_eq!(chat["stream_options"]["include_usage"], true);
        assert_eq!(chat["parallel_tool_calls"], true);
    }

    #[test]
    fn request_forwards_reasoning_effort_for_supported_models_only() {
        let body = |model: &str| json!({ "model": model, "reasoning": { "effort": "xhigh" } });
        let (chat, _) = request_to_chat(&body("gpt-5.2"));
        assert_eq!(chat["reasoning_effort"], "xhigh");
        // DeepSeek/Kimi 等不认识 reasoning_effort，严格网关会因未知字段拒收。
        let (chat, _) = request_to_chat(&body("deepseek-v4"));
        assert!(chat.get("reasoning_effort").is_none());
    }

    #[test]
    fn request_max_tokens_field_depends_on_model() {
        let body = |model: &str| json!({ "model": model, "max_output_tokens": 100 });
        let (chat, _) = request_to_chat(&body("o3"));
        assert_eq!(chat["max_completion_tokens"], 100);
        assert!(chat.get("max_tokens").is_none());
        // Grok 4.5+ 也过 effort 门控，但仍是传统 max_tokens 网关。
        let (chat, _) = request_to_chat(&body("grok-4.5"));
        assert_eq!(chat["max_tokens"], 100);
        assert!(chat.get("max_completion_tokens").is_none());
        let (chat, _) = request_to_chat(&body("deepseek-v4"));
        assert_eq!(chat["max_tokens"], 100);
    }

    #[test]
    fn request_input_image_detail_merges_and_downgrades_original() {
        let body = json!({
            "model": "deepseek-v4",
            "input": [{ "type": "message", "role": "user", "content": [
                { "type": "input_image", "image_url": "https://x/a.png", "detail": "high" },
                { "type": "input_image", "image_url": { "url": "https://x/b.png" }, "detail": "original" }
            ] }]
        });
        let (chat, _) = request_to_chat(&body);
        let content = chat["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content[0]["image_url"]["detail"], "high");
        // 严格 Chat 网关会用 400 拒收 original，降级为 auto。
        assert_eq!(content[1]["image_url"]["detail"], "auto");
    }

    #[test]
    fn request_input_file_converts_to_chat_file() {
        let body = json!({
            "model": "deepseek-v4",
            "input": [{ "type": "message", "role": "user", "content": [
                { "type": "input_file", "file_data": "data:application/pdf;base64,AAA", "filename": "a.pdf" },
                { "type": "input_file" }
            ] }]
        });
        let (chat, _) = request_to_chat(&body);
        let content = chat["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "file");
        assert_eq!(
            content[0]["file"]["file_data"],
            "data:application/pdf;base64,AAA"
        );
        assert_eq!(content[0]["file"]["filename"], "a.pdf");
    }

    #[test]
    fn usage_to_responses_maps_cache_write_tokens() {
        let usage = json!({
            "prompt_tokens": 100, "completion_tokens": 10,
            "prompt_tokens_details": { "cached_tokens": 20, "cache_write_tokens": 30 }
        });
        let out = usage_to_responses(Some(&usage));
        assert_eq!(out["input_tokens_details"]["cached_tokens"], 20);
        assert_eq!(out["input_tokens_details"]["cache_write_tokens"], 30);
        // cache_creation_input_tokens 兜底（部分厂商用这个字段名）。
        let usage = json!({ "prompt_tokens": 5, "cache_creation_input_tokens": 7 });
        let out = usage_to_responses(Some(&usage));
        assert_eq!(out["input_tokens_details"]["cache_write_tokens"], 7);
        // 无缓存写入时不出现该字段，保持既有读缓存形态不变。
        let out = usage_to_responses(Some(&json!({ "prompt_tokens": 5 })));
        assert!(
            out["input_tokens_details"]
                .get("cache_write_tokens")
                .is_none()
        );
    }

    #[test]
    fn tool_output_images_move_to_synthetic_user_message_after_tool_messages() {
        let body = json!({
            "model": "deepseek-v4",
            "input": [
                { "type": "function_call", "call_id": "c1", "name": "shell", "arguments": "{}" },
                { "type": "function_call_output", "call_id": "c1", "output": [
                    { "type": "input_text", "text": "a.txt" },
                    { "type": "input_image", "image_url": "https://x/y.png" }
                ] },
                { "type": "function_call", "call_id": "c2", "name": "shell", "arguments": "{}" },
                { "type": "function_call_output", "call_id": "c2", "output": [
                    { "type": "input_text", "text": "no image" }
                ] },
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "next" }] }
            ]
        });
        let (chat, _) = request_to_chat(&body);
        let m = chat["messages"].as_array().unwrap();
        // 每轮 function_call → function_call_output 都拆成独立的
        // assistant/tool 对；c1 的图片在其 tool 消息后立即落地成合成 user
        // 消息（一旦遇到 c2 的 function_call，c1 的搬运图片就必须先收尾）。
        assert_eq!(m.len(), 6);
        assert_eq!(m[0]["tool_calls"][0]["id"], "c1");
        assert_eq!(
            m[1],
            json!({ "role": "tool", "tool_call_id": "c1", "content": "a.txt\n[图片见下一条消息]" })
        );
        assert_eq!(m[2]["role"], "user");
        let synth_content = m[2]["content"].as_array().unwrap();
        assert!(synth_content[0]["text"].as_str().unwrap().contains("c1"));
        assert_eq!(synth_content[1]["image_url"]["url"], "https://x/y.png");
        assert_eq!(m[3]["tool_calls"][0]["id"], "c2");
        assert_eq!(
            m[4],
            json!({ "role": "tool", "tool_call_id": "c2", "content": "no image" })
        );
        assert_eq!(m[5]["content"], "next");
    }

    #[test]
    fn non_stream_response_restores_tool_shapes() {
        let (_, ctx) = request_to_chat(&codex_request());
        let chat = json!({
            "id": "chatcmpl-1", "model": "deepseek-v4", "created": 7,
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": "<think>plan</think>Doing it.",
                    "tool_calls": [
                        { "id": "t1", "type": "function", "function": { "name": "mcp__fs__read", "arguments": "{\"p\":1}" } },
                        { "id": "t2", "type": "function", "function": { "name": "apply_patch", "arguments": "{\"input\":\"*** Begin Patch\"}" } }
                    ]
                }
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5, "prompt_tokens_details": { "cached_tokens": 4 } }
        });
        let resp = response_from_chat(&chat, &ctx).unwrap();
        assert_eq!(resp["id"], "resp_chatcmpl-1");
        assert_eq!(resp["status"], "completed");
        let output = resp["output"].as_array().unwrap();
        assert_eq!(output[0]["type"], "reasoning");
        assert_eq!(output[0]["summary"][0]["text"], "plan");
        assert_eq!(output[1]["content"][0]["text"], "Doing it.");
        assert_eq!(output[2]["type"], "function_call");
        assert_eq!(output[2]["name"], "read");
        assert_eq!(output[2]["namespace"], "mcp__fs");
        assert_eq!(output[3]["type"], "custom_tool_call");
        assert_eq!(output[3]["input"], "*** Begin Patch");
        assert_eq!(resp["usage"]["input_tokens"], 10);
        assert_eq!(resp["usage"]["input_tokens_details"]["cached_tokens"], 4);
        assert_eq!(resp["usage"]["total_tokens"], 15);
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
    async fn stream_converts_reasoning_text_tool_calls_and_usage() {
        let (_, ctx) = request_to_chat(&codex_request());
        let chunks = [
            r#"{"id":"x","model":"m","choices":[{"delta":{"reasoning_content":"think"}}]}"#,
            r#"{"id":"x","choices":[{"delta":{"content":"Hi"}}]}"#,
            r#"{"id":"x","choices":[{"delta":{"tool_calls":[{"index":0,"id":"t1","function":{"name":"shell","arguments":"{\"cmd\""}}]}}]}"#,
            r#"{"id":"x","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":":\"ls\"}"}}]},"finish_reason":"tool_calls"}]}"#,
            r#"{"id":"x","choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2}}"#,
        ];
        let mut raw = String::new();
        for chunk in chunks {
            raw.push_str(&format!("data: {chunk}\n\n"));
        }
        raw.push_str("data: [DONE]\n\n");
        // 故意在多字节字符与块边界中间切开。
        let bytes = raw.into_bytes();
        let (a, b) = bytes.split_at(37);
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
        assert!(types.contains(&"response.function_call_arguments.done"));
        let completed = events.last().unwrap();
        assert_eq!(completed["type"], "response.completed");
        let output = completed["response"]["output"].as_array().unwrap();
        assert_eq!(output[0]["type"], "reasoning");
        assert_eq!(output[1]["content"][0]["text"], "Hi");
        assert_eq!(output[2]["arguments"], "{\"cmd\":\"ls\"}");
        assert_eq!(output[2]["call_id"], "t1");
        assert_eq!(completed["response"]["usage"]["output_tokens"], 2);
    }

    #[tokio::test]
    async fn stream_inline_think_and_truncation() {
        let raw = "data: {\"choices\":[{\"delta\":{\"content\":\"<thi\"}}]}\n\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\"nk>deep</think>Answer\"}}]}\n\n";
        let upstream = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(raw))]);
        let out: Vec<Bytes> = convert_stream(upstream, ToolContext::default())
            .map(|r| r.unwrap())
            .collect()
            .await;
        let events = events_of(&out);
        let completed = events.last().unwrap();
        // 无 [DONE] 也无 finish_reason 但已有产出：按截断收尾。
        assert_eq!(completed["response"]["status"], "incomplete");
        let output = completed["response"]["output"].as_array().unwrap();
        assert_eq!(output[0]["summary"][0]["text"], "deep");
        assert_eq!(output[1]["content"][0]["text"], "Answer");

        let empty = futures_util::stream::iter(Vec::<Result<Bytes, std::io::Error>>::new());
        let out: Vec<Bytes> = convert_stream(empty, ToolContext::default())
            .map(|r| r.unwrap())
            .collect()
            .await;
        assert_eq!(events_of(&out).last().unwrap()["type"], "response.failed");
    }

    #[test]
    fn long_namespace_names_fit_chat_limit_and_stay_unique() {
        let a = flatten_name(&"n".repeat(60), "tool_one");
        let b = flatten_name(&"n".repeat(60), "tool_two");
        assert!(a.len() <= CHAT_TOOL_NAME_MAX_LEN && b.len() <= CHAT_TOOL_NAME_MAX_LEN);
        assert_ne!(a, b);
    }
}
