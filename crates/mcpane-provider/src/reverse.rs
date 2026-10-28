//! 反解：从客户端配置文档还原 Provider 字段。
//!
//! 两个调用方共用同一套——首启收编 live 配置（[`crate::ProviderService`] 的
//! `import_live_as_provider`）与 cc-switch 迁移（`mcpane-ccswitch`）。后者存的
//! `settings_config` 就是客户端配置的整份快照，两边解析的是同一种文档，不该有
//! 两套字段名清单：漏一个键，用户看到的就是"导进来了但少了模型映射"。

use mcpane_config::ConfigDocument;

use crate::domain::{
    CODEX_CONTEXT_WINDOW_1M, ClaudeAuthField, CodexWireApi, ProviderProfile, UpstreamApi,
};

/// Claude settings.json 里被结构化字段吃掉的 env 键。
/// cc-switch 迁移侧据此挑出"剩下的"放进 extra_settings，不能两边各写一份。
pub const CLAUDE_MANAGED_ENV_KEYS: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_FABLE_MODEL",
    "CLAUDE_CODE_SUBAGENT_MODEL",
];

fn doc_str(doc: &ConfigDocument, path: &[&str]) -> Option<String> {
    doc.get(path)
        .and_then(|v| v.as_str().map(str::to_owned))
        .filter(|v| !v.is_empty())
}

fn json_u64(value: Option<serde_json::Value>) -> Option<u64> {
    value.and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_i64().and_then(|n| u64::try_from(n).ok()))
    })
}

/// Claude / Claude Desktop 的 settings.json：填 base_url 与模型映射，
/// 返回明文 Key（顺带按取到的是哪个键定 `api_key_field`）。
pub fn fill_from_claude(profile: &mut ProviderProfile, doc: &ConfigDocument) -> Option<String> {
    let env = |key: &str| doc_str(doc, &["env", key]);
    profile.base_url = env("ANTHROPIC_BASE_URL");
    profile.model = env("ANTHROPIC_MODEL");
    profile.model_haiku = env("ANTHROPIC_DEFAULT_HAIKU_MODEL");
    profile.model_sonnet = env("ANTHROPIC_DEFAULT_SONNET_MODEL");
    profile.model_opus = env("ANTHROPIC_DEFAULT_OPUS_MODEL");
    profile.model_fable = env("ANTHROPIC_DEFAULT_FABLE_MODEL");
    profile.model_subagent = env("CLAUDE_CODE_SUBAGENT_MODEL");
    if let Some(token) = env("ANTHROPIC_AUTH_TOKEN") {
        profile.api_key_field = ClaudeAuthField::AuthToken;
        Some(token)
    } else if let Some(token) = env("ANTHROPIC_API_KEY") {
        profile.api_key_field = ClaudeAuthField::ApiKey;
        Some(token)
    } else {
        None
    }
}

/// Codex（与借用 Codex TOML 载体的 Grok）的 config.toml：从 `model_provider`
/// 指向的那张表取 base_url / wire_api / query_params，顶层取 model 与 1M 窗口。
/// Key 住在 auth.json，由调用方给。
pub fn fill_from_codex_config(profile: &mut ProviderProfile, doc: &ConfigDocument) {
    let Some(slug) = doc_str(doc, &["model_provider"]) else {
        return;
    };
    profile.base_url = doc_str(doc, &["model_providers", &slug, "base_url"]);
    if doc_str(doc, &["model_providers", &slug, "wire_api"]).as_deref() == Some("chat") {
        profile.codex_wire_api = CodexWireApi::Chat;
    }
    profile.model = doc_str(doc, &["model"]);
    if let Some(serde_json::Value::Object(params)) =
        doc.get(&["model_providers", &slug, "query_params"])
    {
        profile.codex_query_params = params
            .into_iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_owned())))
            .collect();
    }
    apply_codex_context_window(profile, doc);
}

/// live 顶层 `model_context_window == 1000000` 才视为开启 1M。
pub fn apply_codex_context_window(profile: &mut ProviderProfile, doc: &ConfigDocument) {
    profile.codex_context_window_1m =
        json_u64(doc.get(&["model_context_window"])) == Some(CODEX_CONTEXT_WINDOW_1M);
    profile.codex_auto_compact_limit = profile
        .codex_context_window_1m
        .then(|| json_u64(doc.get(&["model_auto_compact_token_limit"])))
        .flatten()
        .filter(|&n| n > 0);
}

/// Gemini `.env`：填 base_url 与 model，返回 `GEMINI_API_KEY`。
pub fn fill_from_gemini_env(profile: &mut ProviderProfile, doc: &ConfigDocument) -> Option<String> {
    profile.base_url = doc_str(doc, &["GOOGLE_GEMINI_BASE_URL"]);
    profile.model = doc_str(doc, &["GEMINI_MODEL"]);
    doc_str(doc, &["GEMINI_API_KEY"])
}

/// additive 三家的上游协议：各写各的字面量，归一到同一个维度。
/// 认不出就返回 None（= OpenAI 兼容，两家的默认）。
pub fn upstream_api_from_str(value: &str) -> Option<UpstreamApi> {
    match value {
        "@ai-sdk/anthropic" | "anthropic-messages" | "anthropic_messages" => {
            Some(UpstreamApi::Anthropic)
        }
        _ => None,
    }
}
