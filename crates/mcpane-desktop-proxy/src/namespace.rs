//! Codex namespace 工具在严格 Responses 网关上的拍平与还原（ADR-022 二期）。
//!
//! Codex CLI 0.142+ 把 MCP/插件工具声明成 Responses 私有扩展形态
//! `{"type":"namespace","name":"mcp__x__","tools":[...]}`（外加可能的
//! `tool_search`），OpenAI 自家后端认得，但部分严格的第三方 Responses 网关
//! （如 xAI）会用 `unknown variant "namespace"` 之类的 4xx 拒收。这里做
//! 原生 Responses 直通路径的请求侧拍平与响应侧（非流式/流式）还原；
//! Chat 路径的拍平复用同一套命名规则（`codex_chat::flatten_name`），
//! 两条路径互不感知彼此存在但产出的名字一致。

use std::collections::HashMap;

use axum::body::Bytes;
use futures_util::{Stream, StreamExt};
use serde_json::{Value, json};

/// 拍平名 → (原命名空间, 原名)。
#[derive(Debug, Clone, Default)]
pub(crate) struct NamespaceMap(HashMap<String, (String, String)>);

impl NamespaceMap {
    fn insert(&mut self, flat: String, namespace: String, name: String) {
        self.0.entry(flat).or_insert((namespace, name));
    }

    fn get(&self, flat: &str) -> Option<&(String, String)> {
        self.0.get(flat)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

// ---------------------------------------------------------------------------
// 拒收识别
// ---------------------------------------------------------------------------

/// 上游是否因 namespace / tool_search 工具形态拒收（据此拍平后重试）。
pub(crate) fn is_namespace_rejection(status: u16, body: &str) -> bool {
    if !(400..500).contains(&status) {
        return false;
    }
    let message = crate::media::extract_error_text(body).to_ascii_lowercase();
    if !(message.contains("namespace") || message.contains("tool_search")) {
        return false;
    }
    const HINTS: &[&str] = &[
        "unknown variant",
        "unknown type",
        "unknown field",
        "invalid type",
        "invalid value",
        "unrecognized",
        "not supported",
        "unsupported",
        "failed to deserialize",
    ];
    HINTS.iter().any(|hint| message.contains(hint))
}

// ---------------------------------------------------------------------------
// 请求侧拍平
// ---------------------------------------------------------------------------

/// 拍平请求：namespace 子工具展开为顶层 function（名字用
/// `codex_chat::flatten_name`），丢弃 tool_search 工具，同时把 input 历史里
/// 带 namespace 的 function_call 改成拍平名；返回还原表。
pub(crate) fn flatten_request(body: &mut Value) -> NamespaceMap {
    let mut map = NamespaceMap::default();
    let Some(tools) = body.get("tools").and_then(Value::as_array) else {
        return map;
    };
    let needs_flatten = tools.iter().any(|t| {
        matches!(
            t.get("type").and_then(Value::as_str),
            Some("namespace" | "tool_search")
        )
    });
    if !needs_flatten {
        return map;
    }

    let tools = tools.clone();
    let mut flattened = Vec::with_capacity(tools.len());
    for tool in tools {
        match tool.get("type").and_then(Value::as_str) {
            // 上游本就拒收这个形态，丢弃而非转换——没有顶层等价物。
            Some("tool_search") => continue,
            Some("namespace") => {
                let Some(namespace) = tool.get("name").and_then(Value::as_str).map(str::to_owned)
                else {
                    continue;
                };
                let children = tool
                    .get("tools")
                    .or_else(|| tool.get("children"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for mut child in children {
                    if child.get("type").and_then(Value::as_str) != Some("function") {
                        continue;
                    }
                    let Some(name) = child.get("name").and_then(Value::as_str).map(str::to_owned)
                    else {
                        continue;
                    };
                    let flat = crate::codex_chat::flatten_name(&namespace, &name);
                    map.insert(flat.clone(), namespace.clone(), name);
                    if let Some(obj) = child.as_object_mut() {
                        obj.insert("name".into(), json!(flat));
                    }
                    flattened.push(child);
                }
            }
            _ => flattened.push(tool),
        }
    }
    body["tools"] = json!(flattened);

    if let Some(input) = body.get_mut("input") {
        rewrite_history_calls(input, &map);
    }
    if let Some(choice) = body.get_mut("tool_choice") {
        if choice.get("type").and_then(Value::as_str) == Some("namespace") {
            *choice = json!("auto");
        } else {
            rewrite_call_object(choice, &map);
        }
    }
    map
}

fn rewrite_history_calls(value: &mut Value, map: &NamespaceMap) {
    match value {
        Value::Array(items) => {
            for item in items {
                rewrite_history_calls(item, map);
            }
        }
        Value::Object(obj) => {
            if obj.get("type").and_then(Value::as_str) == Some("function_call") {
                rewrite_call_object(value, map);
                return;
            }
            for child in obj.values_mut() {
                rewrite_history_calls(child, map);
            }
        }
        _ => {}
    }
}

/// 把 `{name, namespace}` 形态改成拍平名（要求两者与拍平表严格一致，
/// 避免误改同名但不同命名空间的调用）。
fn rewrite_call_object(item: &mut Value, map: &NamespaceMap) -> bool {
    let Some(obj) = item.as_object_mut() else {
        return false;
    };
    let namespace = obj
        .get("namespace")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let name = obj.get("name").and_then(Value::as_str).map(str::to_owned);
    let (Some(namespace), Some(name)) = (namespace, name) else {
        return false;
    };
    let flat = crate::codex_chat::flatten_name(&namespace, &name);
    match map.get(&flat) {
        Some((ns, nm)) if *ns == namespace && *nm == name => {
            obj.insert("name".into(), json!(flat));
            obj.remove("namespace");
            true
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// 响应侧还原
// ---------------------------------------------------------------------------

/// 非流式响应：把拍平名的 function_call 还原为 name + namespace。
pub(crate) fn restore_json(body: &mut Value, map: &NamespaceMap) {
    if map.is_empty() {
        return;
    }
    restore_value(body, map);
}

fn restore_value(value: &mut Value, map: &NamespaceMap) -> bool {
    let mut changed = false;
    match value {
        Value::Array(items) => {
            for item in items {
                changed |= restore_value(item, map);
            }
        }
        Value::Object(obj) => {
            if obj.get("type").and_then(Value::as_str) == Some("function_call")
                && let Some(flat) = obj.get("name").and_then(Value::as_str).map(str::to_owned)
                && let Some((ns, name)) = map.get(&flat)
            {
                obj.insert("name".into(), json!(name));
                obj.insert("namespace".into(), json!(ns));
                changed = true;
            }
            for child in obj.values_mut() {
                changed |= restore_value(child, map);
            }
        }
        _ => {}
    }
    changed
}

/// 流式响应：逐事件还原（`response.output_item.added`/`done` 的 item、
/// `response.completed` 的 output），其余字节原样透传。
pub(crate) fn restore_stream<S, E>(
    upstream: S,
    map: NamespaceMap,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send
where
    S: Stream<Item = Result<Bytes, E>> + Send + Unpin + 'static,
    E: std::fmt::Display + Send + 'static,
{
    struct State<S> {
        upstream: S,
        map: NamespaceMap,
        buffer: String,
        remainder: Vec<u8>,
        ended: bool,
    }
    let state = State {
        upstream,
        map,
        buffer: String::new(),
        remainder: Vec::new(),
        ended: false,
    };
    futures_util::stream::unfold(state, |mut st| async move {
        loop {
            if let Some(block) = take_block(&mut st.buffer) {
                let rewritten = rewrite_block(&block, &st.map);
                return Some((Ok(Bytes::from(rewritten)), st));
            }
            if st.ended {
                return None;
            }
            match st.upstream.next().await {
                Some(Ok(bytes)) => {
                    crate::sse::append_utf8(&mut st.buffer, &mut st.remainder, &bytes)
                }
                Some(Err(error)) => {
                    st.ended = true;
                    return Some((
                        Err(std::io::Error::other(format!("上游流中断：{error}"))),
                        st,
                    ));
                }
                None => {
                    st.ended = true;
                    // 上游未以空行收尾：把残留缓冲原样吐出，不吞尾部字节。
                    let tail = std::mem::take(&mut st.buffer);
                    if tail.is_empty() {
                        return None;
                    }
                    return Some((Ok(Bytes::from(tail)), st));
                }
            }
        }
    })
}

/// 从缓冲区取出下一个完整 SSE 块的原始文本（含分隔符）；不能直接复用
/// `sse::take_data`——它只回传合并后的 data 负载，丢弃了原始块文本与
/// `event:` 行，而这里要求未命中还原的块原样透传。
fn take_block(buffer: &mut String) -> Option<String> {
    let (pos, len) = [("\r\n\r\n", 4), ("\n\n", 2)]
        .into_iter()
        .filter_map(|(delim, len)| buffer.find(delim).map(|pos| (pos, len)))
        .min_by_key(|(pos, _)| *pos)?;
    let block = buffer[..pos + len].to_owned();
    buffer.drain(..pos + len);
    Some(block)
}

fn block_data_payload(block: &str) -> Option<String> {
    let data = block
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("\n");
    (!data.is_empty()).then_some(data)
}

/// 未命中还原的块原样返回；命中时只重写 `data:` 行，`event:` 等其余行不动。
fn rewrite_block(block: &str, map: &NamespaceMap) -> String {
    if map.is_empty() {
        return block.to_owned();
    }
    let Some(data) = block_data_payload(block) else {
        return block.to_owned();
    };
    let Ok(mut event) = serde_json::from_str::<Value>(&data) else {
        return block.to_owned();
    };
    if !restore_value(&mut event, map) {
        return block.to_owned();
    }
    let rewritten_data = event.to_string();
    let mut out = String::new();
    let mut data_written = false;
    for line in block.lines() {
        if line.starts_with("data:") {
            if !data_written {
                out.push_str("data: ");
                out.push_str(&rewritten_data);
                out.push('\n');
                data_written = true;
            }
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn namespace_request() -> Value {
        json!({
            "model": "grok-4.5",
            "tools": [
                { "type": "function", "name": "plain_tool", "parameters": {} },
                {
                    "type": "namespace",
                    "name": "mcp__files__",
                    "tools": [
                        { "type": "function", "name": "read", "description": "read a file", "parameters": {} },
                        { "type": "function", "name": "write", "parameters": {} }
                    ]
                },
                { "type": "tool_search" }
            ],
            "input": [
                {
                    "type": "function_call",
                    "name": "read",
                    "namespace": "mcp__files__",
                    "call_id": "c1",
                    "arguments": "{}"
                }
            ],
            "tool_choice": { "type": "namespace", "name": "mcp__files__" }
        })
    }

    #[test]
    fn is_namespace_rejection_matches_known_gateway_errors() {
        assert!(is_namespace_rejection(
            422,
            r#"{"error":{"message":"unknown variant `namespace`, expected one of `function`, `custom`"}}"#
        ));
        assert!(is_namespace_rejection(
            400,
            r#"{"message":"unsupported tool_search field"}"#
        ));
        assert!(!is_namespace_rejection(
            400,
            r#"{"error":{"message":"invalid api key"}}"#
        ));
        assert!(!is_namespace_rejection(
            500,
            r#"{"error":{"message":"unknown variant namespace"}}"#
        ));
    }

    #[test]
    fn flatten_lifts_namespace_children_drops_tool_search_and_rewrites_history() {
        let mut body = namespace_request();
        let map = flatten_request(&mut body);

        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3, "plain + read + write，tool_search 被丢弃");
        assert!(tools.iter().all(|t| t["type"] == "function"));
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"plain_tool"));
        assert!(names.contains(&"mcp__files____read"));
        assert!(names.contains(&"mcp__files____write"));
        let read = tools
            .iter()
            .find(|t| t["name"] == "mcp__files____read")
            .unwrap();
        assert_eq!(read["description"], "read a file");

        let call = &body["input"][0];
        assert_eq!(call["name"], "mcp__files____read");
        assert!(call.get("namespace").is_none());
        assert_eq!(call["call_id"], "c1");
        assert_eq!(body["tool_choice"], json!("auto"));

        assert!(!map.is_empty());
    }

    #[test]
    fn flatten_is_noop_without_namespace_or_tool_search_tools() {
        let mut body =
            json!({ "tools": [{ "type": "function", "name": "plain", "parameters": {} }] });
        let original = body.clone();
        let map = flatten_request(&mut body);
        assert!(map.is_empty());
        assert_eq!(body, original);
    }

    #[test]
    fn round_trip_flatten_then_restore_json_recovers_namespace() {
        let mut body = namespace_request();
        let map = flatten_request(&mut body);

        let mut response = json!({
            "output": [
                { "type": "function_call", "name": "mcp__files____read", "call_id": "c1", "arguments": "{}" },
                { "type": "function_call", "name": "plain_tool", "call_id": "c2" }
            ]
        });
        restore_json(&mut response, &map);
        assert_eq!(response["output"][0]["name"], "read");
        assert_eq!(response["output"][0]["namespace"], "mcp__files__");
        // 未映射的调用原样保留。
        assert_eq!(response["output"][1]["name"], "plain_tool");
        assert!(response["output"][1].get("namespace").is_none());
    }

    #[tokio::test]
    async fn restore_stream_rewrites_matching_events_and_keeps_others_byte_identical() {
        let mut body = namespace_request();
        let map = flatten_request(&mut body);

        let unrelated = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n";
        let matching = "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"name\":\"mcp__files____read\",\"call_id\":\"c1\"}}\n\n";
        let raw = format!("{unrelated}{matching}");
        let bytes = raw.into_bytes();
        // 故意在块边界中间切开，模拟上游分片。
        let (a, b) = bytes.split_at(30);
        let upstream = futures_util::stream::iter(vec![
            Ok::<_, std::io::Error>(Bytes::copy_from_slice(a)),
            Ok(Bytes::copy_from_slice(b)),
        ]);
        let out: Vec<Bytes> = restore_stream(upstream, map)
            .map(|r| r.unwrap())
            .collect()
            .await;
        assert_eq!(out.len(), 2);
        // 未命中还原的块必须逐字节保持原样。
        assert_eq!(out[0].as_ref(), unrelated.as_bytes());
        let rewritten = String::from_utf8(out[1].to_vec()).unwrap();
        assert!(rewritten.starts_with("event: response.output_item.done\n"));
        let data_line = rewritten.lines().find(|l| l.starts_with("data:")).unwrap();
        let event: Value = serde_json::from_str(data_line.trim_start_matches("data: ")).unwrap();
        assert_eq!(event["item"]["name"], "read");
        assert_eq!(event["item"]["namespace"], "mcp__files__");
    }

    #[tokio::test]
    async fn restore_stream_passes_through_when_map_is_empty() {
        let raw = "event: response.completed\ndata: {\"type\":\"response.completed\"}\n\n";
        let upstream = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(raw))]);
        let out: Vec<Bytes> = restore_stream(upstream, NamespaceMap::default())
            .map(|r| r.unwrap())
            .collect()
            .await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].as_ref(), raw.as_bytes());
    }

    #[test]
    fn long_namespace_names_stay_consistent_with_codex_chat_flatten_name() {
        let long_child = "a".repeat(80);
        let mut body = json!({
            "tools": [{
                "type": "namespace",
                "name": "mcp__srv__",
                "tools": [ { "type": "function", "name": long_child, "parameters": {} } ]
            }]
        });
        let map = flatten_request(&mut body);
        let flat_name = body["tools"][0]["name"].as_str().unwrap().to_owned();
        assert!(flat_name.len() <= 64);
        assert_eq!(
            crate::codex_chat::flatten_name("mcp__srv__", &long_child),
            flat_name
        );
        assert!(map.get(&flat_name).is_some());
    }
}
