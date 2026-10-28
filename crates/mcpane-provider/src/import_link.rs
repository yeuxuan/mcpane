//! `mcpane://import/provider?...` 深链解析（ADR-023）。
//!
//! 链接来自网页/聊天等不可信来源：这里只做**严格校验并产出草稿**，不落库、不切换；
//! 落库必须经用户在界面确认。未知参数忽略，字段有长度上限，地址必须 https
//! （本机回环除外）且不带内嵌凭据。

use url::Url;

use crate::ClientApp;

/// 深链支持的客户端：`名称 + 地址 + Key + 模型` 足以描述一条可用中转站的那几个。
/// additive 型客户端与 Claude Desktop 需要额外结构化字段，暂不接。
const IMPORTABLE_APPS: [ClientApp; 4] = [
    ClientApp::Claude,
    ClientApp::Codex,
    ClientApp::Gemini,
    ClientApp::Grok,
];

const MAX_NAME_CHARS: usize = 64;
const MAX_URL_CHARS: usize = 2048;
const MAX_KEY_CHARS: usize = 512;
const MAX_MODEL_CHARS: usize = 128;

/// 解析出的导入草稿（尚未确认）。
#[derive(Clone, PartialEq, Eq)]
pub struct ImportDraft {
    pub app: ClientApp,
    pub name: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: Option<String>,
}

impl ImportDraft {
    /// 目标主机名（界面突出显示用）。`base_url` 已在解析时校验过，取不到只可能是空串。
    pub fn host(&self) -> String {
        Url::parse(&self.base_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default()
    }
}

// Key 不进 Debug 输出：草稿可能被 `{:?}` 打进日志。
impl std::fmt::Debug for ImportDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportDraft")
            .field("app", &self.app)
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("model", &self.model)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImportLinkError {
    #[error("不是 MCPane 的导入链接")]
    NotImportLink,
    #[error("暂不支持导入到该客户端：{0}")]
    UnsupportedApp(String),
    #[error("缺少必填参数：{0}")]
    Missing(&'static str),
    #[error("参数无效：{0}")]
    Invalid(&'static str),
}

/// 解析并校验一条深链。
pub fn parse_import_link(raw: &str) -> Result<ImportDraft, ImportLinkError> {
    let url = Url::parse(raw.trim()).map_err(|_| ImportLinkError::NotImportLink)?;
    if url.scheme() != "mcpane"
        || url.host_str() != Some("import")
        || url.path().trim_end_matches('/') != "/provider"
    {
        return Err(ImportLinkError::NotImportLink);
    }
    // 重复参数取第一个，避免后出现的同名参数悄悄覆盖前者。
    let param = |key: &str| {
        url.query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    };

    let app_raw = param("app").ok_or(ImportLinkError::Missing("app"))?;
    let app = IMPORTABLE_APPS
        .into_iter()
        .find(|app| app.as_str() == app_raw)
        .ok_or_else(|| ImportLinkError::UnsupportedApp(app_raw.chars().take(32).collect()))?;

    let name = param("name").ok_or(ImportLinkError::Missing("name"))?;
    check_text(&name, MAX_NAME_CHARS, "name")?;

    let base_url = param("baseUrl").ok_or(ImportLinkError::Missing("baseUrl"))?;
    check_base_url(&base_url)?;

    let api_key = param("apiKey");
    if let Some(key) = &api_key
        && (key.chars().count() > MAX_KEY_CHARS
            || key.chars().any(|c| c.is_whitespace() || c.is_control()))
    {
        return Err(ImportLinkError::Invalid("apiKey"));
    }

    let model = param("model");
    if let Some(model) = &model {
        check_text(model, MAX_MODEL_CHARS, "model")?;
    }

    Ok(ImportDraft {
        app,
        name,
        base_url,
        api_key,
        model,
    })
}

fn check_text(value: &str, max_chars: usize, field: &'static str) -> Result<(), ImportLinkError> {
    if value.chars().count() > max_chars || value.chars().any(char::is_control) {
        return Err(ImportLinkError::Invalid(field));
    }
    Ok(())
}

/// 地址必须 https（本机回环允许 http，给本地模型服务用）、有主机名、不带内嵌凭据。
fn check_base_url(raw: &str) -> Result<(), ImportLinkError> {
    const FIELD: &str = "baseUrl";
    if raw.chars().count() > MAX_URL_CHARS || raw.chars().any(|c| c.is_control() || c == ' ') {
        return Err(ImportLinkError::Invalid(FIELD));
    }
    let url = Url::parse(raw).map_err(|_| ImportLinkError::Invalid(FIELD))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ImportLinkError::Invalid(FIELD));
    }
    match url.scheme() {
        "https" if url.host_str().is_some() => Ok(()),
        "http" if is_loopback(&url) => Ok(()),
        _ => Err(ImportLinkError::Invalid(FIELD)),
    }
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => domain == "localhost",
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const OK: &str = "mcpane://import/provider?app=claude&name=%E4%B8%AD%E8%BD%AC%20A&baseUrl=https%3A%2F%2Frelay.example.com%2Fapi&apiKey=sk-abc123&model=glm-5";

    fn link(query: &str) -> Result<ImportDraft, ImportLinkError> {
        parse_import_link(&format!("mcpane://import/provider?{query}"))
    }

    #[test]
    fn parses_a_complete_link_with_percent_decoding() {
        let draft = parse_import_link(OK).unwrap();
        assert_eq!(draft.app, ClientApp::Claude);
        assert_eq!(draft.name, "中转 A");
        assert_eq!(draft.base_url, "https://relay.example.com/api");
        assert_eq!(draft.api_key.as_deref(), Some("sk-abc123"));
        assert_eq!(draft.model.as_deref(), Some("glm-5"));
    }

    #[test]
    fn key_and_model_are_optional_and_unknown_params_ignored() {
        let draft = link("app=codex&name=X&baseUrl=https://r.example.com/v1&utm=1").unwrap();
        assert_eq!(draft.app, ClientApp::Codex);
        assert!(draft.api_key.is_none() && draft.model.is_none());
    }

    #[test]
    fn first_duplicate_param_wins() {
        let draft =
            link("app=claude&name=first&name=second&baseUrl=https://r.example.com").unwrap();
        assert_eq!(draft.name, "first");
    }

    #[test]
    fn rejects_foreign_links() {
        for raw in [
            "https://import/provider?app=claude",
            "mcpane://other/provider?app=claude&name=x&baseUrl=https://r.example.com",
            "mcpane://import/elsewhere?app=claude&name=x&baseUrl=https://r.example.com",
            "not a url",
            "",
        ] {
            assert_eq!(
                parse_import_link(raw),
                Err(ImportLinkError::NotImportLink),
                "{raw}"
            );
        }
    }

    #[test]
    fn rejects_unsupported_or_missing_app_and_required_fields() {
        let base = "baseUrl=https://r.example.com";
        assert_eq!(
            link(&format!("app=opencode&name=x&{base}")),
            Err(ImportLinkError::UnsupportedApp("opencode".into()))
        );
        assert_eq!(
            link(&format!("name=x&{base}")),
            Err(ImportLinkError::Missing("app"))
        );
        assert_eq!(
            link(&format!("app=claude&{base}")),
            Err(ImportLinkError::Missing("name"))
        );
        assert_eq!(
            link("app=claude&name=x"),
            Err(ImportLinkError::Missing("baseUrl"))
        );
    }

    #[test]
    fn base_url_must_be_https_without_embedded_credentials() {
        let with_base = |base: &str| link(&format!("app=claude&name=x&baseUrl={base}"));
        assert!(with_base("https://r.example.com/v1").is_ok());
        for bad in [
            "http://r.example.com",
            "ftp://r.example.com",
            "https://user:pw@r.example.com",
            "https://user@r.example.com",
            "javascript:alert(1)",
            "https://",
            "r.example.com",
        ] {
            assert_eq!(
                with_base(bad),
                Err(ImportLinkError::Invalid("baseUrl")),
                "{bad}"
            );
        }
    }

    #[test]
    fn plain_http_is_allowed_only_on_loopback() {
        let with_base = |base: &str| link(&format!("app=claude&name=x&baseUrl={base}"));
        for ok in [
            "http://localhost:11434",
            "http://127.0.0.1:8080/v1",
            "http://[::1]:8080",
        ] {
            assert!(with_base(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://10.0.0.5",
            "http://192.168.1.2:8080",
            "http://evil.example",
        ] {
            assert!(with_base(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn fields_reject_control_chars_and_overlong_values() {
        let with = |query: &str| link(&format!("app=claude&baseUrl=https://r.example.com&{query}"));
        assert_eq!(with("name=a%0Ab"), Err(ImportLinkError::Invalid("name")));
        assert_eq!(
            with(&format!("name={}", "a".repeat(65))),
            Err(ImportLinkError::Invalid("name"))
        );
        assert_eq!(
            with("name=x&apiKey=sk%20with%20space"),
            Err(ImportLinkError::Invalid("apiKey"))
        );
        assert_eq!(
            with(&format!("name=x&apiKey={}", "k".repeat(513))),
            Err(ImportLinkError::Invalid("apiKey"))
        );
        assert_eq!(
            with(&format!("name=x&model={}", "m".repeat(129))),
            Err(ImportLinkError::Invalid("model"))
        );
    }

    #[test]
    fn debug_output_never_contains_the_key() {
        let draft = parse_import_link(OK).unwrap();
        let shown = format!("{draft:?}");
        assert!(!shown.contains("sk-abc123"), "{shown}");
        assert!(shown.contains("<redacted>"));
    }
}
