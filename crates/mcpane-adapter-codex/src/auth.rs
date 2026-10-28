//! Codex `auth.json` 的判别与生成。
//!
//! `~/.codex/auth.json` 由 Codex 自己管理，两种形态：
//! - 官方订阅登录：`{"auth_mode": "chatgpt", "tokens": {...}, "OPENAI_API_KEY": null}`
//! - API Key（中转站）：`{"OPENAI_API_KEY": "sk-..."}`
//!
//! MCPane 切换中转站前把官方形态整份备份进 Secret Store，切回官方时恢复；
//! 该文件含 OAuth Token，禁止进入磁盘快照/操作历史。

use serde_json::Value;

/// `auth.json` 的内容形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind {
    /// 官方订阅登录（含 OAuth tokens）。
    Official,
    /// 仅 API Key（中转站形态，通常是 MCPane 写入的）。
    Relay,
    /// 无法识别的自定义内容。
    Unknown,
    /// 文件不存在。
    Missing,
}

/// 判别 `auth.json` 内容形态。`None` 表示文件不存在。
pub fn classify_auth(content: Option<&str>) -> AuthKind {
    let Some(content) = content else {
        return AuthKind::Missing;
    };
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(content) else {
        return AuthKind::Unknown;
    };
    let has_tokens = map.get("tokens").is_some_and(|v| !v.is_null());
    let is_chatgpt_mode = map
        .get("auth_mode")
        .and_then(Value::as_str)
        .is_some_and(|m| m.eq_ignore_ascii_case("chatgpt"));
    if has_tokens || is_chatgpt_mode {
        return AuthKind::Official;
    }
    let has_api_key = map.get("OPENAI_API_KEY").is_some_and(|v| v.is_string());
    // 仅 OPENAI_API_KEY（可附带 null 字段）视为中转形态；出现其他实义字段则不敢断言。
    let only_known_fields = map
        .iter()
        .all(|(k, v)| k == "OPENAI_API_KEY" || v.is_null());
    if has_api_key && only_known_fields {
        AuthKind::Relay
    } else {
        AuthKind::Unknown
    }
}

/// 生成中转站形态的 `auth.json` 内容（serde 构造，防注入）。
pub fn relay_auth_content(api_key: &str) -> String {
    serde_json::to_string_pretty(&serde_json::json!({ "OPENAI_API_KEY": api_key }))
        .unwrap_or_else(|_| String::from("{}"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn classify_official_by_tokens_or_auth_mode() {
        let by_tokens =
            r#"{"OPENAI_API_KEY": null, "tokens": {"id_token": "x", "access_token": "y"}}"#;
        assert_eq!(classify_auth(Some(by_tokens)), AuthKind::Official);
        let by_mode = r#"{"auth_mode": "chatgpt"}"#;
        assert_eq!(classify_auth(Some(by_mode)), AuthKind::Official);
    }

    #[test]
    fn classify_relay_and_missing() {
        assert_eq!(
            classify_auth(Some(r#"{"OPENAI_API_KEY": "sk-test"}"#)),
            AuthKind::Relay
        );
        assert_eq!(
            classify_auth(Some(r#"{"OPENAI_API_KEY": "sk-test", "tokens": null}"#)),
            AuthKind::Relay
        );
        assert_eq!(classify_auth(None), AuthKind::Missing);
    }

    #[test]
    fn classify_unknown_on_garbage_or_extra_fields() {
        assert_eq!(classify_auth(Some("not json")), AuthKind::Unknown);
        assert_eq!(
            classify_auth(Some(r#"{"custom": true}"#)),
            AuthKind::Unknown
        );
        assert_eq!(
            classify_auth(Some(r#"{"OPENAI_API_KEY": "sk-x", "custom": "y"}"#)),
            AuthKind::Unknown
        );
    }

    #[test]
    fn relay_content_escapes_special_chars() {
        let content = relay_auth_content(r#"sk-"quote\path"#);
        let parsed: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["OPENAI_API_KEY"], r#"sk-"quote\path"#);
        assert_eq!(classify_auth(Some(&content)), AuthKind::Relay);
    }
}
