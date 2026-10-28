//! cc-switch 的 provider 记录 → MCPane 的 [`ProviderProfile`]。
//!
//! cc-switch 把整份客户端配置的快照塞在 `settings_config` 里，所以反解就是
//! "从客户端配置里认字段"——与首启收编 live 配置是同一件事，共用
//! `mcpane_provider::reverse` 的那套键名，不在这里另抄一份。

use mcpane_config::{ConfigDocument, ConfigFormat};
use mcpane_provider::{
    CLAUDE_MANAGED_ENV_KEYS, ClientApp, DesktopMode, DesktopRoute, ProviderProfile,
    fill_from_claude, fill_from_codex_config, fill_from_gemini_env, upstream_api_from_str,
};
use serde_json::Value;
use sqlx::Row;

use crate::CcSwitchData;

/// 一条可迁移的供应商。Key 是明文（cc-switch 就是明文存的），
/// 由调用方立刻转存进系统凭据库，不要落任何盘。
#[derive(Debug)]
pub struct ImportedProvider {
    pub profile: ProviderProfile,
    pub api_key: Option<String>,
    /// 在 cc-switch 故障转移队列里的名次（越小越靠前）；不在队列为 None。
    pub failover_rank: Option<i64>,
}

/// db 行与旧版 JSON 归一后的中间形态。
pub(crate) struct RawProvider {
    pub app_type: String,
    pub name: String,
    pub settings_config: Value,
    pub meta: Value,
    pub category: Option<String>,
    pub website_url: Option<String>,
    pub icon: Option<String>,
    pub notes: Option<String>,
    /// cc-switch 的 `in_failover_queue` + `sort_index`（旧 schema / 旧版 JSON 无此列）。
    pub failover_rank: Option<i64>,
}

/// 只认 MCPane 也有的客户端。`pi` 等没有对应客户端，认不出就跳过。
fn map_app(app_type: &str) -> Option<ClientApp> {
    Some(match app_type {
        "claude" => ClientApp::Claude,
        "claude-desktop" => ClientApp::ClaudeDesktop,
        "codex" => ClientApp::Codex,
        "gemini" => ClientApp::Gemini,
        // cc-switch 管 Grok CLI 叫 grokbuild。
        "grokbuild" => ClientApp::Grok,
        "opencode" => ClientApp::OpenCode,
        "openclaw" => ClientApp::OpenClaw,
        "hermes" => ClientApp::Hermes,
        _ => return None,
    })
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|v| !v.is_empty())
}

/// 从 sqlx 行取值；缺列（更早的 schema）按缺省处理，不因此整条丢弃。
pub(crate) fn raw_from_row(row: &sqlx::sqlite::SqliteRow) -> Option<RawProvider> {
    let app_type: String = row.try_get("app_type").ok()?;
    let name: String = row.try_get("name").ok()?;
    let settings_config: String = row.try_get("settings_config").ok()?;
    let meta: String = row.try_get("meta").unwrap_or_else(|_| "{}".into());
    Some(RawProvider {
        app_type,
        name,
        settings_config: serde_json::from_str(&settings_config).unwrap_or(Value::Null),
        meta: serde_json::from_str(&meta).unwrap_or(Value::Null),
        category: row.try_get("category").ok(),
        website_url: row.try_get("website_url").ok(),
        icon: row.try_get("icon").ok(),
        notes: row.try_get("notes").ok(),
        failover_rank: failover_rank(row),
    })
}

/// 队列名次：与 cc-switch `ORDER BY COALESCE(sort_index, 999999)` 同口径；
/// 列不存在（早期 schema）按不在队列处理。
fn failover_rank(row: &sqlx::sqlite::SqliteRow) -> Option<i64> {
    let queued = row
        .try_get::<bool, _>("in_failover_queue")
        .or_else(|_| row.try_get::<i64, _>("in_failover_queue").map(|v| v != 0))
        .unwrap_or(false);
    queued.then(|| {
        row.try_get::<Option<i64>, _>("sort_index")
            .ok()
            .flatten()
            .unwrap_or(999_999)
    })
}

/// 旧版 `config.json`：顶层按 app 名 flatten，每个 app 一份 `{providers, current}`。
pub(crate) fn raw_from_legacy(root: &Value) -> Vec<RawProvider> {
    let Some(obj) = root.as_object() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (app_type, section) in obj {
        let Some(providers) = section.get("providers").and_then(Value::as_object) else {
            continue;
        };
        for entry in providers.values() {
            let Some(name) = text(entry, "name") else {
                continue;
            };
            out.push(RawProvider {
                app_type: app_type.clone(),
                name,
                settings_config: entry.get("settingsConfig").cloned().unwrap_or(Value::Null),
                meta: entry.get("meta").cloned().unwrap_or(Value::Null),
                category: text(entry, "category"),
                website_url: text(entry, "websiteUrl"),
                icon: text(entry, "icon"),
                notes: text(entry, "notes"),
                failover_rank: None,
            });
        }
    }
    out
}

pub(crate) fn convert_all(raws: Vec<RawProvider>, data: &mut CcSwitchData) {
    let mut unsupported: Vec<String> = Vec::new();
    for raw in raws {
        let Some(app) = map_app(&raw.app_type) else {
            if !unsupported.contains(&raw.app_type) {
                unsupported.push(raw.app_type.clone());
                data.warnings.push(format!(
                    "cc-switch 中的「{}」：MCPane 暂不支持该工具，已跳过导入",
                    raw.app_type
                ));
            }
            continue;
        };
        // 官方订阅由 MCPane 自己按"装没装"播种，搬过来只会多一张切不动的卡片。
        if raw.category.as_deref() == Some("official") {
            continue;
        }
        match convert(app, &raw) {
            Ok(Some(imported)) => data.providers.push(imported),
            // 既没有 base_url 也没有 Key：这条本来就是官方形态或空壳。
            Ok(None) => {}
            Err(reason) => data.warnings.push(format!("{}：{reason}", raw.name)),
        }
    }
}

fn convert(app: ClientApp, raw: &RawProvider) -> Result<Option<ImportedProvider>, String> {
    let cfg = &raw.settings_config;
    let mut profile = ProviderProfile::new_relay(app, raw.name.clone());
    let key = match app {
        ClientApp::Claude => fill_claude_like(&mut profile, cfg)?,
        ClientApp::ClaudeDesktop => {
            // proxy 模式的 base_url 指向 cc-switch 自己的本机代理端口，
            // 搬过来就是一条死链——它的模型映射也依赖那个代理才成立。
            if raw.meta.get("claudeDesktopMode").and_then(Value::as_str) == Some("proxy") {
                return Err(
                    "该条目在 cc-switch 中使用本机代理模式，导入后无法使用，请在 MCPane 中重新配置"
                        .into(),
                );
            }
            let key = fill_claude_like(&mut profile, cfg)?;
            profile.desktop_mode = DesktopMode::Direct;
            profile.desktop_routes = desktop_routes(&raw.meta);
            key
        }
        // Grok 的记录借用 Codex 的 TOML 载体，两者反解同一套键。
        ClientApp::Codex | ClientApp::Grok => {
            let doc = codex_config_doc(cfg)?;
            fill_from_codex_config(&mut profile, &doc);
            cfg.get("auth")
                .and_then(|auth| text(auth, "OPENAI_API_KEY"))
        }
        ClientApp::Gemini => {
            let env = cfg.get("env").cloned().unwrap_or(Value::Null);
            let doc = json_doc(&env)?;
            fill_from_gemini_env(&mut profile, &doc)
        }
        ClientApp::OpenCode => {
            let options = cfg.get("options").cloned().unwrap_or(Value::Null);
            profile.base_url = text(&options, "baseURL");
            profile.upstream_api = cfg
                .get("npm")
                .and_then(Value::as_str)
                .and_then(upstream_api_from_str);
            profile.headers = string_map(options.get("headers"));
            profile.models = cfg
                .get("models")
                .and_then(Value::as_object)
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            text(&options, "apiKey")
        }
        ClientApp::OpenClaw => {
            profile.base_url = text(cfg, "baseUrl");
            profile.upstream_api = cfg
                .get("api")
                .and_then(Value::as_str)
                .and_then(upstream_api_from_str);
            profile.headers = string_map(cfg.get("headers"));
            profile.models = model_ids(cfg.get("models"));
            text(cfg, "apiKey")
        }
        ClientApp::Hermes => {
            profile.base_url = text(cfg, "base_url");
            profile.upstream_api = cfg
                .get("api_mode")
                .and_then(Value::as_str)
                .and_then(upstream_api_from_str);
            profile.models = model_ids(cfg.get("models"));
            profile.context_length = cfg
                .get("models")
                .and_then(Value::as_array)
                .and_then(|list| list.iter().find_map(|m| m.get("context_length")?.as_u64()));
            text(cfg, "api_key")
        }
    };

    if key.is_none() && profile.base_url.is_none() {
        return Ok(None);
    }
    profile.website_url = raw.website_url.clone();
    profile.icon = raw.icon.clone();
    profile.notes = raw.notes.clone();
    // cc-switch 的「本地代理请求覆盖」对应我们路由接管时的上游请求头 / 请求体覆盖。
    if app.supports_routing()
        && let Some(overrides) = raw.meta.get("localProxyRequestOverrides")
    {
        profile.headers.extend(string_map(overrides.get("headers")));
        if let Some(body) = overrides.get("body").filter(|b| b.is_object()) {
            profile.route_body_override = body.clone();
        }
    }
    Ok(Some(ImportedProvider {
        profile,
        api_key: key,
        failover_rank: raw.failover_rank,
    }))
}

/// Claude 与 Claude Desktop 的记录都是一份 settings.json 快照。
fn fill_claude_like(profile: &mut ProviderProfile, cfg: &Value) -> Result<Option<String>, String> {
    let doc = json_doc(cfg)?;
    let key = fill_from_claude(profile, &doc);
    profile.extra_settings = leftover_env(cfg);
    Ok(key)
}

/// settings.json 快照里除 env 之外还有 theme / language / permissions 这类**个人
/// 偏好**，不该跟着供应商走（`skipDangerousModePermissionPrompt` 更是不能悄悄搬）。
/// 只带 env 里没被结构化字段吃掉的那些键。
fn leftover_env(cfg: &Value) -> Value {
    let Some(env) = cfg.get("env").and_then(Value::as_object) else {
        return Value::Null;
    };
    let rest: serde_json::Map<String, Value> = env
        .iter()
        .filter(|(k, _)| !CLAUDE_MANAGED_ENV_KEYS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if rest.is_empty() {
        Value::Null
    } else {
        serde_json::json!({ "env": rest })
    }
}

/// cc-switch 的路由是 `routeId → {model, labelOverride, supports1m}` 的 map。
fn desktop_routes(meta: &Value) -> Vec<DesktopRoute> {
    let Some(routes) = meta
        .get("claudeDesktopModelRoutes")
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };
    routes
        .iter()
        .map(|(route_id, entry)| DesktopRoute {
            route_id: route_id.clone(),
            // 缺 model 就按"路由名即上游模型"处理，与 direct 模式的语义一致。
            upstream_model: text(entry, "model").unwrap_or_else(|| route_id.clone()),
            label_override: text(entry, "labelOverride"),
            supports_1m: entry
                .get("supports1m")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
        .collect()
}

/// Codex 记录的 `config` 通常是整份 config.toml 的文本；老记录里也可能已是对象。
fn codex_config_doc(cfg: &Value) -> Result<ConfigDocument, String> {
    match cfg.get("config") {
        Some(Value::String(toml)) => ConfigDocument::parse(toml, ConfigFormat::Toml)
            .map_err(|e| format!("config.toml 解析失败：{e}")),
        Some(value) => json_doc(value),
        None => json_doc(&Value::Null),
    }
}

fn json_doc(value: &Value) -> Result<ConfigDocument, String> {
    let text = if value.is_null() {
        "{}".to_string()
    } else {
        value.to_string()
    };
    ConfigDocument::parse(&text, ConfigFormat::Json).map_err(|e| format!("配置解析失败：{e}"))
}

fn string_map(value: Option<&Value>) -> std::collections::BTreeMap<String, String> {
    value
        .and_then(Value::as_object)
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

/// OpenClaw / Hermes 的模型是 `[{id, ...}]` 数组。
fn model_ids(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(|m| text(m, "id")).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use mcpane_provider::{ClaudeAuthField, CodexWireApi, UpstreamApi};

    fn raw(app_type: &str, cfg: Value) -> RawProvider {
        RawProvider {
            app_type: app_type.into(),
            name: "测试站".into(),
            settings_config: cfg,
            meta: Value::Null,
            category: None,
            website_url: Some("https://example.com".into()),
            icon: Some("kimi".into()),
            notes: None,
            failover_rank: None,
        }
    }

    fn convert_one(raw: RawProvider) -> ImportedProvider {
        let mut data = CcSwitchData::default();
        convert_all(vec![raw], &mut data);
        assert_eq!(data.warnings, Vec::<String>::new(), "不该有警告");
        data.providers.pop().expect("应导入一条")
    }

    #[test]
    fn claude_takes_structured_fields_and_keeps_leftover_env() {
        let imported = convert_one(raw(
            "claude",
            serde_json::json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "https://api.test",
                    "ANTHROPIC_AUTH_TOKEN": "sk-test",
                    "ANTHROPIC_MODEL": "glm-5",
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "glm-5-air",
                    "ANTHROPIC_REASONING_MODEL": "glm-5-think"
                },
                "theme": "dark",
                "skipDangerousModePermissionPrompt": true
            }),
        ));
        let p = &imported.profile;
        assert_eq!(p.base_url.as_deref(), Some("https://api.test"));
        assert_eq!(imported.api_key.as_deref(), Some("sk-test"));
        assert_eq!(p.model.as_deref(), Some("glm-5"));
        assert_eq!(p.model_haiku.as_deref(), Some("glm-5-air"));
        assert_eq!(p.api_key_field, ClaudeAuthField::AuthToken);
        assert_eq!(p.website_url.as_deref(), Some("https://example.com"));
        // 个人偏好不跟着供应商走，只留 env 里的剩余键。
        assert_eq!(
            p.extra_settings,
            serde_json::json!({ "env": { "ANTHROPIC_REASONING_MODEL": "glm-5-think" } })
        );
    }

    #[test]
    fn claude_detects_api_key_auth_field() {
        let imported = convert_one(raw(
            "claude",
            serde_json::json!({
                "env": { "ANTHROPIC_BASE_URL": "https://api.test", "ANTHROPIC_API_KEY": "sk-x" }
            }),
        ));
        assert_eq!(imported.profile.api_key_field, ClaudeAuthField::ApiKey);
        assert_eq!(imported.profile.extra_settings, Value::Null);
    }

    #[test]
    fn codex_reads_provider_table_from_toml_text() {
        let imported = convert_one(raw(
            "codex",
            serde_json::json!({
                "auth": { "OPENAI_API_KEY": "sk-codex" },
                "config": "model_provider = \"custom\"\nmodel = \"deepseek-v4\"\nmodel_context_window = 1000000\n\n[model_providers.custom]\nbase_url = \"https://codex.test/v1\"\nwire_api = \"chat\"\n\n[model_providers.custom.query_params]\napi-version = \"2025-04-01\"\n\n[projects.\"/tmp/x\"]\ntrust_level = \"trusted\"\n"
            }),
        ));
        let p = &imported.profile;
        assert_eq!(p.base_url.as_deref(), Some("https://codex.test/v1"));
        assert_eq!(p.model.as_deref(), Some("deepseek-v4"));
        assert_eq!(p.codex_wire_api, CodexWireApi::Chat);
        assert_eq!(imported.api_key.as_deref(), Some("sk-codex"));
        assert!(p.codex_context_window_1m);
        assert_eq!(
            p.codex_query_params.get("api-version").map(String::as_str),
            Some("2025-04-01")
        );
        // 项目信任配置是用户的本机状态，不该跟着供应商走。
        assert_eq!(p.extra_settings, Value::Null);
    }

    #[test]
    fn grokbuild_reuses_codex_toml_carrier() {
        let imported = convert_one(raw(
            "grokbuild",
            serde_json::json!({
                "auth": { "OPENAI_API_KEY": "sk-grok" },
                "config": "model_provider = \"custom\"\nmodel = \"grok-4.5\"\n\n[model_providers.custom]\nbase_url = \"https://grok.test/v1\"\nwire_api = \"responses\"\n"
            }),
        ));
        assert_eq!(imported.profile.app, ClientApp::Grok);
        assert_eq!(
            imported.profile.base_url.as_deref(),
            Some("https://grok.test/v1")
        );
        assert_eq!(imported.api_key.as_deref(), Some("sk-grok"));
    }

    #[test]
    fn gemini_reads_env_object() {
        let imported = convert_one(raw(
            "gemini",
            serde_json::json!({
                "env": {
                    "GOOGLE_GEMINI_BASE_URL": "https://gem.test",
                    "GEMINI_MODEL": "gemini-3.6-flash",
                    "GEMINI_API_KEY": "sk-gem"
                }
            }),
        ));
        assert_eq!(
            imported.profile.base_url.as_deref(),
            Some("https://gem.test")
        );
        assert_eq!(imported.profile.model.as_deref(), Some("gemini-3.6-flash"));
        assert_eq!(imported.api_key.as_deref(), Some("sk-gem"));
    }

    #[test]
    fn opencode_maps_npm_package_and_models() {
        let imported = convert_one(raw(
            "opencode",
            serde_json::json!({
                "npm": "@ai-sdk/anthropic",
                "name": "Kimi",
                "options": {
                    "baseURL": "https://oc.test/v1",
                    "apiKey": "sk-oc",
                    "headers": { "X-Route": "cn" }
                },
                "models": { "kimi-k3": { "name": "Kimi K3" } }
            }),
        ));
        let p = &imported.profile;
        assert_eq!(p.base_url.as_deref(), Some("https://oc.test/v1"));
        assert_eq!(p.upstream_api, Some(UpstreamApi::Anthropic));
        assert_eq!(p.models, vec!["kimi-k3".to_string()]);
        assert_eq!(p.headers.get("X-Route").map(String::as_str), Some("cn"));
        assert_eq!(imported.api_key.as_deref(), Some("sk-oc"));
    }

    #[test]
    fn openclaw_maps_api_field_and_model_array() {
        let imported = convert_one(raw(
            "openclaw",
            serde_json::json!({
                "baseUrl": "https://claw.test/v1",
                "apiKey": "sk-claw",
                "api": "openai-completions",
                "models": [{ "id": "kimi-k3", "name": "Kimi K3" }]
            }),
        ));
        let p = &imported.profile;
        assert_eq!(p.base_url.as_deref(), Some("https://claw.test/v1"));
        // openai-completions 就是默认形态，不写 upstream_api。
        assert_eq!(p.upstream_api, None);
        assert_eq!(p.models, vec!["kimi-k3".to_string()]);
    }

    #[test]
    fn hermes_carries_context_length() {
        let imported = convert_one(raw(
            "hermes",
            serde_json::json!({
                "name": "kimi",
                "base_url": "https://hermes.test/v1",
                "api_key": "sk-hermes",
                "api_mode": "anthropic_messages",
                "models": [{ "id": "kimi-k3", "context_length": 1048576 }]
            }),
        ));
        let p = &imported.profile;
        assert_eq!(p.upstream_api, Some(UpstreamApi::Anthropic));
        assert_eq!(p.context_length, Some(1048576));
    }

    #[test]
    fn claude_desktop_direct_keeps_routes_proxy_warns() {
        let mut direct = raw(
            "claude-desktop",
            serde_json::json!({
                "env": { "ANTHROPIC_BASE_URL": "https://d.test", "ANTHROPIC_AUTH_TOKEN": "sk-d" }
            }),
        );
        direct.meta = serde_json::json!({
            "claudeDesktopMode": "direct",
            "claudeDesktopModelRoutes": {
                "claude-sonnet-5": { "model": "glm-5", "labelOverride": "GLM", "supports1m": true }
            }
        });
        let imported = convert_one(direct);
        assert_eq!(imported.profile.desktop_mode, DesktopMode::Direct);
        let route = &imported.profile.desktop_routes[0];
        assert_eq!(route.route_id, "claude-sonnet-5");
        assert_eq!(route.upstream_model, "glm-5");
        assert!(route.supports_1m);

        let mut proxy = raw(
            "claude-desktop",
            serde_json::json!({ "env": { "ANTHROPIC_BASE_URL": "http://127.0.0.1:15721" } }),
        );
        proxy.meta = serde_json::json!({ "claudeDesktopMode": "proxy" });
        let mut data = CcSwitchData::default();
        convert_all(vec![proxy], &mut data);
        assert!(data.providers.is_empty());
        assert_eq!(data.warnings.len(), 1, "proxy 模式要给出原因而不是静默丢弃");
    }

    #[test]
    fn official_and_unsupported_apps_are_dropped() {
        let mut official = raw(
            "claude",
            serde_json::json!({ "env": { "ANTHROPIC_BASE_URL": "https://api.test" } }),
        );
        official.category = Some("official".into());
        let mut data = CcSwitchData::default();
        convert_all(
            vec![
                official,
                raw("pi", serde_json::json!({})),
                raw("pi", serde_json::json!({})),
            ],
            &mut data,
        );
        assert!(data.providers.is_empty());
        // 同一个不认识的客户端只提醒一次，不刷屏。
        assert_eq!(data.warnings.len(), 1);
    }

    #[test]
    fn empty_entries_are_skipped_without_warning() {
        let mut data = CcSwitchData::default();
        convert_all(
            vec![raw("claude", serde_json::json!({ "theme": "dark" }))],
            &mut data,
        );
        assert!(data.providers.is_empty());
        assert!(data.warnings.is_empty());
    }

    #[test]
    fn legacy_json_groups_by_app() {
        let root = serde_json::json!({
            "version": 2,
            "claude": {
                "current": "a",
                "providers": {
                    "a": {
                        "id": "a",
                        "name": "旧站",
                        "websiteUrl": "https://old.test",
                        "settingsConfig": {
                            "env": { "ANTHROPIC_BASE_URL": "https://old.test/api", "ANTHROPIC_AUTH_TOKEN": "sk-old" }
                        }
                    }
                }
            }
        });
        let raws = raw_from_legacy(&root);
        assert_eq!(raws.len(), 1);
        let mut data = CcSwitchData::default();
        convert_all(raws, &mut data);
        assert_eq!(data.providers.len(), 1);
        assert_eq!(
            data.providers[0].profile.base_url.as_deref(),
            Some("https://old.test/api")
        );
    }
}
