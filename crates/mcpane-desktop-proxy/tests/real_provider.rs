//! 真实供应商回归套件（opt-in，默认忽略）：每条「客户端协议 × 上游协议」路径 ×
//! 流式/非流式 × 工具调用往返 × 故障转移，全部打真实上游。
//!
//! 运行（Key 只从环境变量读取，绝不入库）：
//! `MCPANE_REAL_KEY=sk-... cargo test -p mcpane-desktop-proxy --test real_provider -- --ignored --test-threads=1`
//! 必须串行：出网客户端是进程级共享连接池，而每个用例有自己的 tokio 运行时——
//! 并发时一个用例结束会带走别的用例正借用的连接（生产只有一个运行时，不存在此问题）。
//! 可选：`MCPANE_REAL_BASE`（默认 https://computetoken.ai，需同时提供 Chat / Responses /
//! Anthropic Messages / Gemini 原生四种接口）、`MCPANE_REAL_MODEL`（默认 gpt-5.6-sol）。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use serde_json::{Value, json};

use mcpane_desktop_proxy::{
    AppRoute, ModelSlots, ProxyServer, ProxyState, RouteUpstream, UpstreamAuth, UpstreamFormat,
};
use mcpane_provider::ClientApp;

const TOKEN: &str = "real-suite-token";

struct Env {
    key: String,
    root: String,
    model: String,
}

fn env() -> Env {
    Env {
        key: std::env::var("MCPANE_REAL_KEY").expect("需要 MCPANE_REAL_KEY"),
        root: std::env::var("MCPANE_REAL_BASE")
            .unwrap_or_else(|_| "https://computetoken.ai".into()),
        model: std::env::var("MCPANE_REAL_MODEL").unwrap_or_else(|_| "gpt-5.6-sol".into()),
    }
}

fn upstream(
    env: &Env,
    id: &str,
    base: String,
    auth: UpstreamAuth,
    format: UpstreamFormat,
) -> Arc<RouteUpstream> {
    Arc::new(RouteUpstream {
        provider_id: id.into(),
        name: id.into(),
        base_url: base,
        api_key: env.key.clone(),
        auth,
        models: ModelSlots::resolve(Some(env.model.clone()), None, None, None, None),
        format,
        headers: Vec::new(),
        body_override: None,
    })
}

async fn router(app: ClientApp, candidates: Vec<Arc<RouteUpstream>>) -> (ProxyServer, ProxyState) {
    let state = ProxyState::new(TOKEN);
    state.set_route(
        app,
        Some(AppRoute {
            candidates,
            ..AppRoute::default()
        }),
    );
    (ProxyServer::start(0, state.clone()).await.unwrap(), state)
}

async fn post(url: String, body: Value) -> (u16, String) {
    let resp = reqwest::Client::new()
        .post(url)
        .bearer_auth(TOKEN)
        .header("x-goog-api-key", TOKEN)
        .header("anthropic-version", "2023-06-01")
        .timeout(std::time::Duration::from_secs(180))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.text().await.unwrap())
}

/// SSE 文本 → data JSON 列表。
fn sse_events(text: &str) -> Vec<Value> {
    text.split("\n\n")
        .filter_map(|block| {
            let data: String = block
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(str::trim_start)
                .collect();
            serde_json::from_str(&data).ok()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Claude 客户端（Anthropic Messages）
// ---------------------------------------------------------------------------

fn claude_tools() -> Value {
    json!([{
        "name": "get_weather",
        "description": "Get the current weather for a city.",
        "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}
    }])
}

/// Anthropic 响应（非流式 JSON 或流式 SSE）→ (文本, tool_use 列表)。
fn claude_parse(text: &str, streaming: bool) -> (String, Vec<(String, String, Value)>) {
    if !streaming {
        let json: Value = serde_json::from_str(text).unwrap_or_else(|_| panic!("非 JSON：{text}"));
        let mut out = String::new();
        let mut tools = Vec::new();
        for block in json["content"]
            .as_array()
            .unwrap_or_else(|| panic!("缺 content：{json}"))
        {
            match block["type"].as_str() {
                Some("text") => out.push_str(block["text"].as_str().unwrap_or("")),
                Some("tool_use") => tools.push((
                    block["id"].as_str().unwrap().to_owned(),
                    block["name"].as_str().unwrap().to_owned(),
                    block["input"].clone(),
                )),
                _ => {}
            }
        }
        return (out, tools);
    }
    let events = sse_events(text);
    assert_eq!(
        events.first().map(|e| e["type"].clone()),
        Some(json!("message_start")),
        "{text}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "message_delta")
            .count(),
        1,
        "message_delta 必须恰好一次：{text}"
    );
    assert_eq!(
        events.last().map(|e| e["type"].clone()),
        Some(json!("message_stop")),
        "{text}"
    );
    let mut out = String::new();
    let mut tools: Vec<(String, String, String)> = Vec::new();
    for e in &events {
        match e["type"].as_str() {
            Some("content_block_start") if e["content_block"]["type"] == "tool_use" => {
                tools.push((
                    e["content_block"]["id"].as_str().unwrap().to_owned(),
                    e["content_block"]["name"].as_str().unwrap().to_owned(),
                    String::new(),
                ))
            }
            Some("content_block_delta") => match e["delta"]["type"].as_str() {
                Some("text_delta") => out.push_str(e["delta"]["text"].as_str().unwrap_or("")),
                Some("input_json_delta") => {
                    if let Some(last) = tools.last_mut() {
                        last.2
                            .push_str(e["delta"]["partial_json"].as_str().unwrap_or(""));
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    let tools = tools
        .into_iter()
        .map(|(id, name, args)| (id, name, serde_json::from_str(&args).unwrap_or(json!({}))))
        .collect();
    (out, tools)
}

/// 两轮：强制调用 get_weather → 回传工具结果 → 最终文本里带上结果。
async fn claude_tool_round_trip(port: u16, env: &Env, streaming: bool) {
    let url = format!("http://127.0.0.1:{port}/claude/v1/messages");
    let first = json!({
        "model": env.model, "max_tokens": 512, "stream": streaming,
        "system": "You are a terse assistant.",
        "tools": claude_tools(), "tool_choice": {"type": "tool", "name": "get_weather"},
        "messages": [{"role": "user", "content": "What's the weather in Paris?"}]
    });
    let (status, text) = post(url.clone(), first).await;
    assert_eq!(status, 200, "{text}");
    let (_, tools) = claude_parse(&text, streaming);
    let (id, name, input) = tools
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("未发起工具调用：{text}"));
    assert_eq!(name, "get_weather");
    assert!(
        input["city"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .contains("paris"),
        "{input}"
    );

    let second = json!({
        "model": env.model, "max_tokens": 512, "stream": streaming,
        "tools": claude_tools(),
        "messages": [
            {"role": "user", "content": "What's the weather in Paris?"},
            {"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": name, "input": input}]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": "Sunny, 23°C, code ZX-81"}]}
        ]
    });
    let (status, text) = post(url, second).await;
    assert_eq!(status, 200, "{text}");
    let (answer, _) = claude_parse(&text, streaming);
    assert!(
        answer.contains("23")
            || answer.contains("ZX-81")
            || answer.to_lowercase().contains("sunny"),
        "未用上工具结果：{answer}"
    );
}

async fn claude_suite(format: UpstreamFormat, base: String, auth: UpstreamAuth) {
    let env = env();
    let (server, state) = router(
        ClientApp::Claude,
        vec![upstream(&env, "real", base, auth, format)],
    )
    .await;
    for streaming in [false, true] {
        let (status, text) = post(
            format!("http://127.0.0.1:{}/claude/v1/messages", server.port()),
            json!({"model": env.model, "max_tokens": 64, "stream": streaming, "messages": [{"role": "user", "content": "Reply with exactly: pong"}]}),
        )
        .await;
        assert_eq!(status, 200, "{text}");
        let (answer, _) = claude_parse(&text, streaming);
        assert!(
            answer.to_lowercase().contains("pong"),
            "{format:?} stream={streaming}: {answer}"
        );
        claude_tool_round_trip(server.port(), &env, streaming).await;
    }
    // 用量旁路：每个请求都解析到了用量。
    let records = state.recent_requests(None, 20);
    assert!(
        records
            .iter()
            .all(|r| r.usage.is_some_and(|u| u.output_tokens > 0)),
        "{records:?}"
    );
    server.shutdown().await;
}

#[tokio::test]
#[ignore = "打真实上游，需 MCPANE_REAL_KEY"]
async fn claude_to_anthropic_native() {
    let env = env();
    claude_suite(
        UpstreamFormat::Native,
        env.root.clone(),
        UpstreamAuth::XApiKey,
    )
    .await;
}

#[tokio::test]
#[ignore = "打真实上游，需 MCPANE_REAL_KEY"]
async fn claude_to_openai_chat() {
    let env = env();
    claude_suite(
        UpstreamFormat::OpenaiChat,
        format!("{}/v1", env.root),
        UpstreamAuth::Bearer,
    )
    .await;
}

#[tokio::test]
#[ignore = "打真实上游，需 MCPANE_REAL_KEY"]
async fn claude_to_openai_responses() {
    let env = env();
    claude_suite(
        UpstreamFormat::OpenaiResponses,
        format!("{}/v1", env.root),
        UpstreamAuth::Bearer,
    )
    .await;
}

/// Claude Code 新版默认 `thinking: adaptive` + `/effort`：转成 reasoning effort 后上游应直接接受，
/// 不靠降档整流兜底（记录里没有整流经过）。
#[tokio::test]
#[ignore = "打真实上游，需 MCPANE_REAL_KEY"]
async fn claude_adaptive_effort_is_accepted_by_openai_upstreams() {
    let env = env();
    for format in [UpstreamFormat::OpenaiChat, UpstreamFormat::OpenaiResponses] {
        let (server, state) = router(
            ClientApp::Claude,
            vec![upstream(
                &env,
                "real",
                format!("{}/v1", env.root),
                UpstreamAuth::Bearer,
                format,
            )],
        )
        .await;
        for effort in ["max", "high"] {
            let (status, text) = post(
                format!("http://127.0.0.1:{}/claude/v1/messages", server.port()),
                json!({"model": env.model, "max_tokens": 2048,
                       "thinking": {"type": "adaptive"}, "output_config": {"effort": effort},
                       "messages": [{"role": "user", "content": "Reply with exactly: pong"}]}),
            )
            .await;
            assert_eq!(status, 200, "{format:?} effort={effort}: {text}");
            let (answer, _) = claude_parse(&text, false);
            assert!(
                answer.to_lowercase().contains("pong"),
                "{format:?}: {answer}"
            );
            let record = &state.recent_requests(None, 1)[0];
            assert!(
                record.attempts.is_empty(),
                "{format:?} effort={effort} 不应靠整流：{:?}",
                record.attempts
            );
        }
        server.shutdown().await;
    }
}

#[tokio::test]
#[ignore = "打真实上游，需 MCPANE_REAL_KEY"]
async fn claude_count_tokens_is_estimated_for_openai_upstreams() {
    let env = env();
    let (server, _) = router(
        ClientApp::Claude,
        vec![upstream(
            &env,
            "real",
            format!("{}/v1", env.root),
            UpstreamAuth::Bearer,
            UpstreamFormat::OpenaiChat,
        )],
    )
    .await;
    let (status, text) = post(
        format!(
            "http://127.0.0.1:{}/claude/v1/messages/count_tokens",
            server.port()
        ),
        json!({"model": env.model, "messages": [{"role": "user", "content": "hello"}]}),
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        serde_json::from_str::<Value>(&text).unwrap()["input_tokens"]
            .as_u64()
            .unwrap()
            > 0
    );
    server.shutdown().await;
}

// ---------------------------------------------------------------------------
// Codex 客户端（Responses）
// ---------------------------------------------------------------------------

fn codex_tools() -> Value {
    json!([
        {"type": "function", "name": "get_weather", "description": "Get the current weather for a city.",
         "parameters": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}},
        {"type": "namespace", "name": "mcp__demo__", "description": "Demo MCP server tools.", "tools": [
            {"type": "function", "name": "lookup", "description": "Look something up.",
             "parameters": {"type": "object", "properties": {"q": {"type": "string"}}}}
        ]}
    ])
}

/// Responses 响应 → (文本, function_call 列表)。流式取 response.completed 的完整 output。
fn codex_parse(text: &str, streaming: bool) -> (String, Vec<(String, String, String)>) {
    let response = if streaming {
        let events = sse_events(text);
        assert_eq!(
            events.first().map(|e| e["type"].clone()),
            Some(json!("response.created")),
            "{text}"
        );
        events
            .iter()
            .find(|e| e["type"] == "response.completed")
            .unwrap_or_else(|| panic!("缺 response.completed：{text}"))["response"]
            .clone()
    } else {
        serde_json::from_str::<Value>(text).unwrap_or_else(|_| panic!("非 JSON：{text}"))
    };
    let mut out = String::new();
    let mut calls = Vec::new();
    for item in response["output"]
        .as_array()
        .unwrap_or_else(|| panic!("缺 output：{response}"))
    {
        match item["type"].as_str() {
            Some("message") => {
                for part in item["content"].as_array().into_iter().flatten() {
                    out.push_str(part["text"].as_str().unwrap_or(""));
                }
            }
            Some("function_call") => calls.push((
                item["call_id"].as_str().unwrap().to_owned(),
                item["name"].as_str().unwrap().to_owned(),
                item["arguments"].as_str().unwrap_or("{}").to_owned(),
            )),
            _ => {}
        }
    }
    (out, calls)
}

async fn codex_tool_round_trip(port: u16, env: &Env, streaming: bool) {
    let url = format!("http://127.0.0.1:{port}/codex/responses");
    let user = json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "What's the weather in Paris? Use the tool."}]});
    let first = json!({
        "model": env.model, "stream": streaming, "instructions": "You are a terse assistant.",
        "input": [user], "tools": codex_tools(), "tool_choice": "required", "store": false
    });
    let (status, text) = post(url.clone(), first).await;
    assert_eq!(status, 200, "{text}");
    let (_, calls) = codex_parse(&text, streaming);
    let (call_id, name, arguments) = calls
        .iter()
        .find(|c| c.1 == "get_weather")
        .cloned()
        .unwrap_or_else(|| panic!("未调用 get_weather：{text}"));
    let second = json!({
        "model": env.model, "stream": streaming, "tools": codex_tools(), "store": false,
        "input": [
            user,
            {"type": "function_call", "call_id": call_id, "name": name, "arguments": arguments},
            {"type": "function_call_output", "call_id": call_id, "output": "Sunny, 23°C, code ZX-81"}
        ]
    });
    let (status, text) = post(url, second).await;
    assert_eq!(status, 200, "{text}");
    let (answer, _) = codex_parse(&text, streaming);
    assert!(
        answer.contains("23")
            || answer.contains("ZX-81")
            || answer.to_lowercase().contains("sunny"),
        "未用上工具结果：{answer}"
    );
}

async fn codex_suite(format: UpstreamFormat, base: String, auth: UpstreamAuth) {
    let env = env();
    let (server, state) = router(
        ClientApp::Codex,
        vec![upstream(&env, "real", base, auth, format)],
    )
    .await;
    for streaming in [false, true] {
        let (status, text) = post(
            format!("http://127.0.0.1:{}/codex/responses", server.port()),
            json!({"model": env.model, "stream": streaming, "store": false, "input": "Reply with exactly: pong"}),
        )
        .await;
        assert_eq!(status, 200, "{text}");
        let (answer, _) = codex_parse(&text, streaming);
        assert!(
            answer.to_lowercase().contains("pong"),
            "{format:?} stream={streaming}: {answer}"
        );
        codex_tool_round_trip(server.port(), &env, streaming).await;
    }
    let records = state.recent_requests(None, 20);
    assert!(
        records
            .iter()
            .all(|r| r.usage.is_some_and(|u| u.output_tokens > 0)),
        "{records:?}"
    );
    server.shutdown().await;
}

#[tokio::test]
#[ignore = "打真实上游，需 MCPANE_REAL_KEY"]
async fn codex_to_responses_native() {
    let env = env();
    codex_suite(
        UpstreamFormat::Native,
        format!("{}/v1", env.root),
        UpstreamAuth::Bearer,
    )
    .await;
}

#[tokio::test]
#[ignore = "打真实上游，需 MCPANE_REAL_KEY"]
async fn codex_to_openai_chat() {
    let env = env();
    codex_suite(
        UpstreamFormat::OpenaiChat,
        format!("{}/v1", env.root),
        UpstreamAuth::Bearer,
    )
    .await;
}

#[tokio::test]
#[ignore = "打真实上游，需 MCPANE_REAL_KEY"]
async fn codex_to_anthropic() {
    let env = env();
    codex_suite(
        UpstreamFormat::Anthropic,
        env.root.clone(),
        UpstreamAuth::XApiKey,
    )
    .await;
}

// ---------------------------------------------------------------------------
// Gemini / Grok 透传
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "打真实上游，需 MCPANE_REAL_KEY"]
async fn gemini_native_passthrough_stream_and_json() {
    let env = env();
    let (server, state) = router(
        ClientApp::Gemini,
        vec![upstream(
            &env,
            "real",
            env.root.clone(),
            UpstreamAuth::XGoogApiKey,
            UpstreamFormat::Native,
        )],
    )
    .await;
    let body =
        json!({"contents": [{"role": "user", "parts": [{"text": "Reply with exactly: pong"}]}]});
    let base = format!(
        "http://127.0.0.1:{}/gemini/v1beta/models/{}",
        server.port(),
        env.model
    );
    let (status, text) = post(format!("{base}:generateContent"), body.clone()).await;
    assert_eq!(status, 200, "{text}");
    assert!(text.to_lowercase().contains("pong"), "{text}");
    let (status, text) = post(format!("{base}:streamGenerateContent?alt=sse"), body).await;
    assert_eq!(status, 200, "{text}");
    assert!(
        sse_events(&text)
            .iter()
            .any(|e| e.to_string().to_lowercase().contains("pong")),
        "{text}"
    );
    assert!(
        state
            .recent_requests(None, 2)
            .iter()
            .all(|r| r.usage.is_some())
    );
    server.shutdown().await;
}

#[tokio::test]
#[ignore = "打真实上游，需 MCPANE_REAL_KEY"]
async fn grok_chat_passthrough_with_tool_call() {
    let env = env();
    let (server, _) = router(
        ClientApp::Grok,
        vec![upstream(
            &env,
            "real",
            format!("{}/v1", env.root),
            UpstreamAuth::Bearer,
            UpstreamFormat::Native,
        )],
    )
    .await;
    let (status, text) = post(
        format!("http://127.0.0.1:{}/grok/chat/completions", server.port()),
        json!({"model": env.model, "stream": true, "tool_choice": "required",
               "messages": [{"role": "user", "content": "Weather in Paris?"}],
               "tools": [{"type": "function", "function": {"name": "get_weather",
                          "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}}}]}),
    )
    .await;
    assert_eq!(status, 200, "{text}");
    assert!(text.contains("get_weather"), "{text}");
    server.shutdown().await;
}

// ---------------------------------------------------------------------------
// 故障转移
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "打真实上游，需 MCPANE_REAL_KEY"]
async fn failover_from_bad_key_and_dead_host_to_real_provider() {
    let env = env();
    let mut bad_key = (*upstream(
        &env,
        "bad-key",
        format!("{}/v1", env.root),
        UpstreamAuth::Bearer,
        UpstreamFormat::Native,
    ))
    .clone();
    bad_key.api_key = "sk-invalid".into();
    let (server, state) = router(
        ClientApp::Codex,
        vec![
            Arc::new(bad_key),
            upstream(
                &env,
                "dead",
                "http://127.0.0.1:9/v1".into(),
                UpstreamAuth::Bearer,
                UpstreamFormat::Native,
            ),
            upstream(
                &env,
                "real",
                format!("{}/v1", env.root),
                UpstreamAuth::Bearer,
                UpstreamFormat::OpenaiChat,
            ),
        ],
    )
    .await;
    for _ in 0..2 {
        let (status, text) = post(
            format!("http://127.0.0.1:{}/codex/responses", server.port()),
            json!({"model": env.model, "stream": true, "store": false, "input": "Reply with exactly: pong"}),
        )
        .await;
        assert_eq!(status, 200, "{text}");
    }
    let status = state.route_status();
    assert_eq!(status[0].last_served_by.as_deref(), Some("real"));
    assert_eq!(status[0].sticky.as_ref().unwrap().provider_id, "real");
    let records = state.recent_requests(None, 2);
    // 第一个请求经过坏 Key 与死主机；第二个请求直接由接替者服务。
    let first = &records[1];
    let outcomes: Vec<&str> = first.attempts.iter().map(|a| a.outcome.as_str()).collect();
    assert_eq!(outcomes, vec!["HTTP 401", "连接失败或超时"], "{first:?}");
    assert!(records[0].attempts.is_empty(), "{:?}", records[0]);
    server.shutdown().await;
}
