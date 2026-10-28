//! Provider Service 集成测试（v2：官方订阅 ↔ 中转站切换）。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;

use mcpane_config::history::JsonlHistoryStore;
use mcpane_config::snapshot::FsSnapshotStore;
use mcpane_config::{ConfigEngine, RealFs};
use mcpane_provider::{
    CODEX_AUTO_COMPACT_LIMIT_1M, CODEX_CONTEXT_WINDOW_1M, ClaudeAuthField, ClientApp, ClientPaths,
    JsonProviderStore, OFFICIAL_CLAUDE_ID, OFFICIAL_CODEX_ID, ProviderCategory, ProviderError,
    ProviderProfile, ProviderService,
};
use mcpane_secrets::{InMemorySecretStore, Secret, SecretReference, SecretStore};

type Service = ProviderService<JsonProviderStore, Arc<InMemorySecretStore>>;

const OFFICIAL_AUTH: &str = r#"{"OPENAI_API_KEY": null, "tokens": {"id_token": "official-id-token", "access_token": "official-access-token"}, "last_refresh": "2026-07-01T00:00:00Z"}"#;

struct Harness {
    _dir: tempfile::TempDir,
    svc: Service,
    secrets: Arc<InMemorySecretStore>,
    engine: ConfigEngine<FsSnapshotStore, JsonlHistoryStore, RealFs>,
    paths: ClientPaths,
    mcpane_home: std::path::PathBuf,
}

fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let mcpane_home = dir.path().join("mcpane-home");
    let secrets = Arc::new(InMemorySecretStore::default());
    let svc = ProviderService::new(JsonProviderStore::new(&mcpane_home), secrets.clone());
    let engine = ConfigEngine::new(
        FsSnapshotStore::new(&mcpane_home),
        JsonlHistoryStore::new(&mcpane_home),
        RealFs,
    );
    let paths = ClientPaths {
        claude_settings: dir.path().join("claude/settings.json"),
        codex_config: dir.path().join("codex/config.toml"),
        codex_auth: dir.path().join("codex/auth.json"),
        gemini_env: dir.path().join("gemini/.env"),
        gemini_settings: dir.path().join("gemini/settings.json"),
        grok_config: dir.path().join("grok/config.toml"),
        opencode_config: dir.path().join("opencode/opencode.json"),
        openclaw_config: dir.path().join("openclaw/openclaw.json"),
        hermes_config: dir.path().join("hermes/config.yaml"),
        claude_desktop: mcpane_provider::claude_desktop::paths_from_dirs(
            &dir.path().join("desktop/Claude"),
            &dir.path().join("desktop/Claude-3p"),
        ),
    };
    std::fs::create_dir_all(dir.path().join("claude")).unwrap();
    std::fs::create_dir_all(dir.path().join("codex")).unwrap();
    std::fs::create_dir_all(dir.path().join("gemini")).unwrap();
    // 官方项不再运行时合成，由首启播种落库。测试默认按"所有客户端都装了"播种，
    // 与真机首启后的状态一致；播种语义本身由 `official_seed_*` 系列测试覆盖。
    svc.seed_officials(&ClientApp::ALL).unwrap();
    Harness {
        _dir: dir,
        svc,
        secrets,
        engine,
        paths,
        mcpane_home,
    }
}

fn add_relay(svc: &Service, app: ClientApp, name: &str, base_url: &str) -> ProviderProfile {
    let mut p = ProviderProfile::new_relay(app, name);
    p.base_url = Some(base_url.into());
    svc.add_relay(p).unwrap()
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// 读取持久化的 ProviderState 原始 JSON（校验备份引用等内部状态）。
fn read_state(mcpane_home: &Path) -> serde_json::Value {
    read_json(&mcpane_home.join("providers.json"))
}

// ---------- CRUD ----------

#[test]
fn relay_crud_and_official_protection() {
    let h = harness();

    // list 空态：合成四个官方项，顺序即展示序（Claude 系相邻）。
    let listed = h.svc.list().unwrap();
    // 每个"有官方形态"的客户端一条官方项；OpenCode 这类 additive 客户端没有。
    assert_eq!(
        listed.len(),
        ClientApp::ALL
            .into_iter()
            .filter(|app| app.has_official())
            .count()
    );
    assert_eq!(listed[0].id, OFFICIAL_CLAUDE_ID);
    assert_eq!(listed[1].id, mcpane_provider::OFFICIAL_CLAUDE_DESKTOP_ID);
    assert_eq!(listed[2].id, OFFICIAL_CODEX_ID);

    // 新增：Codex 中转站自动生成 mcpane- 前缀 slug。
    let relay = add_relay(
        &h.svc,
        ClientApp::Codex,
        "My Relay",
        "https://relay.example.com/v1",
    );
    assert_eq!(relay.codex_slug.as_deref(), Some("mcpane-my-relay"));
    let relay2 = add_relay(
        &h.svc,
        ClientApp::Codex,
        "My Relay",
        "https://other.example.com/v1",
    );
    assert_eq!(
        relay2.codex_slug.as_deref(),
        Some("mcpane-my-relay-2"),
        "slug 去重"
    );

    // base_url 缺失被拒。
    let invalid = ProviderProfile::new_relay(ClientApp::Claude, "无地址");
    assert!(matches!(
        h.svc.add_relay(invalid),
        Err(ProviderError::InvalidOperation(_))
    ));

    // 更新：可编辑字段生效，slug 冻结。
    let mut edited = relay.clone();
    edited.name = "改名".into();
    edited.icon = Some("deepseek".into());
    edited.codex_slug = Some("mcpane-hacked".into());
    h.svc.update_relay(edited, &h.engine, &h.paths).unwrap();
    let after = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .find(|p| p.id == relay.id)
        .unwrap();
    assert_eq!(after.name, "改名");
    assert_eq!(after.icon.as_deref(), Some("deepseek"), "显式图标持久化");
    assert_eq!(
        after.codex_slug.as_deref(),
        Some("mcpane-my-relay"),
        "slug 不可改"
    );

    // 官方项可删（且删了不复活，见 official_seed_does_not_revive_deleted），
    // 但不能被当成中转站更新——update_relay 要求 base_url，官方项没有。
    let mut official_edit = ProviderProfile::official(ClientApp::Claude);
    official_edit.base_url = None;
    assert!(matches!(
        h.svc.update_relay(official_edit, &h.engine, &h.paths),
        Err(ProviderError::InvalidOperation(_))
    ));
    h.svc.delete(OFFICIAL_CLAUDE_ID).unwrap();
    assert!(
        !h.svc
            .list()
            .unwrap()
            .iter()
            .any(|p| p.id == OFFICIAL_CLAUDE_ID)
    );

    // 删除。
    h.svc.delete(&relay2.id).unwrap();
    assert!(matches!(
        h.svc.delete("nonexistent"),
        Err(ProviderError::NotFound(_))
    ));
}

#[test]
fn delete_rejects_in_use_and_cascades_secret() {
    let h = harness();
    std::fs::write(&h.paths.claude_settings, r#"{"env": {}}"#).unwrap();

    let relay = add_relay(&h.svc, ClientApp::Claude, "X", "https://relay.example.com");
    h.svc
        .set_api_key(&relay.id, &Secret::new("sk-abc"))
        .unwrap();
    h.svc
        .switch(ClientApp::Claude, &relay.id, &h.engine, &h.paths)
        .unwrap();

    // 使用中的供应商不可删除：直接删会让 live 配置残留指向。
    assert!(matches!(
        h.svc.delete(&relay.id),
        Err(ProviderError::InvalidOperation(_))
    ));

    let reference = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .find(|p| p.id == relay.id)
        .unwrap()
        .api_key_ref
        .unwrap();
    assert_eq!(h.secrets.get(&reference).unwrap().expose(), "sk-abc");

    // 切走后即可删除，且级联清理 Secret。
    h.svc
        .switch(ClientApp::Claude, OFFICIAL_CLAUDE_ID, &h.engine, &h.paths)
        .unwrap();
    h.svc.delete(&relay.id).unwrap();
    assert!(
        h.secrets.get(&reference).is_err(),
        "删除必须级联清理 Secret"
    );
}

// ---------- Claude 切换 ----------

#[test]
fn claude_relay_switch_writes_env_and_redacts_snapshot() {
    let h = harness();
    std::fs::write(
        &h.paths.claude_settings,
        r#"{"env": {"USER_VAR": "keep"}, "permissions": {"allow": ["Bash"]}}"#,
    )
    .unwrap();

    let mut p = ProviderProfile::new_relay(ClientApp::Claude, "中转 A");
    p.base_url = Some("https://relay-a.example.com".into());
    p.model = Some("claude-sonnet-5".into());
    let relay = h.svc.add_relay(p).unwrap();
    h.svc
        .set_api_key(&relay.id, &Secret::new("sk-real-api-key-value"))
        .unwrap();

    let outcome = h
        .svc
        .switch(ClientApp::Claude, &relay.id, &h.engine, &h.paths)
        .unwrap();
    assert!(outcome.written);

    let after = read_json(&h.paths.claude_settings);
    assert_eq!(
        after["env"]["ANTHROPIC_BASE_URL"],
        "https://relay-a.example.com"
    );
    assert_eq!(
        after["env"]["ANTHROPIC_AUTH_TOKEN"],
        "sk-real-api-key-value"
    );
    assert_eq!(after["env"]["ANTHROPIC_MODEL"], "claude-sonnet-5");
    assert_eq!(after["env"]["USER_VAR"], "keep", "用户 env 保留");
    assert_eq!(after["permissions"]["allow"][0], "Bash", "未知字段保留");

    // 快照脱敏：MCPane home 下任何落盘内容不得含 Key 明文。
    assert!(
        !dir_contains(&h.mcpane_home, "sk-real-api-key-value"),
        "快照/历史不得包含 API Key 明文"
    );
}

#[test]
fn claude_relay_api_key_field_writes_api_key_and_clears_auth_token() {
    let h = harness();
    // 上一个中转站用 AUTH_TOKEN 形态，切到 API_KEY 形态站点后必须清残留。
    std::fs::write(
        &h.paths.claude_settings,
        r#"{"env": {"ANTHROPIC_AUTH_TOKEN": "old-token", "ANTHROPIC_BASE_URL": "https://old.example.com"}}"#,
    )
    .unwrap();

    let mut p = ProviderProfile::new_relay(ClientApp::Claude, "x-api-key 站");
    p.base_url = Some("https://xkey.example.com".into());
    p.api_key_field = ClaudeAuthField::ApiKey;
    let relay = h.svc.add_relay(p).unwrap();
    h.svc
        .set_api_key(&relay.id, &Secret::new("sk-xkey-secret"))
        .unwrap();

    h.svc
        .switch(ClientApp::Claude, &relay.id, &h.engine, &h.paths)
        .unwrap();

    let after = read_json(&h.paths.claude_settings);
    assert_eq!(after["env"]["ANTHROPIC_API_KEY"], "sk-xkey-secret");
    assert!(
        after["env"].get("ANTHROPIC_AUTH_TOKEN").is_none(),
        "另一种认证键必须清除"
    );
    assert!(
        !dir_contains(&h.mcpane_home, "sk-xkey-secret"),
        "API_KEY 形态的新 Key 也不得进快照"
    );
    assert!(
        !dir_contains(&h.mcpane_home, "old-token"),
        "写前 live 里的旧 token 也不得进快照"
    );
}

#[test]
fn claude_relay_model_roles_write_and_clear() {
    let h = harness();
    std::fs::write(&h.paths.claude_settings, "{}").unwrap();

    let mut a = ProviderProfile::new_relay(ClientApp::Claude, "带角色映射");
    a.base_url = Some("https://a.example.com".into());
    a.model = Some("main-model".into());
    a.model_haiku = Some("small-model".into());
    a.model_sonnet = Some("mid-model".into());
    a.model_opus = Some("big-model".into());
    a.model_fable = Some("tale-model".into());
    a.model_subagent = Some("sub-model".into());
    let a = h.svc.add_relay(a).unwrap();
    h.svc.set_api_key(&a.id, &Secret::new("sk-a")).unwrap();

    h.svc
        .switch(ClientApp::Claude, &a.id, &h.engine, &h.paths)
        .unwrap();
    let after = read_json(&h.paths.claude_settings);
    assert_eq!(after["env"]["ANTHROPIC_MODEL"], "main-model");
    assert_eq!(after["env"]["ANTHROPIC_DEFAULT_HAIKU_MODEL"], "small-model");
    assert_eq!(after["env"]["ANTHROPIC_DEFAULT_SONNET_MODEL"], "mid-model");
    assert_eq!(after["env"]["ANTHROPIC_DEFAULT_OPUS_MODEL"], "big-model");
    assert_eq!(after["env"]["ANTHROPIC_DEFAULT_FABLE_MODEL"], "tale-model");
    assert_eq!(after["env"]["CLAUDE_CODE_SUBAGENT_MODEL"], "sub-model");

    // 切到无映射的站点：六个模型键全部清除；切回官方：受管键全清。
    let b = add_relay(&h.svc, ClientApp::Claude, "无映射", "https://b.example.com");
    h.svc.set_api_key(&b.id, &Secret::new("sk-b")).unwrap();
    h.svc
        .switch(ClientApp::Claude, &b.id, &h.engine, &h.paths)
        .unwrap();
    let after = read_json(&h.paths.claude_settings);
    for key in [
        "ANTHROPIC_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_FABLE_MODEL",
        "CLAUDE_CODE_SUBAGENT_MODEL",
    ] {
        assert!(after["env"].get(key).is_none(), "{key} 应被清除");
    }

    h.svc
        .switch(ClientApp::Claude, OFFICIAL_CLAUDE_ID, &h.engine, &h.paths)
        .unwrap();
    let after = read_json(&h.paths.claude_settings);
    assert!(after["env"].get("ANTHROPIC_BASE_URL").is_none());
    assert!(after["env"].get("ANTHROPIC_AUTH_TOKEN").is_none());
}

#[test]
fn claude_relay_only_main_model_fills_role_slots() {
    let h = harness();
    std::fs::write(&h.paths.claude_settings, "{}").unwrap();

    // 只配主模型、未配角色映射：haiku/sonnet/opus 应回落到主模型，
    // 避免后台/子代理流量以官方默认名 claude-haiku-* 打中转 404。
    let mut a = ProviderProfile::new_relay(ClientApp::Claude, "只填主模型");
    a.base_url = Some("https://a.example.com".into());
    a.model = Some("relay-model".into());
    let a = h.svc.add_relay(a).unwrap();
    h.svc.set_api_key(&a.id, &Secret::new("sk-a")).unwrap();
    h.svc
        .switch(ClientApp::Claude, &a.id, &h.engine, &h.paths)
        .unwrap();

    let env = &read_json(&h.paths.claude_settings)["env"];
    assert_eq!(env["ANTHROPIC_MODEL"], "relay-model");
    assert_eq!(
        env["ANTHROPIC_DEFAULT_HAIKU_MODEL"], "relay-model",
        "haiku 回落主模型"
    );
    assert_eq!(env["ANTHROPIC_DEFAULT_SONNET_MODEL"], "relay-model");
    assert_eq!(env["ANTHROPIC_DEFAULT_OPUS_MODEL"], "relay-model");
    assert_eq!(
        env["ANTHROPIC_DEFAULT_FABLE_MODEL"], "relay-model",
        "fable 同样回落——否则 fable 别名会以官方 claude-fable-5 打中转 404"
    );
    assert!(
        env.get("CLAUDE_CODE_SUBAGENT_MODEL").is_none(),
        "子代理槽不回落：留空才是让 Claude Code 走正常别名解析"
    );

    // 显式覆盖某一角色时以覆盖值为准，其余仍回落。
    let mut edited = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .find(|p| p.id == a.id)
        .unwrap();
    edited.model_haiku = Some("relay-haiku".into());
    h.svc.update_relay(edited, &h.engine, &h.paths).unwrap();
    let env = &read_json(&h.paths.claude_settings)["env"];
    assert_eq!(
        env["ANTHROPIC_DEFAULT_HAIKU_MODEL"], "relay-haiku",
        "显式覆盖优先"
    );
    assert_eq!(
        env["ANTHROPIC_DEFAULT_SONNET_MODEL"], "relay-model",
        "未覆盖仍回落"
    );
}

#[test]
fn claude_switch_clears_legacy_small_fast_model_key() {
    let h = harness();
    // 从旧工具迁移来的用户 live：残留 SMALL_FAST_MODEL 指向旧中转的模型名、
    // 以及 cc-switch 写的 *_MODEL_NAME 显示名（挂的是上一个供应商的模型），
    // 且有一个用户自设 env 键必须保留。
    std::fs::write(
        &h.paths.claude_settings,
        serde_json::json!({
            "env": {
                "ANTHROPIC_SMALL_FAST_MODEL": "old-relay-haiku",
                "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME": "旧站的 GLM",
                "ANTHROPIC_DEFAULT_FABLE_MODEL_NAME": "旧站的顶配",
                "USER_VAR": "keep"
            }
        })
        .to_string(),
    )
    .unwrap();

    let relay = add_relay(
        &h.svc,
        ClientApp::Claude,
        "新中转",
        "https://new.example.com",
    );
    h.svc
        .set_api_key(&relay.id, &Secret::new("sk-new"))
        .unwrap();

    // 切到中转站：遗留键必须被清除。
    h.svc
        .switch(ClientApp::Claude, &relay.id, &h.engine, &h.paths)
        .unwrap();
    let after = read_json(&h.paths.claude_settings);
    assert!(
        after["env"].get("ANTHROPIC_SMALL_FAST_MODEL").is_none(),
        "切到中转站必须清除遗留的 SMALL_FAST_MODEL"
    );
    for key in [
        "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
        "ANTHROPIC_DEFAULT_FABLE_MODEL_NAME",
    ] {
        assert!(
            after["env"].get(key).is_none(),
            "{key} 是绑在上一个供应商上的显示名，必须清除"
        );
    }
    assert_eq!(after["env"]["USER_VAR"], "keep", "用户自设键必须保留");

    // 再写回一次遗留键，验证切回官方也会清。
    std::fs::write(
        &h.paths.claude_settings,
        serde_json::json!({
            "env": {
                "ANTHROPIC_BASE_URL": "https://new.example.com",
                "ANTHROPIC_AUTH_TOKEN": "sk-new",
                "ANTHROPIC_SMALL_FAST_MODEL": "old-relay-haiku"
            }
        })
        .to_string(),
    )
    .unwrap();
    h.svc
        .switch(ClientApp::Claude, OFFICIAL_CLAUDE_ID, &h.engine, &h.paths)
        .unwrap();
    let after = read_json(&h.paths.claude_settings);
    assert!(
        after["env"].get("ANTHROPIC_SMALL_FAST_MODEL").is_none(),
        "切回官方也必须清除遗留的 SMALL_FAST_MODEL"
    );
}

/// 遗留键的**逃生口**：附加片段显式声明的值必须压过清理。
///
/// 这是 `LEGACY_ENV_KEYS` 的设计前提——它清的是"上一个供应商留下的残留"，
/// 不是"禁止用户使用这些键"。longcat 预设就靠这条写
/// `ANTHROPIC_SMALL_FAST_MODEL`，想自定义 `/model` 选择器标签的用户靠它写
/// `*_MODEL_NAME`。依赖的是 op 顺序：片段的 Set 排在遗留键 Remove 之后。
/// （受管键 `PROVIDER_ENV_KEYS` 则相反——片段里出现会被 `validate_fragment`
/// 直接拒绝，两套语义不能混。）
#[test]
fn fragment_can_deliberately_set_legacy_env_keys() {
    let h = harness();
    std::fs::write(&h.paths.claude_settings, "{}").unwrap();

    let mut profile = ProviderProfile::new_relay(ClientApp::Claude, "自带遗留键的预设");
    profile.base_url = Some("https://relay.example.com".into());
    profile.model = Some("LongCat-2.0".into());
    profile.extra_settings = serde_json::json!({
        "env": {
            "ANTHROPIC_SMALL_FAST_MODEL": "LongCat-2.0",
            "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME": "我的 LongCat"
        }
    });
    let relay = h.svc.add_relay(profile).unwrap();
    h.svc.set_api_key(&relay.id, &Secret::new("sk")).unwrap();
    h.svc
        .switch(ClientApp::Claude, &relay.id, &h.engine, &h.paths)
        .unwrap();

    let env = &read_json(&h.paths.claude_settings)["env"];
    assert_eq!(
        env["ANTHROPIC_SMALL_FAST_MODEL"], "LongCat-2.0",
        "片段显式声明的遗留键必须压过清理（longcat 预设依赖）"
    );
    assert_eq!(
        env["ANTHROPIC_DEFAULT_SONNET_MODEL_NAME"], "我的 LongCat",
        "自定义 /model 显示名同样要能显式保留"
    );
}

/// 受管键不能被片段偷渡：与上面那条逃生口相对的另一半语义。
/// 新加的 fable / subagent 两个槽也必须落在这道防线里，否则片段能绕过
/// "认证与模型由结构化字段统一管理"的前提。
#[test]
fn fragment_cannot_smuggle_managed_model_keys() {
    let h = harness();
    std::fs::write(&h.paths.claude_settings, "{}").unwrap();

    for key in [
        "ANTHROPIC_DEFAULT_FABLE_MODEL",
        "CLAUDE_CODE_SUBAGENT_MODEL",
        "ANTHROPIC_BASE_URL",
    ] {
        let mut profile = ProviderProfile::new_relay(ClientApp::Claude, "偷渡受管键");
        profile.base_url = Some("https://relay.example.com".into());
        profile.extra_settings = serde_json::json!({ "env": { key: "x" } });
        let error = h
            .svc
            .add_relay(profile)
            .expect_err(&format!("{key} 是受管键，片段里出现必须被拒"));
        assert!(
            error.to_string().contains(key),
            "错误信息要点明是哪个键：{error}"
        );
    }
}

#[test]
fn claude_official_switch_removes_managed_keys_only() {
    let h = harness();
    std::fs::write(&h.paths.claude_settings, r#"{"env": {"USER_VAR": "keep"}}"#).unwrap();

    let relay = add_relay(
        &h.svc,
        ClientApp::Claude,
        "中转 A",
        "https://relay-a.example.com",
    );
    h.svc
        .set_api_key(&relay.id, &Secret::new("sk-key"))
        .unwrap();
    h.svc
        .switch(ClientApp::Claude, &relay.id, &h.engine, &h.paths)
        .unwrap();

    let outcome = h
        .svc
        .switch(ClientApp::Claude, OFFICIAL_CLAUDE_ID, &h.engine, &h.paths)
        .unwrap();
    assert!(outcome.written);
    let after = read_json(&h.paths.claude_settings);
    assert!(after["env"].get("ANTHROPIC_BASE_URL").is_none());
    assert!(after["env"].get("ANTHROPIC_AUTH_TOKEN").is_none());
    assert!(after["env"].get("ANTHROPIC_API_KEY").is_none());
    assert!(after["env"].get("ANTHROPIC_MODEL").is_none());
    assert_eq!(after["env"]["USER_VAR"], "keep");
    // relay→official 的写前快照里有中转站 token，必须已脱敏。
    assert!(
        !dir_contains(&h.mcpane_home, "sk-key"),
        "切回官方的写前快照不得泄露中转站 Key"
    );
    assert_eq!(
        h.svc
            .current()
            .unwrap()
            .get(&ClientApp::Claude)
            .map(String::as_str),
        Some(OFFICIAL_CLAUDE_ID)
    );

    // 已是官方再切官方：幂等 no-op。
    let again = h
        .svc
        .switch(ClientApp::Claude, OFFICIAL_CLAUDE_ID, &h.engine, &h.paths)
        .unwrap();
    assert!(!again.written);
}

#[test]
fn claude_relay_to_relay_redacts_previous_token_in_snapshot() {
    let h = harness();
    std::fs::write(&h.paths.claude_settings, r#"{"env": {}}"#).unwrap();

    let relay_a = add_relay(&h.svc, ClientApp::Claude, "中转 A", "https://a.example.com");
    h.svc
        .set_api_key(&relay_a.id, &Secret::new("sk-old-relay-a-token"))
        .unwrap();
    let relay_b = add_relay(&h.svc, ClientApp::Claude, "中转 B", "https://b.example.com");
    h.svc
        .set_api_key(&relay_b.id, &Secret::new("sk-new-relay-b-token"))
        .unwrap();

    h.svc
        .switch(ClientApp::Claude, &relay_a.id, &h.engine, &h.paths)
        .unwrap();
    h.svc
        .switch(ClientApp::Claude, &relay_b.id, &h.engine, &h.paths)
        .unwrap();

    let after = read_json(&h.paths.claude_settings);
    assert_eq!(after["env"]["ANTHROPIC_AUTH_TOKEN"], "sk-new-relay-b-token");
    // relay→relay 的写前快照里是上一个中转站的旧 token，必须一并脱敏。
    assert!(
        !dir_contains(&h.mcpane_home, "sk-old-relay-a-token"),
        "写前快照不得泄露上一个中转站的 Key"
    );
    assert!(
        !dir_contains(&h.mcpane_home, "sk-new-relay-b-token"),
        "写前快照不得泄露新中转站的 Key"
    );
    // 反证快照确实生成且发生了脱敏（防止"没有快照"导致的假绿）。
    assert!(
        dir_contains(&h.mcpane_home, "[REDACTED:mcpane-api-key]"),
        "写前快照应存在且含脱敏占位符"
    );
}

// ---------- Codex 切换 ----------

#[test]
fn codex_relay_switch_backs_up_official_auth() {
    let h = harness();
    std::fs::write(&h.paths.codex_auth, OFFICIAL_AUTH).unwrap();
    std::fs::write(
        &h.paths.codex_config,
        "# 用户注释\nmodel = \"o3\"\n\n[mcp_servers.keep]\ncommand = \"npx\"\n",
    )
    .unwrap();

    let mut p = ProviderProfile::new_relay(ClientApp::Codex, "Relay A");
    p.base_url = Some("https://relay-a.example.com/v1".into());
    p.model = Some("gpt-5.2".into());
    let relay = h.svc.add_relay(p).unwrap();
    h.svc
        .set_api_key(&relay.id, &Secret::new("sk-relay-key"))
        .unwrap();

    let outcome = h
        .svc
        .switch(ClientApp::Codex, &relay.id, &h.engine, &h.paths)
        .unwrap();
    assert!(outcome.written);
    assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);

    // auth.json 变成 relay 形态。
    let auth = read_json(&h.paths.codex_auth);
    assert_eq!(auth["OPENAI_API_KEY"], "sk-relay-key");
    assert!(auth.get("tokens").is_none());

    // 官方内容整份进了 Secret Store（引用持久化在 state）。
    let state = read_state(&h.mcpane_home);
    let account = state["codexAuthBackup"]["account"].as_str().unwrap();
    let backup = h
        .secrets
        .get(&SecretReference {
            account: account.to_owned(),
        })
        .unwrap();
    assert_eq!(backup.expose(), OFFICIAL_AUTH);

    // config.toml：指向我们的表，注释与用户表保留，无明文 Key。
    let config = std::fs::read_to_string(&h.paths.codex_config).unwrap();
    assert!(config.contains("model_provider = \"mcpane-relay-a\""));
    assert!(config.contains("base_url = \"https://relay-a.example.com/v1\""));
    assert!(config.contains("wire_api = \"responses\""));
    assert!(
        config.contains("requires_openai_auth = true"),
        "必须路由认证到 auth.json 的 OPENAI_API_KEY"
    );
    assert!(
        config.contains("disable_response_storage = true"),
        "中转站必须关闭服务端响应存储"
    );
    assert!(
        !config.contains("model_context_window"),
        "1M 窗口默认关闭，不得写出"
    );
    assert!(config.contains("model = \"gpt-5.2\""));
    assert!(config.contains("# 用户注释"));
    assert!(config.contains("[mcp_servers.keep]"));
    assert!(!config.contains("sk-relay-key"), "TOML 不得出现明文 Key");

    // MCPane home 落盘内容不得含官方 Token 或 Key 明文。
    assert!(!dir_contains(&h.mcpane_home, "official-access-token"));
    assert!(!dir_contains(&h.mcpane_home, "sk-relay-key"));
}

#[test]
fn codex_relay_to_relay_keeps_official_backup_and_cleans_old_slug() {
    let h = harness();
    std::fs::write(&h.paths.codex_auth, OFFICIAL_AUTH).unwrap();
    std::fs::write(&h.paths.codex_config, "").unwrap();

    let relay_a = add_relay(
        &h.svc,
        ClientApp::Codex,
        "Relay A",
        "https://a.example.com/v1",
    );
    h.svc
        .set_api_key(&relay_a.id, &Secret::new("sk-key-a"))
        .unwrap();
    let relay_b = add_relay(
        &h.svc,
        ClientApp::Codex,
        "Relay B",
        "https://b.example.com/v1",
    );
    h.svc
        .set_api_key(&relay_b.id, &Secret::new("sk-key-b"))
        .unwrap();

    h.svc
        .switch(ClientApp::Codex, &relay_a.id, &h.engine, &h.paths)
        .unwrap();
    let backup_account = read_state(&h.mcpane_home)["codexAuthBackup"]["account"]
        .as_str()
        .unwrap()
        .to_owned();

    h.svc
        .switch(ClientApp::Codex, &relay_b.id, &h.engine, &h.paths)
        .unwrap();

    // 官方备份未被 relay 内容覆盖。
    let state = read_state(&h.mcpane_home);
    assert_eq!(state["codexAuthBackup"]["account"], backup_account.as_str());
    let backup = h
        .secrets
        .get(&SecretReference {
            account: backup_account,
        })
        .unwrap();
    assert_eq!(
        backup.expose(),
        OFFICIAL_AUTH,
        "relay→relay 不得覆盖官方备份"
    );

    // auth.json 换成新 Key；config.toml 旧表被清、新表生效。
    assert_eq!(read_json(&h.paths.codex_auth)["OPENAI_API_KEY"], "sk-key-b");
    let config = std::fs::read_to_string(&h.paths.codex_config).unwrap();
    assert!(config.contains("model_provider = \"mcpane-relay-b\""));
    assert!(!config.contains("mcpane-relay-a"), "旧中转站的表必须清理");
}

#[test]
fn codex_official_restores_auth_and_cleans_config() {
    let h = harness();
    std::fs::write(&h.paths.codex_auth, OFFICIAL_AUTH).unwrap();
    std::fs::write(
        &h.paths.codex_config,
        "[model_providers.user-own]\nname = \"用户自建\"\nbase_url = \"https://own.example.com\"\n",
    )
    .unwrap();

    let relay = add_relay(
        &h.svc,
        ClientApp::Codex,
        "Relay A",
        "https://a.example.com/v1",
    );
    h.svc
        .set_api_key(&relay.id, &Secret::new("sk-key-a"))
        .unwrap();
    h.svc
        .switch(ClientApp::Codex, &relay.id, &h.engine, &h.paths)
        .unwrap();

    let outcome = h
        .svc
        .switch(ClientApp::Codex, OFFICIAL_CODEX_ID, &h.engine, &h.paths)
        .unwrap();
    assert!(outcome.written);
    assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);

    // auth.json 恢复为官方内容，备份删除。
    assert_eq!(
        std::fs::read_to_string(&h.paths.codex_auth).unwrap(),
        OFFICIAL_AUTH
    );
    let state = read_state(&h.mcpane_home);
    assert!(state["codexAuthBackup"].is_null(), "恢复后备份引用必须清除");

    // config.toml：我们的痕迹清除，用户自建表保留。
    let config = std::fs::read_to_string(&h.paths.codex_config).unwrap();
    assert!(!config.contains("model_provider = \"mcpane-relay-a\""));
    assert!(!config.contains("[model_providers.mcpane-relay-a]"));
    assert!(
        !config.contains("disable_response_storage"),
        "切回官方必须清掉响应存储开关"
    );
    assert!(
        config.contains("[model_providers.user-own]"),
        "用户自建表保留"
    );
    assert_eq!(
        h.svc
            .current()
            .unwrap()
            .get(&ClientApp::Codex)
            .map(String::as_str),
        Some(OFFICIAL_CODEX_ID)
    );
}

#[test]
fn codex_1m_window_writes_clears_across_switch_and_update() {
    let h = harness();
    std::fs::write(&h.paths.codex_auth, OFFICIAL_AUTH).unwrap();
    std::fs::write(&h.paths.codex_config, "").unwrap();

    let mut on = ProviderProfile::new_relay(ClientApp::Codex, "1M On");
    on.base_url = Some("https://on.example.com/v1".into());
    on.codex_context_window_1m = true;
    on.codex_auto_compact_limit = Some(800_000);
    let on = h.svc.add_relay(on).unwrap();
    h.svc.set_api_key(&on.id, &Secret::new("sk-on")).unwrap();

    let mut off = ProviderProfile::new_relay(ClientApp::Codex, "1M Off");
    off.base_url = Some("https://off.example.com/v1".into());
    let off = h.svc.add_relay(off).unwrap();
    h.svc.set_api_key(&off.id, &Secret::new("sk-off")).unwrap();

    h.svc
        .switch(ClientApp::Codex, &on.id, &h.engine, &h.paths)
        .unwrap();
    let config = std::fs::read_to_string(&h.paths.codex_config).unwrap();
    assert!(
        config.contains(&format!("model_context_window = {CODEX_CONTEXT_WINDOW_1M}")),
        "{config}"
    );
    assert!(
        config.contains("model_auto_compact_token_limit = 800000"),
        "{config}"
    );

    // 切到未开 1M 的卡：两个键都消失。
    h.svc
        .switch(ClientApp::Codex, &off.id, &h.engine, &h.paths)
        .unwrap();
    let config = std::fs::read_to_string(&h.paths.codex_config).unwrap();
    assert!(!config.contains("model_context_window"), "{config}");
    assert!(
        !config.contains("model_auto_compact_token_limit"),
        "{config}"
    );

    // 再切回 1M 卡后切官方：官方也不留窗口键。
    h.svc
        .switch(ClientApp::Codex, &on.id, &h.engine, &h.paths)
        .unwrap();
    h.svc
        .switch(ClientApp::Codex, OFFICIAL_CODEX_ID, &h.engine, &h.paths)
        .unwrap();
    let config = std::fs::read_to_string(&h.paths.codex_config).unwrap();
    assert!(!config.contains("model_context_window"), "{config}");
    assert!(
        !config.contains("model_auto_compact_token_limit"),
        "{config}"
    );
}

#[test]
fn update_relay_turns_off_1m_window_on_current_codex() {
    let h = harness();
    std::fs::write(&h.paths.codex_auth, OFFICIAL_AUTH).unwrap();
    std::fs::write(&h.paths.codex_config, "").unwrap();

    let mut relay = ProviderProfile::new_relay(ClientApp::Codex, "Relay A");
    relay.base_url = Some("https://a.example.com/v1".into());
    relay.codex_context_window_1m = true;
    let relay = h.svc.add_relay(relay).unwrap();
    h.svc.set_api_key(&relay.id, &Secret::new("sk-a")).unwrap();
    h.svc
        .switch(ClientApp::Codex, &relay.id, &h.engine, &h.paths)
        .unwrap();
    let config = std::fs::read_to_string(&h.paths.codex_config).unwrap();
    assert!(
        config.contains(&format!("model_context_window = {CODEX_CONTEXT_WINDOW_1M}")),
        "{config}"
    );
    assert!(
        config.contains(&format!(
            "model_auto_compact_token_limit = {CODEX_AUTO_COMPACT_LIMIT_1M}"
        )),
        "{config}"
    );

    // 同一 id 关掉 1M：reapply 必须真删 live 键（PreviousRelay.model 在同 id 时是 None）。
    let mut edited = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .find(|p| p.id == relay.id)
        .unwrap();
    edited.codex_context_window_1m = false;
    edited.codex_auto_compact_limit = None;
    h.svc.update_relay(edited, &h.engine, &h.paths).unwrap();
    let config = std::fs::read_to_string(&h.paths.codex_config).unwrap();
    assert!(!config.contains("model_context_window"), "{config}");
    assert!(
        !config.contains("model_auto_compact_token_limit"),
        "{config}"
    );
}

#[test]
fn extra_settings_rejects_codex_1m_window_keys() {
    let h = harness();
    let mut window = ProviderProfile::new_relay(ClientApp::Codex, "Bad window");
    window.base_url = Some("https://bad.example.com/v1".into());
    window.extra_settings = serde_json::json!({"model_context_window": 1_000_000});
    assert!(matches!(
        h.svc.add_relay(window),
        Err(ProviderError::InvalidOperation(_))
    ));

    let mut compact = ProviderProfile::new_relay(ClientApp::Codex, "Bad compact");
    compact.base_url = Some("https://bad.example.com/v1".into());
    compact.extra_settings = serde_json::json!({"model_auto_compact_token_limit": 900_000});
    assert!(matches!(
        h.svc.add_relay(compact),
        Err(ProviderError::InvalidOperation(_))
    ));
}

#[test]
fn backfill_reads_codex_1m_window_from_live() {
    let h = harness();
    std::fs::write(&h.paths.codex_auth, OFFICIAL_AUTH).unwrap();
    std::fs::write(&h.paths.codex_config, "").unwrap();

    let mut relay = ProviderProfile::new_relay(ClientApp::Codex, "Relay A");
    relay.base_url = Some("https://a.example.com/v1".into());
    relay.codex_context_window_1m = true;
    let relay = h.svc.add_relay(relay).unwrap();
    h.svc.set_api_key(&relay.id, &Secret::new("sk-a")).unwrap();
    h.svc
        .switch(ClientApp::Codex, &relay.id, &h.engine, &h.paths)
        .unwrap();

    // 用户手改 compact 阈值；切走时 backfill 必须带走。
    let mut config = std::fs::read_to_string(&h.paths.codex_config).unwrap();
    config = config.replace(
        &format!("model_auto_compact_token_limit = {CODEX_AUTO_COMPACT_LIMIT_1M}"),
        "model_auto_compact_token_limit = 850000",
    );
    std::fs::write(&h.paths.codex_config, config).unwrap();

    h.svc
        .switch(ClientApp::Codex, OFFICIAL_CODEX_ID, &h.engine, &h.paths)
        .unwrap();
    let after = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .find(|p| p.id == relay.id)
        .unwrap();
    assert!(after.codex_context_window_1m);
    assert_eq!(after.codex_auto_compact_limit, Some(850_000));
}

#[test]
fn codex_edge_cases_warn_instead_of_fail() {
    let h = harness();
    std::fs::write(&h.paths.codex_config, "").unwrap();
    // 场景 1：无 auth.json 切 relay → warning。
    let relay = add_relay(
        &h.svc,
        ClientApp::Codex,
        "Relay A",
        "https://a.example.com/v1",
    );
    h.svc
        .set_api_key(&relay.id, &Secret::new("sk-key-a"))
        .unwrap();
    let outcome = h
        .svc
        .switch(ClientApp::Codex, &relay.id, &h.engine, &h.paths)
        .unwrap();
    assert!(!outcome.warnings.is_empty(), "未登录切 relay 应有提示");

    // 场景 2：无备份切官方（auth 是 relay 形态）→ 删文件 + warning。
    let outcome = h
        .svc
        .switch(ClientApp::Codex, OFFICIAL_CODEX_ID, &h.engine, &h.paths)
        .unwrap();
    assert!(!h.paths.codex_auth.exists(), "relay 凭据必须清除");
    assert!(!outcome.warnings.is_empty(), "无备份切官方应提示重新登录");
}

#[test]
fn codex_official_switch_survives_dangling_backup() {
    let h = harness();
    std::fs::write(&h.paths.codex_auth, OFFICIAL_AUTH).unwrap();
    std::fs::write(&h.paths.codex_config, "").unwrap();

    let relay = add_relay(
        &h.svc,
        ClientApp::Codex,
        "Relay A",
        "https://a.example.com/v1",
    );
    h.svc
        .set_api_key(&relay.id, &Secret::new("sk-key-a"))
        .unwrap();
    h.svc
        .switch(ClientApp::Codex, &relay.id, &h.engine, &h.paths)
        .unwrap();

    // 模拟用户手动删掉 Keychain 里的官方备份条目。
    let account = read_state(&h.mcpane_home)["codexAuthBackup"]["account"]
        .as_str()
        .unwrap()
        .to_owned();
    h.secrets.delete(&SecretReference { account }).unwrap();

    // 切回官方不得硬失败：降级为清除中转凭据 + 提示重新登录。
    let outcome = h
        .svc
        .switch(ClientApp::Codex, OFFICIAL_CODEX_ID, &h.engine, &h.paths)
        .unwrap();
    assert!(!outcome.warnings.is_empty(), "必须提示备份不可用/重新登录");
    assert!(!h.paths.codex_auth.exists(), "中转凭据必须清除");
    assert!(
        read_state(&h.mcpane_home)["codexAuthBackup"].is_null(),
        "悬空引用必须清理"
    );
    assert_eq!(
        h.svc
            .current()
            .unwrap()
            .get(&ClientApp::Codex)
            .map(String::as_str),
        Some(OFFICIAL_CODEX_ID)
    );
}

#[test]
fn switch_rejects_wrong_app_or_missing_key() {
    let h = harness();
    std::fs::write(&h.paths.claude_settings, "{}").unwrap();

    // app 不匹配。
    let relay = add_relay(&h.svc, ClientApp::Codex, "Relay", "https://a.example.com");
    assert!(matches!(
        h.svc
            .switch(ClientApp::Claude, &relay.id, &h.engine, &h.paths),
        Err(ProviderError::NotFound(_))
    ));

    // 未录入 Key。
    let relay = add_relay(&h.svc, ClientApp::Claude, "无 Key", "https://a.example.com");
    assert!(matches!(
        h.svc
            .switch(ClientApp::Claude, &relay.id, &h.engine, &h.paths),
        Err(ProviderError::InvalidOperation(_))
    ));
}

// ---------- backfill ----------

#[test]
fn backfill_syncs_live_edits_before_switching_away() {
    let h = harness();
    std::fs::write(&h.paths.claude_settings, r#"{"env": {}}"#).unwrap();

    let relay = add_relay(&h.svc, ClientApp::Claude, "中转 A", "https://a.example.com");
    h.svc
        .set_api_key(&relay.id, &Secret::new("sk-old"))
        .unwrap();
    h.svc
        .switch(ClientApp::Claude, &relay.id, &h.engine, &h.paths)
        .unwrap();

    // 用户手改 live 配置。
    std::fs::write(
        &h.paths.claude_settings,
        r#"{"env": {"ANTHROPIC_BASE_URL": "https://edited.example.com", "ANTHROPIC_AUTH_TOKEN": "sk-edited", "ANTHROPIC_MODEL": "claude-opus-4-8"}}"#,
    )
    .unwrap();

    h.svc
        .switch(ClientApp::Claude, OFFICIAL_CLAUDE_ID, &h.engine, &h.paths)
        .unwrap();

    let after = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .find(|p| p.id == relay.id)
        .unwrap();
    assert_eq!(
        after.base_url.as_deref(),
        Some("https://edited.example.com")
    );
    assert_eq!(after.model.as_deref(), Some("claude-opus-4-8"));
    let key = h.secrets.get(&after.api_key_ref.unwrap()).unwrap();
    assert_eq!(key.expose(), "sk-edited", "手改的 Key 必须回填 Keychain");
}

// ---------- 连通性 ----------

#[tokio::test]
async fn connectivity_test_reports_http_status() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let app = axum::Router::new().route(
            "/",
            axum::routing::get(|| async {
                (axum::http::StatusCode::UNAUTHORIZED, "auth required")
            }),
        );
        axum::serve(listener, app).await.unwrap();
    });

    let h = harness();
    let relay = add_relay(
        &h.svc,
        ClientApp::Claude,
        "local",
        &format!("http://{addr}/"),
    );
    // 401 也算网络可达。
    assert_eq!(h.svc.test_connectivity(&relay.id).await.unwrap(), 401);

    let dead = add_relay(&h.svc, ClientApp::Claude, "dead", "http://127.0.0.1:1/");
    assert!(h.svc.test_connectivity(&dead.id).await.is_err());
}

// ---------- 工具 ----------

/// 递归检查目录下是否有任何文件包含给定明文（快照/历史脱敏校验）。
fn dir_contains(root: &Path, needle: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if dir_contains(&path, needle) {
                return true;
            }
        } else if std::fs::read_to_string(&path)
            .map(|c| c.contains(needle))
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

// ---------- 新特性：wire_api / 配置片段 / 官方可编辑 / 统一供应商 ----------

#[test]
fn codex_relay_wire_api_chat_is_refused_and_leaves_files_untouched() {
    // Codex 2026-02 起 `wire_api = "chat"` 让整份 config.toml 拒绝加载，
    // 切换必须在写盘前拦下，auth.json / config.toml 一个字节都不能动。
    let h = harness();
    std::fs::write(&h.paths.codex_auth, OFFICIAL_AUTH).unwrap();
    std::fs::write(&h.paths.codex_config, "# untouched\n").unwrap();

    let mut p = ProviderProfile::new_relay(ClientApp::Codex, "Chat Vendor");
    p.base_url = Some("https://api.vendor.example/v1".into());
    p.codex_wire_api = mcpane_provider::CodexWireApi::Chat;
    let relay = h.svc.add_relay(p).unwrap();
    h.svc.set_api_key(&relay.id, &Secret::new("sk-k")).unwrap();
    let err = h
        .svc
        .switch(ClientApp::Codex, &relay.id, &h.engine, &h.paths)
        .unwrap_err();
    assert!(err.to_string().contains("Chat Completions"), "{err}");

    assert_eq!(
        std::fs::read_to_string(&h.paths.codex_config).unwrap(),
        "# untouched\n"
    );
    assert_eq!(
        std::fs::read_to_string(&h.paths.codex_auth).unwrap(),
        OFFICIAL_AUTH
    );
    assert!(!h.svc.current().unwrap().contains_key(&ClientApp::Codex));
}

#[test]
fn claude_extra_settings_fragment_merges_and_cleans_up() {
    let h = harness();
    std::fs::write(&h.paths.claude_settings, r#"{"env": {"USER_VAR": "keep"}}"#).unwrap();

    let mut a = ProviderProfile::new_relay(ClientApp::Claude, "A");
    a.base_url = Some("https://a.example.com".into());
    a.extra_settings = serde_json::json!({
        "env": {"API_TIMEOUT_MS": "3000000"},
        "attribution": {"commit": "", "pr": ""}
    });
    let a = h.svc.add_relay(a).unwrap();
    h.svc.set_api_key(&a.id, &Secret::new("sk-a")).unwrap();
    h.svc
        .switch(ClientApp::Claude, &a.id, &h.engine, &h.paths)
        .unwrap();

    let live = read_json(&h.paths.claude_settings);
    assert_eq!(live["env"]["API_TIMEOUT_MS"], "3000000");
    assert_eq!(live["attribution"]["commit"], "");
    assert_eq!(live["env"]["USER_VAR"], "keep");

    // 用户手改片段里的值 → 切走时不动手改值，只清仍匹配的键。
    let mut edited = live.clone();
    edited["env"]["API_TIMEOUT_MS"] = serde_json::json!("999");
    std::fs::write(
        &h.paths.claude_settings,
        serde_json::to_string_pretty(&edited).unwrap(),
    )
    .unwrap();

    let b = add_relay(&h.svc, ClientApp::Claude, "B", "https://b.example.com");
    h.svc.set_api_key(&b.id, &Secret::new("sk-b")).unwrap();
    h.svc
        .switch(ClientApp::Claude, &b.id, &h.engine, &h.paths)
        .unwrap();

    let live = read_json(&h.paths.claude_settings);
    // 手改值被 backfill 回填进 A 的片段并保留在 live（backfill 后值即"匹配"，
    // 语义上归属 A 的片段 → 会被清理；这是回填与清理的组合行为）。
    // attribution 未被手改且仍匹配 → 一定被清理。
    assert!(live.get("attribution").is_none(), "{live}");
    assert_eq!(live["env"]["USER_VAR"], "keep");

    // 附加片段不得覆盖受管键。
    let mut bad = ProviderProfile::new_relay(ClientApp::Claude, "Bad");
    bad.base_url = Some("https://bad.example.com".into());
    bad.extra_settings = serde_json::json!({"env": {"ANTHROPIC_BASE_URL": "https://evil"}});
    assert!(matches!(
        h.svc.add_relay(bad),
        Err(ProviderError::InvalidOperation(_))
    ));
}

#[test]
fn claude_common_snippet_overlays_enabled_profiles() {
    let h = harness();
    std::fs::write(&h.paths.claude_settings, r#"{"env": {}}"#).unwrap();

    h.svc
        .set_common_snippet(
            ClientApp::Claude,
            Some(serde_json::json!({"includeCoAuthoredBy": false})),
            &h.engine,
            &h.paths,
        )
        .unwrap();

    let mut relay = ProviderProfile::new_relay(ClientApp::Claude, "R");
    relay.base_url = Some("https://r.example.com".into());
    relay.common_config_enabled = true;
    let relay = h.svc.add_relay(relay).unwrap();
    h.svc.set_api_key(&relay.id, &Secret::new("sk-r")).unwrap();
    h.svc
        .switch(ClientApp::Claude, &relay.id, &h.engine, &h.paths)
        .unwrap();
    let live = read_json(&h.paths.claude_settings);
    assert_eq!(live["includeCoAuthoredBy"], false);

    // 切回官方（未启用通用配置）→ 通用配置键被清理。
    h.svc
        .switch(ClientApp::Claude, OFFICIAL_CLAUDE_ID, &h.engine, &h.paths)
        .unwrap();
    let live = read_json(&h.paths.claude_settings);
    assert!(live.get("includeCoAuthoredBy").is_none(), "{live}");

    // 受管键进通用配置被拒。
    assert!(matches!(
        h.svc.set_common_snippet(
            ClientApp::Claude,
            Some(serde_json::json!({"env": {"ANTHROPIC_API_KEY": "x"}})),
            &h.engine,
            &h.paths,
        ),
        Err(ProviderError::InvalidOperation(_))
    ));
}

#[test]
fn official_override_fragment_written_and_cleaned() {
    let h = harness();
    std::fs::write(&h.paths.claude_settings, r#"{"env": {}}"#).unwrap();

    // 官方订阅可编辑：配置附加片段后切到官方 → 片段写入。
    h.svc
        .update_official(
            ClientApp::Claude,
            serde_json::json!({"env": {"ENABLE_TOOL_SEARCH": "true"}}),
            false,
            &h.engine,
            &h.paths,
        )
        .unwrap();
    h.svc
        .switch(ClientApp::Claude, OFFICIAL_CLAUDE_ID, &h.engine, &h.paths)
        .unwrap();
    let live = read_json(&h.paths.claude_settings);
    assert_eq!(live["env"]["ENABLE_TOOL_SEARCH"], "true");

    // list 合成的官方项带覆盖字段。
    let official = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .find(|p| p.id == OFFICIAL_CLAUDE_ID)
        .unwrap();
    assert_eq!(official.extra_settings["env"]["ENABLE_TOOL_SEARCH"], "true");

    // 切到中转站 → 官方片段被清理（值仍匹配）。
    let relay = add_relay(&h.svc, ClientApp::Claude, "R", "https://r.example.com");
    h.svc.set_api_key(&relay.id, &Secret::new("sk-r")).unwrap();
    h.svc
        .switch(ClientApp::Claude, &relay.id, &h.engine, &h.paths)
        .unwrap();
    let live = read_json(&h.paths.claude_settings);
    assert!(live["env"].get("ENABLE_TOOL_SEARCH").is_none(), "{live}");
}

#[test]
fn unified_provider_syncs_shadows_and_cascades_delete() {
    let h = harness();
    std::fs::write(&h.paths.claude_settings, r#"{"env": {}}"#).unwrap();
    std::fs::write(&h.paths.codex_auth, OFFICIAL_AUTH).unwrap();
    std::fs::write(&h.paths.codex_config, "").unwrap();

    let mut unified = mcpane_provider::UnifiedProvider::new("NewAPI 网关");
    unified.base_url = "https://gw.example.com".into();
    unified.model = Some("claude-sonnet-5".into());
    unified.codex_model = Some("gpt-5.5".into());
    let unified = h.svc.upsert_unified(unified, &h.engine, &h.paths).unwrap();

    // 影子条目：Claude base 原样，Codex 补 /v1 且有 slug。
    let listed = h.svc.list().unwrap();
    let claude_shadow = listed
        .iter()
        .find(|p| p.id == format!("unified-claude-{}", unified.id))
        .expect("claude 影子");
    let codex_shadow = listed
        .iter()
        .find(|p| p.id == format!("unified-codex-{}", unified.id))
        .expect("codex 影子");
    assert_eq!(
        claude_shadow.base_url.as_deref(),
        Some("https://gw.example.com")
    );
    assert_eq!(
        codex_shadow.base_url.as_deref(),
        Some("https://gw.example.com/v1")
    );
    assert!(codex_shadow.codex_slug.is_some());

    // Key 下发到影子副本；影子可正常切换。
    h.svc
        .set_unified_api_key(&unified.id, &Secret::new("sk-unified"), &h.engine, &h.paths)
        .unwrap();
    h.svc
        .switch(ClientApp::Claude, &claude_shadow.id, &h.engine, &h.paths)
        .unwrap();
    let live = read_json(&h.paths.claude_settings);
    assert_eq!(live["env"]["ANTHROPIC_AUTH_TOKEN"], "sk-unified");

    // 影子不可直接删除（生命周期归统一供应商）。
    assert!(matches!(
        h.svc.delete(&codex_shadow.id),
        Err(ProviderError::InvalidOperation(_))
    ));

    // 影子使用中 → 关闭该 app / 删除统一供应商均被拒。
    let mut toggled = unified.clone();
    toggled.apps.claude = false;
    assert!(matches!(
        h.svc.upsert_unified(toggled, &h.engine, &h.paths),
        Err(ProviderError::InvalidOperation(_))
    ));
    assert!(matches!(
        h.svc.delete_unified(&unified.id),
        Err(ProviderError::InvalidOperation(_))
    ));

    // 切走后关闭 Codex 开关 → codex 影子与其 Secret 被清理。
    h.svc
        .switch(ClientApp::Claude, OFFICIAL_CLAUDE_ID, &h.engine, &h.paths)
        .unwrap();
    let codex_ref = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .find(|p| p.id == codex_shadow.id)
        .unwrap()
        .api_key_ref
        .unwrap();
    let mut toggled = unified.clone();
    toggled.apps.codex = false;
    h.svc.upsert_unified(toggled, &h.engine, &h.paths).unwrap();
    assert!(
        !h.svc
            .list()
            .unwrap()
            .iter()
            .any(|p| p.id == codex_shadow.id)
    );
    assert!(h.secrets.get(&codex_ref).is_err(), "影子 Secret 级联清理");

    // 删除统一供应商 → 剩余影子与统一 Secret 全部清理。
    h.svc.delete_unified(&unified.id).unwrap();
    assert!(
        !h.svc
            .list()
            .unwrap()
            .iter()
            .any(|p| p.id.starts_with("unified-"))
    );
    assert!(h.svc.list_unified().unwrap().is_empty());
}

#[test]
fn codex_extra_settings_fragment_and_common_snippet() {
    let h = harness();
    std::fs::write(&h.paths.codex_auth, OFFICIAL_AUTH).unwrap();
    std::fs::write(&h.paths.codex_config, "").unwrap();

    // Codex 通用配置片段（config.toml 子树的 JSON 表达）。
    h.svc
        .set_common_snippet(
            ClientApp::Codex,
            Some(serde_json::json!({"model_verbosity": "high"})),
            &h.engine,
            &h.paths,
        )
        .unwrap();

    let mut a = ProviderProfile::new_relay(ClientApp::Codex, "A");
    a.base_url = Some("https://a.example.com/v1".into());
    a.extra_settings = serde_json::json!({"model_reasoning_effort": "high"});
    a.common_config_enabled = true;
    let a = h.svc.add_relay(a).unwrap();
    h.svc.set_api_key(&a.id, &Secret::new("sk-a")).unwrap();
    h.svc
        .switch(ClientApp::Codex, &a.id, &h.engine, &h.paths)
        .unwrap();

    let config = std::fs::read_to_string(&h.paths.codex_config).unwrap();
    assert!(
        config.contains("model_reasoning_effort = \"high\""),
        "{config}"
    );
    assert!(config.contains("model_verbosity = \"high\""), "{config}");

    // Codex 官方订阅同样可编辑：配置覆盖片段后切回官方 → 片段写入、
    // 上一个中转站的片段与通用配置键清理干净。
    h.svc
        .update_official(
            ClientApp::Codex,
            serde_json::json!({"personality": "pragmatic"}),
            false,
            &h.engine,
            &h.paths,
        )
        .unwrap();
    h.svc
        .switch(ClientApp::Codex, OFFICIAL_CODEX_ID, &h.engine, &h.paths)
        .unwrap();
    let config = std::fs::read_to_string(&h.paths.codex_config).unwrap();
    assert!(config.contains("personality = \"pragmatic\""), "{config}");
    assert!(!config.contains("model_reasoning_effort"), "{config}");
    assert!(!config.contains("model_verbosity"), "{config}");

    // Codex 片段不得覆盖受管键。
    let mut bad = ProviderProfile::new_relay(ClientApp::Codex, "Bad");
    bad.base_url = Some("https://bad.example.com/v1".into());
    bad.extra_settings = serde_json::json!({"model_provider": "evil"});
    assert!(matches!(
        h.svc.add_relay(bad),
        Err(ProviderError::InvalidOperation(_))
    ));

    // 明文凭据字段 experimental_bearer_token 必须被拒（否则进不脱敏的 config.toml
    // 快照，绕过 auth.json→Keychain）：片段与通用配置两条路径都拦。
    let mut bearer = ProviderProfile::new_relay(ClientApp::Codex, "Bearer");
    bearer.base_url = Some("https://bearer.example.com/v1".into());
    bearer.extra_settings = serde_json::json!({"experimental_bearer_token": "sk-leak"});
    assert!(matches!(
        h.svc.add_relay(bearer),
        Err(ProviderError::InvalidOperation(_))
    ));
    assert!(matches!(
        h.svc.set_common_snippet(
            ClientApp::Codex,
            Some(serde_json::json!({"experimental_bearer_token": "sk-leak"})),
            &h.engine,
            &h.paths,
        ),
        Err(ProviderError::InvalidOperation(_))
    ));
}

#[tokio::test]
async fn endpoint_speed_test_reports_latency_and_errors() {
    // 本地起一个真实 HTTP 端点（对齐 connectivity 测试的做法）。
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = axum::Router::new().route("/", axum::routing::get(|| async { "ok" }));
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let good = format!("http://{addr}/");
    let bad = "http://127.0.0.1:1/".to_owned(); // 端口 1：连接必然被拒
    let results = mcpane_provider::test_endpoints(vec![good.clone(), bad.clone()]).await;
    assert_eq!(results.len(), 2);
    let good_result = results.iter().find(|r| r.url == good).unwrap();
    assert_eq!(good_result.status, Some(200));
    assert!(good_result.latency_ms.is_some());
    let bad_result = results.iter().find(|r| r.url == bad).unwrap();
    assert!(bad_result.status.is_none());
    assert!(bad_result.error.is_some());
}

#[test]
fn codex_query_params_write_clear_and_backfill() {
    let h = harness();
    std::fs::write(&h.paths.codex_auth, OFFICIAL_AUTH).unwrap();
    std::fs::write(&h.paths.codex_config, "").unwrap();

    // Azure 形态：query_params 整表写入。
    let mut azure = ProviderProfile::new_relay(ClientApp::Codex, "Azure OpenAI");
    azure.base_url = Some("https://myres.openai.azure.com/openai".into());
    azure
        .codex_query_params
        .insert("api-version".into(), "2025-04-01-preview".into());
    let azure = h.svc.add_relay(azure).unwrap();
    h.svc.set_api_key(&azure.id, &Secret::new("sk-az")).unwrap();
    h.svc
        .switch(ClientApp::Codex, &azure.id, &h.engine, &h.paths)
        .unwrap();
    let config = std::fs::read_to_string(&h.paths.codex_config).unwrap();
    assert!(
        config.contains("api-version") && config.contains("2025-04-01-preview"),
        "{config}"
    );

    // 清空 query_params 后重新启用（同 id 重切不触发 backfill）→ 残留清除。
    let mut edited = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .find(|p| p.id == azure.id)
        .unwrap();
    edited.codex_query_params.clear();
    h.svc.update_relay(edited, &h.engine, &h.paths).unwrap();
    h.svc
        .switch(ClientApp::Codex, &azure.id, &h.engine, &h.paths)
        .unwrap();
    let config = std::fs::read_to_string(&h.paths.codex_config).unwrap();
    assert!(!config.contains("api-version"), "{config}");

    // 切走再切回：backfill 以 live 为准（live 已无 query_params → 保持为空）。
    h.svc
        .switch(ClientApp::Codex, OFFICIAL_CODEX_ID, &h.engine, &h.paths)
        .unwrap();
    h.svc
        .switch(ClientApp::Codex, &azure.id, &h.engine, &h.paths)
        .unwrap();
    let config = std::fs::read_to_string(&h.paths.codex_config).unwrap();
    assert!(!config.contains("api-version"), "{config}");
}

#[test]
fn reapplying_same_provider_cleans_removed_fragment_keys() {
    let h = harness();
    std::fs::write(&h.paths.claude_settings, r#"{"env": {}}"#).unwrap();

    let mut relay = ProviderProfile::new_relay(ClientApp::Claude, "A");
    relay.base_url = Some("https://a.example.com".into());
    relay.extra_settings = serde_json::json!({"env": {"ENABLE_TOOL_SEARCH": "true"}});
    let relay = h.svc.add_relay(relay).unwrap();
    h.svc.set_api_key(&relay.id, &Secret::new("sk-a")).unwrap();
    h.svc
        .switch(ClientApp::Claude, &relay.id, &h.engine, &h.paths)
        .unwrap();
    assert_eq!(
        read_json(&h.paths.claude_settings)["env"]["ENABLE_TOOL_SEARCH"],
        "true"
    );

    // 编辑片段删掉该键 → 同一供应商重新启用 → 残留被清（applied_fragments 记账）。
    let mut edited = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .find(|p| p.id == relay.id)
        .unwrap();
    edited.extra_settings = serde_json::Value::Null;
    h.svc.update_relay(edited, &h.engine, &h.paths).unwrap();
    h.svc
        .switch(ClientApp::Claude, &relay.id, &h.engine, &h.paths)
        .unwrap();
    let live = read_json(&h.paths.claude_settings);
    assert!(
        live["env"].get("ENABLE_TOOL_SEARCH").is_none(),
        "重新启用必须清掉片段里删除的键：{live}"
    );
}

#[test]
fn update_relay_current_provider_applies_live_immediately() {
    let h = harness();
    std::fs::write(&h.paths.claude_settings, r#"{"env": {}}"#).unwrap();

    let mut relay = ProviderProfile::new_relay(ClientApp::Claude, "A");
    relay.base_url = Some("https://a.example.com".into());
    relay.model = Some("model-old".into());
    let relay = h.svc.add_relay(relay).unwrap();
    h.svc.set_api_key(&relay.id, &Secret::new("sk-a")).unwrap();
    h.svc
        .switch(ClientApp::Claude, &relay.id, &h.engine, &h.paths)
        .unwrap();
    assert_eq!(
        read_json(&h.paths.claude_settings)["env"]["ANTHROPIC_MODEL"],
        "model-old"
    );

    // 编辑当前生效项的 base_url + model → update_relay 后无需再 switch，live 已更新。
    let mut edited = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .find(|p| p.id == relay.id)
        .unwrap();
    edited.base_url = Some("https://b.example.com".into());
    edited.model = Some("model-new".into());
    h.svc.update_relay(edited, &h.engine, &h.paths).unwrap();

    let live = read_json(&h.paths.claude_settings);
    assert_eq!(
        live["env"]["ANTHROPIC_BASE_URL"], "https://b.example.com",
        "编辑当前项后 base_url 应立即生效"
    );
    assert_eq!(
        live["env"]["ANTHROPIC_MODEL"], "model-new",
        "编辑当前项后 model 应立即生效"
    );
}

#[test]
fn set_api_key_on_current_provider_rewrites_live_via_reapply() {
    let h = harness();
    std::fs::write(&h.paths.claude_settings, r#"{"env": {}}"#).unwrap();

    let relay = add_relay(&h.svc, ClientApp::Claude, "A", "https://a.example.com");
    h.svc
        .set_api_key(&relay.id, &Secret::new("sk-old"))
        .unwrap();
    h.svc
        .switch(ClientApp::Claude, &relay.id, &h.engine, &h.paths)
        .unwrap();
    assert_eq!(
        read_json(&h.paths.claude_settings)["env"]["ANTHROPIC_AUTH_TOKEN"],
        "sk-old"
    );

    // 轮换当前生效项的 Key：set_api_key 落 Keychain 后 reapply_current 重写 live。
    h.svc
        .set_api_key(&relay.id, &Secret::new("sk-new"))
        .unwrap();
    let applied = h
        .svc
        .reapply_current(&relay.id, &h.engine, &h.paths)
        .unwrap();
    assert!(applied, "当前生效项应触发 live 重写");
    assert_eq!(
        read_json(&h.paths.claude_settings)["env"]["ANTHROPIC_AUTH_TOKEN"],
        "sk-new",
        "轮换的 Key 应即时写入 live"
    );

    // 非当前项：reapply_current 不动 live。
    let b = add_relay(&h.svc, ClientApp::Claude, "B", "https://b.example.com");
    h.svc.set_api_key(&b.id, &Secret::new("sk-b")).unwrap();
    assert!(
        !h.svc.reapply_current(&b.id, &h.engine, &h.paths).unwrap(),
        "非当前项不触发重写"
    );
    assert_eq!(
        read_json(&h.paths.claude_settings)["env"]["ANTHROPIC_AUTH_TOKEN"],
        "sk-new"
    );
}

#[test]
fn update_official_on_current_official_applies_live_immediately() {
    let h = harness();
    std::fs::write(&h.paths.claude_settings, r#"{"env": {}}"#).unwrap();

    // 官方当前生效（空态默认即官方，显式切一次确立 current）。
    h.svc
        .switch(ClientApp::Claude, OFFICIAL_CLAUDE_ID, &h.engine, &h.paths)
        .unwrap();

    // 编辑当前官方项的附加片段 → 无需再切换，live 立即写入。
    h.svc
        .update_official(
            ClientApp::Claude,
            serde_json::json!({"env": {"ENABLE_TOOL_SEARCH": "true"}}),
            false,
            &h.engine,
            &h.paths,
        )
        .unwrap();
    assert_eq!(
        read_json(&h.paths.claude_settings)["env"]["ENABLE_TOOL_SEARCH"],
        "true",
        "编辑当前官方项后片段应即时生效"
    );
}

#[test]
fn set_common_snippet_reapplies_current_enabled_provider() {
    let h = harness();
    std::fs::write(&h.paths.claude_settings, r#"{"env": {}}"#).unwrap();

    // 当前生效项启用了通用配置。
    let mut relay = ProviderProfile::new_relay(ClientApp::Claude, "R");
    relay.base_url = Some("https://r.example.com".into());
    relay.common_config_enabled = true;
    let relay = h.svc.add_relay(relay).unwrap();
    h.svc.set_api_key(&relay.id, &Secret::new("sk-r")).unwrap();
    h.svc
        .switch(ClientApp::Claude, &relay.id, &h.engine, &h.paths)
        .unwrap();

    // 改全局通用配置 → 无需再切换，当前项 live 立即纳入新片段。
    h.svc
        .set_common_snippet(
            ClientApp::Claude,
            Some(serde_json::json!({"includeCoAuthoredBy": false})),
            &h.engine,
            &h.paths,
        )
        .unwrap();
    assert_eq!(
        read_json(&h.paths.claude_settings)["includeCoAuthoredBy"],
        false,
        "改通用配置后当前项应即时生效"
    );

    // 清空通用配置 → 当前项 live 里的键被清理。
    h.svc
        .set_common_snippet(ClientApp::Claude, None, &h.engine, &h.paths)
        .unwrap();
    assert!(
        read_json(&h.paths.claude_settings)
            .get("includeCoAuthoredBy")
            .is_none(),
        "清空通用配置后键应即时清除"
    );
}

#[test]
fn set_unified_api_key_rotates_current_shadow_live() {
    let h = harness();
    std::fs::write(&h.paths.claude_settings, r#"{"env": {}}"#).unwrap();

    let mut unified = mcpane_provider::UnifiedProvider::new("网关");
    unified.base_url = "https://gw.example.com".into();
    unified.model = Some("claude-sonnet-5".into());
    let unified = h.svc.upsert_unified(unified, &h.engine, &h.paths).unwrap();
    h.svc
        .set_unified_api_key(&unified.id, &Secret::new("sk-old"), &h.engine, &h.paths)
        .unwrap();
    let claude_shadow = format!("unified-claude-{}", unified.id);
    h.svc
        .switch(ClientApp::Claude, &claude_shadow, &h.engine, &h.paths)
        .unwrap();
    assert_eq!(
        read_json(&h.paths.claude_settings)["env"]["ANTHROPIC_AUTH_TOKEN"],
        "sk-old"
    );

    // 轮换统一 Key → 当前生效的影子 live 立即用新 Key 重写。
    h.svc
        .set_unified_api_key(&unified.id, &Secret::new("sk-new"), &h.engine, &h.paths)
        .unwrap();
    assert_eq!(
        read_json(&h.paths.claude_settings)["env"]["ANTHROPIC_AUTH_TOKEN"],
        "sk-new",
        "轮换统一 Key 后当前影子应即时生效"
    );
}

#[test]
fn update_relay_non_current_provider_does_not_touch_live() {
    let h = harness();
    std::fs::write(&h.paths.claude_settings, r#"{"env": {}}"#).unwrap();

    // A 为当前项，B 非当前。
    let a = add_relay(&h.svc, ClientApp::Claude, "A", "https://a.example.com");
    h.svc.set_api_key(&a.id, &Secret::new("sk-a")).unwrap();
    h.svc
        .switch(ClientApp::Claude, &a.id, &h.engine, &h.paths)
        .unwrap();

    let b = add_relay(&h.svc, ClientApp::Claude, "B", "https://b.example.com");
    // 编辑非当前项 B → live 仍指向 A，不被 B 覆盖。
    let mut edited = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .find(|p| p.id == b.id)
        .unwrap();
    edited.base_url = Some("https://b2.example.com".into());
    h.svc.update_relay(edited, &h.engine, &h.paths).unwrap();

    let live = read_json(&h.paths.claude_settings);
    assert_eq!(
        live["env"]["ANTHROPIC_BASE_URL"], "https://a.example.com",
        "编辑非当前项不得改动 live"
    );
}

#[test]
fn config_document_empty_path_returns_whole_doc_for_both_formats() {
    // provider_live_settings 依赖空路径取全量（JSON 与 TOML 两种格式）。
    use mcpane_config::{ConfigDocument, ConfigFormat};
    let json = ConfigDocument::parse(r#"{"env": {"A": "1"}}"#, ConfigFormat::Json).unwrap();
    assert_eq!(json.get(&[]).unwrap()["env"]["A"], "1");

    let toml = ConfigDocument::parse(
        "model = \"gpt\"\n[model_providers.x]\nname = \"a\"\n",
        ConfigFormat::Toml,
    )
    .unwrap();
    let root = toml.get(&[]).expect("TOML 空路径必须能取整份文档");
    assert_eq!(root["model"], "gpt");
    assert_eq!(root["model_providers"]["x"]["name"], "a");
}

// ---------- Gemini 切换 ----------

#[test]
fn gemini_relay_switch_writes_env_and_settings_and_redacts_snapshot() {
    let h = harness();
    // 预置：用户已有 .env 自定义键 + settings.json 里的 mcpServers。
    std::fs::write(
        &h.paths.gemini_env,
        "# 用户注释\nGOOGLE_CLOUD_PROJECT=my-proj\n",
    )
    .unwrap();
    std::fs::write(
        &h.paths.gemini_settings,
        r#"{"mcpServers":{"keep":{"httpUrl":"http://x"}},"security":{"auth":{"selectedType":"oauth-personal"}}}"#,
    )
    .unwrap();

    let mut p = ProviderProfile::new_relay(ClientApp::Gemini, "Gemini 中转");
    p.base_url = Some("https://gemini-relay.example.com".into());
    p.model = Some("gemini-3-pro".into());
    let relay = h.svc.add_relay(p).unwrap();
    h.svc
        .set_api_key(&relay.id, &Secret::new("AIza-gemini-secret"))
        .unwrap();

    let outcome = h
        .svc
        .switch(ClientApp::Gemini, &relay.id, &h.engine, &h.paths)
        .unwrap();
    assert!(outcome.written);

    let env_text = std::fs::read_to_string(&h.paths.gemini_env).unwrap();
    assert!(env_text.contains("# 用户注释"), "用户注释保留");
    assert!(
        env_text.contains("GOOGLE_CLOUD_PROJECT=my-proj"),
        "用户键保留"
    );
    assert!(env_text.contains("GOOGLE_GEMINI_BASE_URL=https://gemini-relay.example.com"));
    assert!(env_text.contains("GEMINI_API_KEY=AIza-gemini-secret"));
    assert!(env_text.contains("GEMINI_MODEL=gemini-3-pro"));

    let settings = read_json(&h.paths.gemini_settings);
    assert_eq!(
        settings["security"]["auth"]["selectedType"],
        "gemini-api-key"
    );
    assert_eq!(
        settings["mcpServers"]["keep"]["httpUrl"], "http://x",
        "用户 mcpServers 必须保留"
    );

    assert!(
        !dir_contains(&h.mcpane_home, "AIza-gemini-secret"),
        "快照/历史不得包含 Gemini API Key 明文"
    );
}

#[test]
fn gemini_switch_back_to_official_clears_managed_keys() {
    let h = harness();
    std::fs::write(&h.paths.gemini_env, "USER_VAR=keep\n").unwrap();
    std::fs::write(&h.paths.gemini_settings, "{}").unwrap();

    let mut p = ProviderProfile::new_relay(ClientApp::Gemini, "Gemini 中转");
    p.base_url = Some("https://gr.example.com".into());
    let relay = h.svc.add_relay(p).unwrap();
    h.svc
        .set_api_key(&relay.id, &Secret::new("AIza-old-key"))
        .unwrap();
    h.svc
        .switch(ClientApp::Gemini, &relay.id, &h.engine, &h.paths)
        .unwrap();

    h.svc
        .switch(
            ClientApp::Gemini,
            mcpane_provider::OFFICIAL_GEMINI_ID,
            &h.engine,
            &h.paths,
        )
        .unwrap();

    let env_text = std::fs::read_to_string(&h.paths.gemini_env).unwrap();
    assert!(env_text.contains("USER_VAR=keep"), "用户键保留");
    assert!(!env_text.contains("GOOGLE_GEMINI_BASE_URL"), "受管键清除");
    assert!(!env_text.contains("GEMINI_API_KEY"), "Key 清除");

    let settings = read_json(&h.paths.gemini_settings);
    assert_eq!(
        settings["security"]["auth"]["selectedType"],
        "oauth-personal"
    );
    assert!(
        !dir_contains(&h.mcpane_home, "AIza-old-key"),
        "切回官方时旧 Key 不得进快照"
    );
}

#[test]
fn gemini_backfill_reads_live_env_on_switch_away() {
    let h = harness();
    std::fs::write(&h.paths.gemini_env, "").unwrap();
    std::fs::write(&h.paths.gemini_settings, "{}").unwrap();

    let mut p = ProviderProfile::new_relay(ClientApp::Gemini, "Gemini 中转");
    p.base_url = Some("https://before.example.com".into());
    let relay = h.svc.add_relay(p).unwrap();
    h.svc
        .set_api_key(&relay.id, &Secret::new("AIza-key"))
        .unwrap();
    h.svc
        .switch(ClientApp::Gemini, &relay.id, &h.engine, &h.paths)
        .unwrap();

    // 用户手改 live：换端点 + 加模型 + 换 Key。
    std::fs::write(
        &h.paths.gemini_env,
        "GOOGLE_GEMINI_BASE_URL=https://hand-edited.example.com\nGEMINI_API_KEY=AIza-hand-key\nGEMINI_MODEL=gemini-3-flash\n",
    )
    .unwrap();

    h.svc
        .switch(
            ClientApp::Gemini,
            mcpane_provider::OFFICIAL_GEMINI_ID,
            &h.engine,
            &h.paths,
        )
        .unwrap();

    let listed = h.svc.list().unwrap();
    let back = listed.iter().find(|x| x.id == relay.id).unwrap();
    assert_eq!(
        back.base_url.as_deref(),
        Some("https://hand-edited.example.com"),
        "切走前必须回填 live base_url"
    );
    assert_eq!(back.model.as_deref(), Some("gemini-3-flash"));
    let key = h.secrets.get(back.api_key_ref.as_ref().unwrap()).unwrap();
    assert_eq!(key.expose(), "AIza-hand-key", "live Key 回填到凭据库");
}

// ---------- 首次接管收编存量（P0：防首切销毁用户既有 Key） ----------

#[test]
fn first_switch_imports_existing_claude_relay_config() {
    let h = harness();
    // 用户此前用 cc-switch/手工配置的中转：Key 不在我们的凭据库里。
    std::fs::write(
        &h.paths.claude_settings,
        serde_json::json!({
            "env": {
                "ANTHROPIC_BASE_URL": "https://legacy.example.com",
                "ANTHROPIC_AUTH_TOKEN": "sk-legacy-token",
                "ANTHROPIC_MODEL": "legacy-model"
            }
        })
        .to_string(),
    )
    .unwrap();

    let relay = add_relay(
        &h.svc,
        ClientApp::Claude,
        "新中转",
        "https://new.example.com",
    );
    h.svc
        .set_api_key(&relay.id, &Secret::new("sk-new"))
        .unwrap();
    let outcome = h
        .svc
        .switch(ClientApp::Claude, &relay.id, &h.engine, &h.paths)
        .unwrap();
    assert!(
        outcome.warnings.iter().any(|w| w.contains("导入")),
        "首切收编要有用户可见提示：{:?}",
        outcome.warnings
    );

    // 存量配置被收编为可切回的 Profile，Key 落凭据库。
    let imported = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .find(|p| p.name == "既有配置（自动导入）" && p.app == ClientApp::Claude)
        .expect("既有中转配置必须被收编");
    assert_eq!(
        imported.base_url.as_deref(),
        Some("https://legacy.example.com")
    );
    assert_eq!(imported.model.as_deref(), Some("legacy-model"));
    let key = h
        .secrets
        .get(imported.api_key_ref.as_ref().unwrap())
        .unwrap();
    assert_eq!(key.expose(), "sk-legacy-token", "旧 Key 必须可从凭据库恢复");

    // 切回收编项 = 完整恢复旧配置。
    h.svc
        .switch(ClientApp::Claude, &imported.id, &h.engine, &h.paths)
        .unwrap();
    let live = read_json(&h.paths.claude_settings);
    assert_eq!(
        live["env"]["ANTHROPIC_BASE_URL"],
        "https://legacy.example.com"
    );
    assert_eq!(live["env"]["ANTHROPIC_AUTH_TOKEN"], "sk-legacy-token");
}

#[test]
fn startup_import_captures_existing_relay_and_sets_current() {
    let h = harness();
    // 全新安装、用户已有中转 live（cc-switch/手工配置）：MCPane 从未切换过。
    std::fs::write(
        &h.paths.claude_settings,
        serde_json::json!({
            "env": {
                "ANTHROPIC_BASE_URL": "https://legacy.example.com",
                "ANTHROPIC_AUTH_TOKEN": "sk-legacy-token",
                "ANTHROPIC_MODEL": "legacy-model"
            }
        })
        .to_string(),
    )
    .unwrap();

    // 首启前：无 provider、current 未设（前端会误显示官方生效）。
    assert!(!h.svc.current().unwrap().contains_key(&ClientApp::Claude));

    let name = h
        .svc
        .import_live_on_startup(ClientApp::Claude, &h.engine, &h.paths)
        .unwrap();
    assert_eq!(name.as_deref(), Some("既有配置（自动导入）"));

    // 收编为可管理 Profile，Key 落凭据库。
    let imported = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .find(|p| p.name == "既有配置（自动导入）" && p.app == ClientApp::Claude)
        .expect("既有中转必须被收编");
    assert_eq!(
        imported.base_url.as_deref(),
        Some("https://legacy.example.com")
    );
    let key = h
        .secrets
        .get(imported.api_key_ref.as_ref().unwrap())
        .unwrap();
    assert_eq!(key.expose(), "sk-legacy-token");

    // 关键：current 指向收编项，如实反映 live 是中转（而非官方）。
    assert_eq!(
        h.svc.current().unwrap().get(&ClientApp::Claude),
        Some(&imported.id)
    );

    // 幂等：再次首启不重复导入（已有 provider → 守卫短路）。
    assert!(
        h.svc
            .import_live_on_startup(ClientApp::Claude, &h.engine, &h.paths)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        h.svc
            .list()
            .unwrap()
            .iter()
            .filter(|p| p.name == "既有配置（自动导入）" && p.app == ClientApp::Claude)
            .count(),
        1,
        "不得重复收编"
    );
}

#[test]
fn startup_import_noop_for_official_live_and_engaged_app() {
    let h = harness();
    // 官方形态 live（无中转键）→ 无可收编，不置 current。
    std::fs::write(&h.paths.claude_settings, r#"{"env": {}}"#).unwrap();
    assert!(
        h.svc
            .import_live_on_startup(ClientApp::Claude, &h.engine, &h.paths)
            .unwrap()
            .is_none(),
        "官方 live 无可收编"
    );
    assert!(!h.svc.current().unwrap().contains_key(&ClientApp::Claude));

    // 用户已录入过 provider（已 engage）→ 即便 live 是中转也不自动收编。
    std::fs::write(
        &h.paths.claude_settings,
        r#"{"env": {"ANTHROPIC_BASE_URL": "https://x.example.com", "ANTHROPIC_AUTH_TOKEN": "sk-x"}}"#,
    )
    .unwrap();
    add_relay(
        &h.svc,
        ClientApp::Claude,
        "手动录入",
        "https://manual.example.com",
    );
    assert!(
        h.svc
            .import_live_on_startup(ClientApp::Claude, &h.engine, &h.paths)
            .unwrap()
            .is_none(),
        "已有 provider 的 app 不自动收编"
    );
}

#[test]
fn first_switch_import_skips_official_live_and_duplicates() {
    let h = harness();
    // 官方形态（无中转 env）：不收编。
    std::fs::write(
        &h.paths.claude_settings,
        r#"{"permissions": {"allow": ["Bash"]}}"#,
    )
    .unwrap();
    let relay = add_relay(&h.svc, ClientApp::Claude, "A", "https://a.example.com");
    h.svc.set_api_key(&relay.id, &Secret::new("sk-a")).unwrap();
    h.svc
        .switch(ClientApp::Claude, &relay.id, &h.engine, &h.paths)
        .unwrap();
    assert!(
        !h.svc
            .list()
            .unwrap()
            .iter()
            .any(|p| p.name == "既有配置（自动导入）"),
        "官方形态 live 不应产生导入条目"
    );

    // 同 base_url 已有条目：不重复收编（模拟 state 重置后 live 是我们写的内容）。
    let h2 = harness();
    std::fs::write(
        &h2.paths.claude_settings,
        serde_json::json!({
            "env": {
                "ANTHROPIC_BASE_URL": "https://b.example.com",
                "ANTHROPIC_AUTH_TOKEN": "sk-b-live"
            }
        })
        .to_string(),
    )
    .unwrap();
    let relay_b = add_relay(&h2.svc, ClientApp::Claude, "B", "https://b.example.com");
    h2.svc
        .set_api_key(&relay_b.id, &Secret::new("sk-b"))
        .unwrap();
    h2.svc
        .switch(ClientApp::Claude, &relay_b.id, &h2.engine, &h2.paths)
        .unwrap();
    assert!(
        !h2.svc
            .list()
            .unwrap()
            .iter()
            .any(|p| p.name == "既有配置（自动导入）"),
        "同 base_url 已有条目时不应重复导入"
    );
}

#[test]
fn gemini_first_switch_imports_env_key() {
    let h = harness();
    std::fs::write(
        &h.paths.gemini_env,
        "# 用户注释\nGOOGLE_GEMINI_BASE_URL=https://old-gw.example.com\nGEMINI_API_KEY=AIza-old-key\n",
    )
    .unwrap();
    let relay = add_relay(
        &h.svc,
        ClientApp::Gemini,
        "新网关",
        "https://new-gw.example.com",
    );
    h.svc
        .set_api_key(&relay.id, &Secret::new("AIza-new"))
        .unwrap();
    h.svc
        .switch(ClientApp::Gemini, &relay.id, &h.engine, &h.paths)
        .unwrap();

    let imported = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .find(|p| p.name == "既有配置（自动导入）" && p.app == ClientApp::Gemini)
        .expect("Gemini 既有 .env 中转配置必须被收编");
    assert_eq!(
        imported.base_url.as_deref(),
        Some("https://old-gw.example.com")
    );
    let key = h
        .secrets
        .get(imported.api_key_ref.as_ref().unwrap())
        .unwrap();
    assert_eq!(key.expose(), "AIza-old-key");
}

#[test]
fn codex_first_switch_imports_relay_auth_and_config() {
    let h = harness();
    // 其他工具写的中转形态：auth.json 纯 OPENAI_API_KEY + config 指向自有表。
    std::fs::write(
        &h.paths.codex_auth,
        r#"{"OPENAI_API_KEY": "sk-third-party"}"#,
    )
    .unwrap();
    std::fs::write(
        &h.paths.codex_config,
        "model_provider = \"thirdparty\"\nmodel = \"gpt-x\"\n\n[model_providers.thirdparty]\nname = \"三方\"\nbase_url = \"https://third.example.com/v1\"\nwire_api = \"chat\"\n",
    )
    .unwrap();
    let relay = add_relay(
        &h.svc,
        ClientApp::Codex,
        "新中转",
        "https://new.example.com/v1",
    );
    h.svc
        .set_api_key(&relay.id, &Secret::new("sk-new"))
        .unwrap();
    h.svc
        .switch(ClientApp::Codex, &relay.id, &h.engine, &h.paths)
        .unwrap();

    let imported = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .find(|p| p.name == "既有配置（自动导入）" && p.app == ClientApp::Codex)
        .expect("Codex 既有中转配置必须被收编");
    assert_eq!(
        imported.base_url.as_deref(),
        Some("https://third.example.com/v1")
    );
    assert_eq!(imported.model.as_deref(), Some("gpt-x"));
    assert!(
        imported
            .codex_slug
            .as_deref()
            .is_some_and(|s| s.starts_with("mcpane-")),
        "导入条目的 slug 必须带所有权前缀"
    );
    let key = h
        .secrets
        .get(imported.api_key_ref.as_ref().unwrap())
        .unwrap();
    assert_eq!(key.expose(), "sk-third-party");
}

// ---------- Codex 官方臂失败补偿（P0：防官方 token 半态发给中转站） ----------

#[test]
fn codex_official_switch_failure_rolls_back_auth_and_keeps_backup() {
    let h = harness();
    std::fs::write(&h.paths.codex_auth, OFFICIAL_AUTH).unwrap();
    let relay = add_relay(
        &h.svc,
        ClientApp::Codex,
        "Relay",
        "https://r.example.com/v1",
    );
    h.svc.set_api_key(&relay.id, &Secret::new("sk-r")).unwrap();
    h.svc
        .switch(ClientApp::Codex, &relay.id, &h.engine, &h.paths)
        .unwrap();
    let relay_auth = std::fs::read_to_string(&h.paths.codex_auth).unwrap();

    // 用户手改坏 config.toml：官方臂的 config 清理必然失败。
    std::fs::write(&h.paths.codex_config, "model_provider = [未闭合").unwrap();
    let result = h
        .svc
        .switch(ClientApp::Codex, OFFICIAL_CODEX_ID, &h.engine, &h.paths);
    assert!(result.is_err(), "config.toml 损坏时切官方必须失败");

    // auth.json 回滚到中转内容：绝不能留下「官方 OAuth token + config 指向中转」半态。
    assert_eq!(
        std::fs::read_to_string(&h.paths.codex_auth).unwrap(),
        relay_auth,
        "config 失败后 auth.json 必须回滚"
    );
    // 备份仍在（引用与密文都不销毁），重试切官方可恢复。
    let state = read_state(&h.mcpane_home);
    let backup_ref = state["codexAuthBackup"]["account"]
        .as_str()
        .expect("失败路径不得删除备份引用");
    let backup = h
        .secrets
        .get(&SecretReference {
            account: backup_ref.to_owned(),
        })
        .expect("失败路径不得删除 Keychain 备份");
    assert_eq!(backup.expose(), OFFICIAL_AUTH);

    // 修好 config 后重试成功，官方登录完整恢复。
    std::fs::write(&h.paths.codex_config, "").unwrap();
    h.svc
        .switch(ClientApp::Codex, OFFICIAL_CODEX_ID, &h.engine, &h.paths)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&h.paths.codex_auth).unwrap(),
        OFFICIAL_AUTH
    );
    assert!(read_state(&h.mcpane_home)["codexAuthBackup"].is_null());
}

// ---------- Claude Desktop（direct / proxy 双模式落盘） ----------

// Claude Desktop 只有 macOS / Windows 版，切换入口经
// claude_desktop::is_supported_platform() 把其他平台拦成 InvalidOperation。
// 下面三个用例断言的是落盘行为，门控条件必须与被测代码一致——否则在 ubuntu CI
// 上必然撞平台拦截而红（常量只被它们用，不一起门控会触发 dead_code）。
#[cfg(any(target_os = "macos", windows))]
const MCPANE_DESKTOP_PROFILE_ID: &str = "00000000-0000-4000-8000-0000004d4350";

#[cfg(any(target_os = "macos", windows))]
#[test]
fn claude_desktop_direct_switch_and_restore_official() {
    let h = harness();
    let mut p = ProviderProfile::new_relay(ClientApp::ClaudeDesktop, "AiHubMix");
    p.base_url = Some("https://aihubmix.com".into());
    let relay = h.svc.add_relay(p).unwrap();
    h.svc
        .set_api_key(&relay.id, &Secret::new("sk-desktop"))
        .unwrap();

    let outcome = h
        .svc
        .switch(ClientApp::ClaudeDesktop, &relay.id, &h.engine, &h.paths)
        .unwrap();
    assert!(outcome.written);
    assert!(
        outcome
            .warnings
            .iter()
            .any(|w| w.contains("重启 Claude Desktop"))
    );

    // 4 文件协同：两份 config 置 3p、profile 直指供应商、meta 指向我们的条目。
    let dp = &h.paths.claude_desktop;
    assert_eq!(read_json(&dp.normal_config)["deploymentMode"], "3p");
    assert_eq!(read_json(&dp.threep_config)["deploymentMode"], "3p");
    let profile_json = read_json(&dp.profile);
    assert_eq!(profile_json["inferenceProvider"], "gateway");
    assert_eq!(profile_json["inferenceGatewayAuthScheme"], "bearer");
    assert_eq!(
        profile_json["inferenceGatewayBaseUrl"],
        "https://aihubmix.com"
    );
    assert_eq!(profile_json["inferenceGatewayApiKey"], "sk-desktop");
    let meta = read_json(&dp.meta);
    assert_eq!(meta["appliedId"], MCPANE_DESKTOP_PROFILE_ID);

    // 切回官方：1p 恢复、profile 删除、meta 清理；同样提示重启生效。
    let outcome = h
        .svc
        .switch(
            ClientApp::ClaudeDesktop,
            mcpane_provider::OFFICIAL_CLAUDE_DESKTOP_ID,
            &h.engine,
            &h.paths,
        )
        .unwrap();
    assert!(outcome.written);
    assert!(
        outcome
            .warnings
            .iter()
            .any(|w| w.contains("重启 Claude Desktop"))
    );
    assert_eq!(read_json(&dp.normal_config)["deploymentMode"], "1p");
    assert_eq!(read_json(&dp.threep_config)["deploymentMode"], "1p");
    assert!(!dp.profile.exists());
    assert!(read_json(&dp.meta).get("appliedId").is_none());
}

#[cfg(any(target_os = "macos", windows))]
#[test]
fn claude_desktop_proxy_switch_points_profile_at_local_gateway() {
    let h = harness();
    let mut p = ProviderProfile::new_relay(ClientApp::ClaudeDesktop, "DeepSeek");
    p.base_url = Some("https://api.deepseek.com/anthropic".into());
    p.desktop_mode = mcpane_provider::DesktopMode::Proxy;
    p.desktop_routes = vec![mcpane_provider::DesktopRoute {
        route_id: "claude-sonnet-5".into(),
        upstream_model: "deepseek-v4-pro".into(),
        label_override: Some("DeepSeek".into()),
        supports_1m: false,
    }];
    let relay = h.svc.add_relay(p).unwrap();
    h.svc.set_api_key(&relay.id, &Secret::new("sk-ds")).unwrap();

    let outcome = h
        .svc
        .switch(ClientApp::ClaudeDesktop, &relay.id, &h.engine, &h.paths)
        .unwrap();
    assert!(outcome.written);
    assert!(
        outcome
            .warnings
            .iter()
            .any(|w| w.contains("保持 MCPane 运行"))
    );

    // profile 指向本地网关（token/端口来自 store 持久化的 desktop_gateway）。
    let gateway = h.svc.desktop_gateway().unwrap().unwrap();
    assert!(gateway.token.starts_with("mcpane-"));
    let profile_json = read_json(&h.paths.claude_desktop.profile);
    assert_eq!(
        profile_json["inferenceGatewayBaseUrl"],
        format!("http://127.0.0.1:{}/claude-desktop", gateway.listen_port)
    );
    assert_eq!(
        profile_json["inferenceGatewayApiKey"],
        gateway.token.as_str()
    );
    // 品牌路由带 labelOverride，写为对象条目。
    assert_eq!(
        profile_json["inferenceModels"][0]["name"],
        "claude-sonnet-5"
    );
    assert_eq!(
        profile_json["inferenceModels"][0]["labelOverride"],
        "DeepSeek"
    );
    // 网关配置持久化（重启后端口/token 不漂移）。
    assert!(
        read_state(&h.mcpane_home)["desktopGateway"]["token"]
            .as_str()
            .unwrap()
            .starts_with("mcpane-")
    );

    // 二次切换（direct 另一家再切回 proxy）复用同一 token——Desktop 侧 profile
    // 不必反复失效。
    let outcome2 = h
        .svc
        .switch(ClientApp::ClaudeDesktop, &relay.id, &h.engine, &h.paths)
        .unwrap();
    assert!(outcome2.written);
    let gateway2 = h.svc.desktop_gateway().unwrap().unwrap();
    assert_eq!(gateway.token, gateway2.token);
}

#[cfg(any(target_os = "macos", windows))]
#[test]
fn claude_desktop_proxy_without_routes_fails_without_touching_disk() {
    let h = harness();
    let mut p = ProviderProfile::new_relay(ClientApp::ClaudeDesktop, "Broken");
    p.base_url = Some("https://relay.example.com".into());
    p.desktop_mode = mcpane_provider::DesktopMode::Proxy;
    let relay = h.svc.add_relay(p).unwrap();
    h.svc.set_api_key(&relay.id, &Secret::new("sk")).unwrap();

    let err = h
        .svc
        .switch(ClientApp::ClaudeDesktop, &relay.id, &h.engine, &h.paths)
        .unwrap_err();
    assert!(
        err.to_string().contains("至少需要一个模型路由映射"),
        "{err}"
    );
    // 校验失败在落盘前：4 文件全部不存在。
    assert!(!h.paths.claude_desktop.profile.exists());
    assert!(!h.paths.claude_desktop.normal_config.exists());
}

/// OpenCode 是 additive：启用写进 provider 表、移除只删自己那条，
/// 且它没有官方项——播种时会被跳过。
#[test]
fn opencode_enable_and_remove_are_additive() {
    let h = harness();
    std::fs::create_dir_all(h.paths.opencode_config.parent().unwrap()).unwrap();
    // 用户配置里已有一条他自己写的 provider + 一个 mcp 表。
    std::fs::write(
        &h.paths.opencode_config,
        r#"{"provider":{"mine":{"npm":"@ai-sdk/anthropic"}},"mcp":{"x":{}}}"#,
    )
    .unwrap();

    // 播种不给 OpenCode 建官方项。
    assert!(
        !h.svc
            .list()
            .unwrap()
            .iter()
            .any(|p| p.app == ClientApp::OpenCode),
        "OpenCode 没有官方形态，不该被播种"
    );

    let mut profile = ProviderProfile::new_relay(ClientApp::OpenCode, "Kimi");
    profile.base_url = Some("https://api.moonshot.cn/v1".into());
    // additive 要的是一组模型（专用 models 字段，不再借 Claude 的角色映射字段）。
    profile.models = vec!["kimi-k2.7-code".into(), "kimi-k3".into()];
    let relay = h.svc.add_relay(profile).unwrap();
    h.svc
        .set_api_key(&relay.id, &Secret::new("sk-oc-test"))
        .unwrap();

    // 启用 = 写进 provider 表。
    h.svc
        .switch(ClientApp::OpenCode, &relay.id, &h.engine, &h.paths)
        .unwrap();
    let live: serde_json::Value = read_json(&h.paths.opencode_config);
    assert_eq!(
        live["provider"]["kimi"]["options"]["baseURL"],
        "https://api.moonshot.cn/v1"
    );
    assert_eq!(
        live["provider"]["kimi"]["models"]["kimi-k2.7-code"]["name"],
        "kimi-k2.7-code"
    );
    // 用户自己的 provider 与 mcp 表不受影响。
    assert_eq!(live["provider"]["mine"]["npm"], "@ai-sdk/anthropic");
    assert!(live["mcp"]["x"].is_object());
    assert_eq!(
        h.svc
            .live_provider_ids(ClientApp::OpenCode, &h.paths)
            .unwrap(),
        vec![relay.id.clone()]
    );

    // 移除 = 只从配置里删自己那条，供应商记录仍在列表里。
    h.svc
        .remove_from_live(&relay.id, &h.engine, &h.paths)
        .unwrap();
    let live: serde_json::Value = read_json(&h.paths.opencode_config);
    assert!(
        live["provider"].get("kimi").is_none(),
        "已从配置移除：{live}"
    );
    assert_eq!(live["provider"]["mine"]["npm"], "@ai-sdk/anthropic");
    assert!(
        h.svc.list().unwrap().iter().any(|p| p.id == relay.id),
        "供应商记录保留，可再次启用"
    );
    assert!(
        h.svc
            .live_provider_ids(ClientApp::OpenCode, &h.paths)
            .unwrap()
            .is_empty()
    );
}

/// OpenClaw 与 OpenCode 同为 additive，但配置是 JSON5：写入必须保住用户的
/// 注释与单引号风格，且只动 models.providers 下自己那条。
#[test]
fn openclaw_enable_preserves_json5_style() {
    let h = harness();
    std::fs::create_dir_all(h.paths.openclaw_config.parent().unwrap()).unwrap();
    std::fs::write(
        &h.paths.openclaw_config,
        "{\n  // 我的配置\n  models: {\n    mode: 'merge',\n    providers: {\n      mine: { baseUrl: 'https://x.example.com' },\n    },\n  },\n}\n",
    )
    .unwrap();

    let mut profile = ProviderProfile::new_relay(ClientApp::OpenClaw, "Kimi");
    profile.base_url = Some("https://api.moonshot.cn/v1".into());
    profile.models = vec!["kimi-k2.7-code".into()];
    let relay = h.svc.add_relay(profile).unwrap();
    h.svc
        .set_api_key(&relay.id, &Secret::new("sk-claw-test"))
        .unwrap();
    h.svc
        .switch(ClientApp::OpenClaw, &relay.id, &h.engine, &h.paths)
        .unwrap();

    let live = std::fs::read_to_string(&h.paths.openclaw_config).unwrap();
    assert!(live.contains("// 我的配置"), "JSON5 注释保留：{live}");
    assert!(
        live.contains("mode: 'merge'"),
        "单引号/无引号键不动：{live}"
    );
    assert!(live.contains("kimi"), "已写入：{live}");
    assert!(
        live.contains("https://x.example.com"),
        "用户自建条目保留：{live}"
    );
    assert_eq!(
        h.svc
            .live_provider_ids(ClientApp::OpenClaw, &h.paths)
            .unwrap(),
        vec![relay.id.clone()]
    );

    h.svc
        .remove_from_live(&relay.id, &h.engine, &h.paths)
        .unwrap();
    let live = std::fs::read_to_string(&h.paths.openclaw_config).unwrap();
    assert!(!live.contains("kimi"), "已移除：{live}");
    assert!(live.contains("mine"), "用户自建条目仍在：{live}");
    assert!(live.contains("// 我的配置"), "注释仍在：{live}");
}

/// 上游协议维度：选 Anthropic 时，三家各自映射到自己的字段
/// （OpenCode 换 AI SDK 包，OpenClaw 写 `api`，Hermes 写 `api_mode`）。
#[test]
fn additive_upstream_api_maps_to_each_clients_field() {
    let h = harness();
    for path in [
        &h.paths.opencode_config,
        &h.paths.openclaw_config,
        &h.paths.hermes_config,
    ] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    }
    for app in [ClientApp::OpenCode, ClientApp::OpenClaw, ClientApp::Hermes] {
        let mut profile = ProviderProfile::new_relay(app, "Claude 中转");
        profile.base_url = Some("https://relay.example.com/v1".into());
        profile.models = vec!["claude-sonnet-5".into()];
        profile.upstream_api = Some(mcpane_provider::UpstreamApi::Anthropic);
        let relay = h.svc.add_relay(profile).unwrap();
        h.svc.set_api_key(&relay.id, &Secret::new("sk")).unwrap();
        h.svc.switch(app, &relay.id, &h.engine, &h.paths).unwrap();
    }
    let oc = std::fs::read_to_string(&h.paths.opencode_config).unwrap();
    assert!(oc.contains("@ai-sdk/anthropic"), "OpenCode 换 SDK 包：{oc}");
    assert!(
        oc.contains("setCacheKey"),
        "抄 cc-switch 预设的缓存键开关：{oc}"
    );
    let ocl = std::fs::read_to_string(&h.paths.openclaw_config).unwrap();
    assert!(
        ocl.contains("anthropic-messages"),
        "OpenClaw 写 api 字段：{ocl}"
    );
    let hm = std::fs::read_to_string(&h.paths.hermes_config).unwrap();
    assert!(
        hm.contains("api_mode: anthropic_messages"),
        "Hermes 写 api_mode：{hm}"
    );
}

/// Hermes 的 provider 条目上还有一批我们不管的键（用户可能在 Hermes Web UI
/// 里设过）。整块改写数组时必须原样带回去——对齐 cc-switch `set_provider`。
#[test]
fn hermes_upsert_preserves_user_only_fields() {
    let h = harness();
    std::fs::create_dir_all(h.paths.hermes_config.parent().unwrap()).unwrap();
    let mut profile = ProviderProfile::new_relay(ClientApp::Hermes, "Acme");
    profile.base_url = Some("https://old.example.com".into());
    profile.models = vec!["m".into()];
    let relay = h.svc.add_relay(profile).unwrap();
    h.svc.set_api_key(&relay.id, &Secret::new("sk")).unwrap();
    h.svc
        .switch(ClientApp::Hermes, &relay.id, &h.engine, &h.paths)
        .unwrap();

    // 用户随后在 Hermes Web UI 里给这条加了两个我们不认的键：按 `base_url:`
    // 那行的缩进插进同一个数组元素，模拟真实的手工/Web UI 编辑。
    let live = std::fs::read_to_string(&h.paths.hermes_config).unwrap();
    let mut lines: Vec<String> = Vec::new();
    for line in live.lines() {
        lines.push(line.to_owned());
        if line.trim_start().starts_with("base_url:") {
            let indent = " ".repeat(line.len() - line.trim_start().len());
            lines.push(format!("{indent}request_timeout_seconds: 300"));
            lines.push(format!("{indent}rate_limit_delay: 2"));
        }
    }
    std::fs::write(&h.paths.hermes_config, format!("{}\n", lines.join("\n"))).unwrap();

    // 我们改 base_url 再写一次：自己的键更新，用户的键必须还在。
    let mut edited = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .find(|p| p.id == relay.id)
        .unwrap();
    edited.base_url = Some("https://new.example.com".into());
    h.svc.update_relay(edited, &h.engine, &h.paths).unwrap();
    h.svc
        .switch(ClientApp::Hermes, &relay.id, &h.engine, &h.paths)
        .unwrap();

    let live = std::fs::read_to_string(&h.paths.hermes_config).unwrap();
    assert!(live.contains("new.example.com"), "我们的键已更新：{live}");
    assert!(
        live.contains("request_timeout_seconds"),
        "用户设的键不能被抹掉：{live}"
    );
    assert!(live.contains("rate_limit_delay"), "同上：{live}");
}

/// Hermes v12+ `providers:` 字典：外部条目只读列出、同名启用给遮蔽警告。
#[test]
fn hermes_dict_providers_are_listed_and_shadow_warns() {
    let h = harness();
    std::fs::create_dir_all(h.paths.hermes_config.parent().unwrap()).unwrap();
    std::fs::write(
        &h.paths.hermes_config,
        "providers:\n  webui-one:\n    base_url: https://from-webui\n  acme:\n    base_url: https://dict-acme\n",
    )
    .unwrap();

    // 外部条目（Web UI 托管）能被列出，含 base_url。
    let external = h.svc.hermes_external_providers(&h.paths).unwrap();
    assert_eq!(external.len(), 2);
    assert!(
        external
            .iter()
            .any(|e| e.name == "webui-one" && e.base_url.as_deref() == Some("https://from-webui"))
    );

    // 启用同名 profile：不阻断，但要有遮蔽警告；启用后该键被数组遮蔽、
    // 不再出现在外部条目里。
    let mut profile = ProviderProfile::new_relay(ClientApp::Hermes, "acme");
    profile.base_url = Some("https://mine.example.com".into());
    profile.models = vec!["m".into()];
    let relay = h.svc.add_relay(profile).unwrap();
    h.svc.set_api_key(&relay.id, &Secret::new("sk")).unwrap();
    let outcome = h
        .svc
        .switch(ClientApp::Hermes, &relay.id, &h.engine, &h.paths)
        .unwrap();
    assert!(
        outcome.warnings.iter().any(|w| w.contains("遮蔽")),
        "同名启用应产生遮蔽警告：{:?}",
        outcome.warnings
    );
    let external = h.svc.hermes_external_providers(&h.paths).unwrap();
    assert!(
        external.iter().all(|e| e.name != "acme"),
        "被数组遮蔽的字典键不得再出现"
    );
    assert!(external.iter().any(|e| e.name == "webui-one"));

    // 文件缺失/解析失败容错为空表（与 live_provider_ids 同口径）。
    std::fs::write(&h.paths.hermes_config, "providers: [broken").unwrap();
    assert!(
        h.svc
            .hermes_external_providers(&h.paths)
            .unwrap()
            .is_empty()
    );
}

/// OpenClaw：光写 provider 表等于"这家可用"，还得把模型注册进目录并在默认
/// 模型未设时指过去，否则用户启用完 OpenClaw 毫无变化（对齐 cc-switch）。
#[test]
fn openclaw_enable_registers_model_and_claims_unset_default() {
    let h = harness();
    std::fs::create_dir_all(h.paths.openclaw_config.parent().unwrap()).unwrap();
    let mut profile = ProviderProfile::new_relay(ClientApp::OpenClaw, "Kimi");
    profile.base_url = Some("https://api.moonshot.cn/v1".into());
    profile.models = vec!["kimi-k2.7-code".into()];
    profile.context_length = Some(262_144);
    let first = h.svc.add_relay(profile).unwrap();
    h.svc.set_api_key(&first.id, &Secret::new("sk")).unwrap();
    h.svc
        .switch(ClientApp::OpenClaw, &first.id, &h.engine, &h.paths)
        .unwrap();

    let live = read_json(&h.paths.openclaw_config);
    let entry_key = live["models"]["providers"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    assert_eq!(
        live["models"]["providers"][&entry_key]["models"][0]["contextWindow"], 262_144,
        "上下文窗口必须落到模型条目：{live}"
    );
    let reference = format!("{entry_key}/kimi-k2.7-code");
    assert_eq!(live["agents"]["defaults"]["model"]["primary"], reference);
    assert!(live["agents"]["defaults"]["models"][&reference].is_object());

    // 第二家：目录要注册，但已设的默认模型不能被抢。
    let mut second = ProviderProfile::new_relay(ClientApp::OpenClaw, "Other");
    second.base_url = Some("https://other.example.com/v1".into());
    second.models = vec!["other-model".into()];
    let second = h.svc.add_relay(second).unwrap();
    h.svc.set_api_key(&second.id, &Secret::new("sk2")).unwrap();
    h.svc
        .switch(ClientApp::OpenClaw, &second.id, &h.engine, &h.paths)
        .unwrap();
    let live = read_json(&h.paths.openclaw_config);
    assert_eq!(
        live["agents"]["defaults"]["model"]["primary"], reference,
        "已设的默认模型不能被后来的供应商抢走：{live}"
    );

    // 移除第一家：它的目录条目与指向它的指针一起清掉，别留死引用。
    h.svc
        .remove_from_live(&first.id, &h.engine, &h.paths)
        .unwrap();
    let live = read_json(&h.paths.openclaw_config);
    assert!(
        live["agents"]["defaults"]["models"]
            .get(&reference)
            .is_none(),
        "目录条目应随之清除：{live}"
    );
    assert!(
        live["agents"]["defaults"]["model"].get("primary").is_none(),
        "指向已移除供应商的默认模型指针必须清掉：{live}"
    );
    // "second" 的目录条目还在，两张表本身都不该被清（还有别家在用）。
    assert!(
        live["agents"]["defaults"]["models"]
            .as_object()
            .is_some_and(|m| !m.is_empty()),
        "别家（second）的目录条目还在，表本身不该被清掉：{live}"
    );

    // 顺带清理（P0-3 附带修复）：连 second 也移除后，两张表本身（不只是
    // 条目）都不该再留 `{}`/`{"model":{}}` 空壳。
    h.svc
        .remove_from_live(&second.id, &h.engine, &h.paths)
        .unwrap();
    let live = read_json(&h.paths.openclaw_config);
    assert!(
        live["agents"]["defaults"].get("models").is_none(),
        "最后一条目录条目删完后，目录表本身也该消失，而不是留空对象：{live}"
    );
    assert!(
        live["agents"]["defaults"].get("model").is_none(),
        "最后一个默认模型指针删完后，指针表本身也该消失：{live}"
    );
}

/// 升级回退：`models` 字段加入之前建的 additive 供应商只有单值 `model`，
/// 仍要能启用——否则用户升级后原有条目突然全部失效。
#[test]
fn additive_falls_back_to_single_model_for_legacy_profiles() {
    let h = harness();
    std::fs::create_dir_all(h.paths.opencode_config.parent().unwrap()).unwrap();
    let mut profile = ProviderProfile::new_relay(ClientApp::OpenCode, "Legacy");
    profile.base_url = Some("https://api.example.com/v1".into());
    profile.model = Some("legacy-model".into()); // 老形态：models 为空
    let relay = h.svc.add_relay(profile).unwrap();
    h.svc.set_api_key(&relay.id, &Secret::new("sk")).unwrap();
    h.svc
        .switch(ClientApp::OpenCode, &relay.id, &h.engine, &h.paths)
        .unwrap();
    let live: serde_json::Value = read_json(&h.paths.opencode_config);
    assert_eq!(
        live["provider"]["legacy"]["models"]["legacy-model"]["name"],
        "legacy-model"
    );
}

/// additive：从配置移除后再删除记录，两步都不该报错（移除是幂等的）。
/// 命令层 provider_delete 依赖这个幂等性——它删记录前会先尝试摘掉 live 条目。
#[test]
fn remove_from_live_is_idempotent_before_delete() {
    let h = harness();
    std::fs::create_dir_all(h.paths.opencode_config.parent().unwrap()).unwrap();
    let mut profile = ProviderProfile::new_relay(ClientApp::OpenCode, "Kimi");
    profile.base_url = Some("https://api.moonshot.cn/v1".into());
    profile.model = Some("kimi-k2.7-code".into());
    let relay = h.svc.add_relay(profile).unwrap();
    h.svc.set_api_key(&relay.id, &Secret::new("sk")).unwrap();
    h.svc
        .switch(ClientApp::OpenCode, &relay.id, &h.engine, &h.paths)
        .unwrap();

    h.svc
        .remove_from_live(&relay.id, &h.engine, &h.paths)
        .unwrap();
    // 再摘一次：条目已不在，仍要成功返回（no-op）。
    h.svc
        .remove_from_live(&relay.id, &h.engine, &h.paths)
        .unwrap();
    h.svc.delete(&relay.id).unwrap();
    assert!(!h.svc.list().unwrap().iter().any(|p| p.id == relay.id));
}

/// Hermes：additive + YAML + 数组容器。启用按 name 就地 upsert，
/// 移除只摘自己那条，其他键与用户自建条目不受影响。
#[test]
fn hermes_enable_and_remove_handle_array_container() {
    let h = harness();
    std::fs::create_dir_all(h.paths.hermes_config.parent().unwrap()).unwrap();
    std::fs::write(
        &h.paths.hermes_config,
        "agent:\n  max_turns: 50\ncustom_providers:\n  - name: mine\n    base_url: https://x.example.com\n",
    )
    .unwrap();

    let mut profile = ProviderProfile::new_relay(ClientApp::Hermes, "Kimi");
    profile.base_url = Some("https://api.moonshot.cn/v1".into());
    profile.models = vec!["kimi-k2.7-code".into()];
    let relay = h.svc.add_relay(profile).unwrap();
    h.svc
        .set_api_key(&relay.id, &Secret::new("sk-hermes-test"))
        .unwrap();
    h.svc
        .switch(ClientApp::Hermes, &relay.id, &h.engine, &h.paths)
        .unwrap();

    let live = std::fs::read_to_string(&h.paths.hermes_config).unwrap();
    assert!(live.contains("kimi"), "已写入：{live}");
    assert!(live.contains("mine"), "用户自建条目保留：{live}");
    assert!(live.contains("max_turns"), "其他配置不动：{live}");
    assert_eq!(
        h.svc
            .live_provider_ids(ClientApp::Hermes, &h.paths)
            .unwrap(),
        vec![relay.id.clone()]
    );

    // 快照不留明文 Key。
    let snapshots = std::fs::read_dir(h.mcpane_home.join("snapshots"))
        .map(|dir| {
            dir.filter_map(Result::ok)
                .filter_map(|e| std::fs::read_to_string(e.path()).ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert!(
        snapshots.iter().all(|c| !c.contains("sk-hermes-test")),
        "写前快照必须脱敏"
    );

    h.svc
        .remove_from_live(&relay.id, &h.engine, &h.paths)
        .unwrap();
    let live = std::fs::read_to_string(&h.paths.hermes_config).unwrap();
    assert!(!live.contains("kimi"), "已移除：{live}");
    assert!(live.contains("mine"), "别人的条目仍在：{live}");
}

/// Claude Desktop 的模式与路由必须能被编辑保存——此前 `update_relay` 漏了这两个
/// 字段，用户改完点保存等于什么都没发生。
#[test]
fn desktop_mode_and_routes_survive_update() {
    let h = harness();
    let mut profile = ProviderProfile::new_relay(ClientApp::ClaudeDesktop, "D");
    profile.base_url = Some("https://d.example.com".into());
    let created = h.svc.add_relay(profile).unwrap();

    let mut edited = created.clone();
    edited.desktop_mode = mcpane_provider::DesktopMode::Proxy;
    edited.desktop_routes = vec![mcpane_provider::DesktopRoute {
        route_id: "claude-sonnet-5".into(),
        upstream_model: "deepseek-chat".into(),
        label_override: Some("DeepSeek".into()),
        supports_1m: false,
    }];
    h.svc.update_relay(edited, &h.engine, &h.paths).unwrap();

    let stored = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .find(|p| p.id == created.id)
        .unwrap();
    assert_eq!(stored.desktop_mode, mcpane_provider::DesktopMode::Proxy);
    assert_eq!(stored.desktop_routes.len(), 1);
    assert_eq!(stored.desktop_routes[0].upstream_model, "deepseek-chat");
}

/// 自定义请求头：两家落到各自字段（OpenCode 在 options.headers、OpenClaw 在
/// headers），清空后该键消失而不是留个空对象。
#[test]
fn additive_headers_land_in_each_clients_field() {
    let h = harness();
    for path in [&h.paths.opencode_config, &h.paths.openclaw_config] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    }
    for app in [ClientApp::OpenCode, ClientApp::OpenClaw] {
        let mut profile = ProviderProfile::new_relay(app, "H");
        profile.base_url = Some("https://relay.example.com/v1".into());
        profile.models = vec!["m".into()];
        profile.headers = [("X-Route".to_owned(), "fast".to_owned())]
            .into_iter()
            .collect();
        let relay = h.svc.add_relay(profile).unwrap();
        h.svc.set_api_key(&relay.id, &Secret::new("sk")).unwrap();
        h.svc.switch(app, &relay.id, &h.engine, &h.paths).unwrap();

        let live = std::fs::read_to_string(match app {
            ClientApp::OpenCode => &h.paths.opencode_config,
            _ => &h.paths.openclaw_config,
        })
        .unwrap();
        assert!(live.contains("X-Route"), "{app:?} 应写入自定义头：{live}");

        // 清空后重新启用：该键要消失。
        let mut cleared = h
            .svc
            .list()
            .unwrap()
            .into_iter()
            .find(|p| p.id == relay.id)
            .unwrap();
        cleared.headers.clear();
        h.svc.update_relay(cleared, &h.engine, &h.paths).unwrap();
        h.svc.switch(app, &relay.id, &h.engine, &h.paths).unwrap();
        let live = std::fs::read_to_string(match app {
            ClientApp::OpenCode => &h.paths.opencode_config,
            _ => &h.paths.openclaw_config,
        })
        .unwrap();
        assert!(
            !live.contains("X-Route"),
            "{app:?} 清空后不该留残留：{live}"
        );
    }
}

/// 配置目录覆盖：设了就存、留空就清；非绝对路径与不存在的目录当场拒绝，
/// Claude Desktop（四文件跨两目录）不支持覆盖。
#[test]
fn config_dir_override_validates_and_round_trips() {
    let h = harness();
    let dir = tempfile::tempdir().unwrap();
    let abs = dir.path().display().to_string();

    h.svc
        .set_config_dir_override(ClientApp::Claude, &abs)
        .unwrap();
    assert_eq!(
        h.svc
            .config_dir_overrides()
            .unwrap()
            .get(&ClientApp::Claude),
        Some(&abs)
    );

    // 空串 = 回到默认位置。
    h.svc
        .set_config_dir_override(ClientApp::Claude, "  ")
        .unwrap();
    assert!(h.svc.config_dir_overrides().unwrap().is_empty());

    // 相对路径 / 不存在的目录都不能静默生效——那会让配置全写到别处。
    assert!(
        h.svc
            .set_config_dir_override(ClientApp::Claude, "relative/path")
            .is_err()
    );
    assert!(
        h.svc
            .set_config_dir_override(ClientApp::Claude, "/definitely/not/here/42")
            .is_err()
    );
    // Desktop 的四个文件跨两个目录，整体覆盖没有意义，在这层就挡住。
    assert!(
        h.svc
            .set_config_dir_override(ClientApp::ClaudeDesktop, &abs)
            .is_err()
    );
}

/// 非 additive 客户端不能走「从配置移除」——它们要靠切换到别的供应商。
#[test]
fn remove_from_live_rejects_switch_style_clients() {
    let h = harness();
    let relay = add_relay(&h.svc, ClientApp::Claude, "R", "https://r.example.com");
    let err = h
        .svc
        .remove_from_live(&relay.id, &h.engine, &h.paths)
        .unwrap_err();
    assert!(err.to_string().contains("additive"), "{err}");
}

/// Grok CLI：切中转站写模型表 + 默认指针；切回官方清掉我们的表，
/// 但用户自建的表与他自己设的默认模型都不能动。
#[test]
fn grok_switch_writes_model_table_and_restores_official() {
    let h = harness();
    std::fs::create_dir_all(h.paths.grok_config.parent().unwrap()).unwrap();
    // 用户已有一张自建表 + 自己的默认模型。
    std::fs::write(
        &h.paths.grok_config,
        "[models]\ndefault = \"my-own\"\n\n[model.my-own]\nmodel = \"grok-4\"\n",
    )
    .unwrap();

    let relay = add_relay(
        &h.svc,
        ClientApp::Grok,
        "My Relay",
        "https://relay.example.com/v1",
    );
    h.svc
        .set_api_key(&relay.id, &Secret::new("sk-grok-test"))
        .unwrap();
    h.svc
        .switch(ClientApp::Grok, &relay.id, &h.engine, &h.paths)
        .unwrap();

    let live = std::fs::read_to_string(&h.paths.grok_config).unwrap();
    assert!(live.contains("mcpane-my-relay"), "写入受管模型表：{live}");
    assert!(live.contains("https://relay.example.com/v1"), "{live}");
    assert!(
        live.contains("sk-grok-test"),
        "Key 明文写进 config.toml（Grok 无独立凭据文件）"
    );
    assert!(
        live.contains("context_window"),
        "Grok CLI 要求正整数 context_window：{live}"
    );

    // 快照不能留明文 Key。
    let snapshots = std::fs::read_dir(h.mcpane_home.join("snapshots"))
        .map(|dir| {
            dir.filter_map(Result::ok)
                .filter_map(|e| std::fs::read_to_string(e.path()).ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert!(
        snapshots
            .iter()
            .all(|content| !content.contains("sk-grok-test")),
        "写前快照必须脱敏"
    );

    // 切回官方：我们的表清掉，用户自建表原样保留，且 default 指针必须还原成
    // 用户接入 MCPane 之前自己设的 "my-own"——不是清空（P0-3 回归测试：早期
    // 实现在这里无条件删除指针，永久丢失用户自己的默认模型选择）。
    h.svc
        .switch(ClientApp::Grok, "official-grok", &h.engine, &h.paths)
        .unwrap();
    let live = std::fs::read_to_string(&h.paths.grok_config).unwrap();
    assert!(!live.contains("mcpane-my-relay"), "受管表已清理：{live}");
    assert!(
        live.contains("[model.my-own]") || live.contains("my-own"),
        "用户自建表保留：{live}"
    );
    let doc =
        mcpane_config::ConfigDocument::parse(&live, mcpane_config::ConfigFormat::Toml).unwrap();
    assert_eq!(
        doc.get(&["models", "default"]).unwrap(),
        "my-own",
        "断开后必须还原用户接入前的默认模型，而不是清空：{live}"
    );
}

/// P0-3 补充场景：在两个 MCPane 中转站之间来回切换（A→B），中间从未真正
/// "切回官方"——`find_prior_grok_default` 必须跳过 A→B 这次切换自己的快照
/// （它的 default 也是 mcpane 表），一路找到最初"用户自己的默认"那条边界，
/// 而不是错误地把 A 的中转站表名当成"原值"。
#[test]
fn grok_restores_original_default_across_multiple_relay_switches() {
    let h = harness();
    std::fs::create_dir_all(h.paths.grok_config.parent().unwrap()).unwrap();
    std::fs::write(
        &h.paths.grok_config,
        "[models]\ndefault = \"my-own\"\n\n[model.my-own]\nmodel = \"grok-4\"\n",
    )
    .unwrap();

    let relay_a = add_relay(
        &h.svc,
        ClientApp::Grok,
        "Relay A",
        "https://a.example.com/v1",
    );
    h.svc
        .set_api_key(&relay_a.id, &Secret::new("sk-a"))
        .unwrap();
    h.svc
        .switch(ClientApp::Grok, &relay_a.id, &h.engine, &h.paths)
        .unwrap();

    let relay_b = add_relay(
        &h.svc,
        ClientApp::Grok,
        "Relay B",
        "https://b.example.com/v1",
    );
    h.svc
        .set_api_key(&relay_b.id, &Secret::new("sk-b"))
        .unwrap();
    h.svc
        .switch(ClientApp::Grok, &relay_b.id, &h.engine, &h.paths)
        .unwrap();

    h.svc
        .switch(ClientApp::Grok, "official-grok", &h.engine, &h.paths)
        .unwrap();
    let live = std::fs::read_to_string(&h.paths.grok_config).unwrap();
    let doc =
        mcpane_config::ConfigDocument::parse(&live, mcpane_config::ConfigFormat::Toml).unwrap();
    assert_eq!(
        doc.get(&["models", "default"]).unwrap(),
        "my-own",
        "经过 A→B 两次中转站切换后，切回官方仍要落到最初的用户原值：{live}"
    );
    assert!(!live.contains("mcpane-"), "所有受管表都应清理：{live}");
}

/// P0-3 边界：用户从未设过 default（首次装 Grok 就直接连 MCPane），切回
/// 官方时应该清空指针，而不是凭空造一个值——`find_prior_grok_default` 在
/// 这种情况下必须正确返回 None。
#[test]
fn grok_clears_default_when_there_was_never_a_user_default() {
    let h = harness();
    std::fs::create_dir_all(h.paths.grok_config.parent().unwrap()).unwrap();
    // 空文件：模拟 Grok CLI 装了但从没配过自定义模型。

    let relay = add_relay(
        &h.svc,
        ClientApp::Grok,
        "My Relay",
        "https://relay.example.com/v1",
    );
    h.svc
        .set_api_key(&relay.id, &Secret::new("sk-grok"))
        .unwrap();
    h.svc
        .switch(ClientApp::Grok, &relay.id, &h.engine, &h.paths)
        .unwrap();
    h.svc
        .switch(ClientApp::Grok, "official-grok", &h.engine, &h.paths)
        .unwrap();

    let live = std::fs::read_to_string(&h.paths.grok_config).unwrap();
    let doc =
        mcpane_config::ConfigDocument::parse(&live, mcpane_config::ConfigFormat::Toml).unwrap();
    assert!(
        doc.get(&["models", "default"]).is_none(),
        "从未有过用户原值时应清空指针，而不是保留/捏造一个：{live}"
    );
}

/// Grok 的模型表名由展示名派生，映射是**多对一**的（`My Relay` / `my-relay`
/// 都归一到 `mcpane-my-relay`）。这条锁住的是：撞名**不会**让两家的存档串号。
///
/// 之所以值得单独锁一条：多对一映射看着就像个"后写的覆盖先写的凭据"的坑，
/// 很容易被想当然地当成缺陷去"修"。实际不然——`[model.<slug>]` 是「当前生效
/// 中转站」的单一槽位，切过去本来就该由新的那家覆写；而切走前的 `backfill`
/// 只在 `[models].default` 正指向自己算出的 slug 时才读表，读到的必然是它
/// 自己上一次写进去的值。A→B→A 来回切之后，两边存档都必须原封不动。
#[test]
fn grok_colliding_display_names_do_not_cross_contaminate() {
    let h = harness();
    std::fs::create_dir_all(h.paths.grok_config.parent().unwrap()).unwrap();
    std::fs::write(
        &h.paths.grok_config,
        "[models]\ndefault = \"my-own\"\n\n[model.my-own]\nmodel = \"grok-4\"\n",
    )
    .unwrap();

    let a = add_relay(
        &h.svc,
        ClientApp::Grok,
        "My Relay",
        "https://a.example.com/v1",
    );
    let b = add_relay(
        &h.svc,
        ClientApp::Grok,
        "my-relay",
        "https://b.example.com/v1",
    );
    assert_eq!(
        mcpane_adapter_grok::table_slug("My Relay"),
        mcpane_adapter_grok::table_slug("my-relay"),
        "前提：两个展示名确实派生同一张表，否则这条测试没有意义"
    );
    h.svc.set_api_key(&a.id, &Secret::new("sk-AAA")).unwrap();
    h.svc.set_api_key(&b.id, &Secret::new("sk-BBB")).unwrap();

    for target in [&a.id, &b.id, &a.id] {
        h.svc
            .switch(ClientApp::Grok, target, &h.engine, &h.paths)
            .unwrap();
    }

    let state = read_state(&h.mcpane_home);
    let stored = |id: &str| -> serde_json::Value {
        state["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .cloned()
            .unwrap()
    };
    assert_eq!(
        stored(&a.id)["baseUrl"],
        "https://a.example.com/v1",
        "A 的存档不得被撞名的 B 串改"
    );
    assert_eq!(
        stored(&b.id)["baseUrl"],
        "https://b.example.com/v1",
        "B 的存档不得被撞名的 A 串改"
    );
    // live 是 A（最后切过去的那家），凭据也必须是 A 的。
    let live = std::fs::read_to_string(&h.paths.grok_config).unwrap();
    assert!(live.contains("https://a.example.com/v1"), "{live}");
    assert!(!live.contains("sk-BBB"), "切走的 B 的 Key 不得残留：{live}");
}

/// 官方项已落库 → 与中转站同权：可删除，且删除后不会被下一次播种复活。
#[test]
fn official_seed_does_not_revive_deleted() {
    let h = harness(); // harness 已按"所有客户端都装了"播种
    h.svc
        .delete(mcpane_provider::OFFICIAL_CLAUDE_DESKTOP_ID)
        .unwrap();
    h.svc.delete("official-gemini").unwrap();

    // 再次播种（模拟下次启动）：已记账的 app 不重播，删掉的官方项保持删除态。
    let seeded = h.svc.seed_officials(&ClientApp::ALL).unwrap();
    assert!(seeded.is_empty(), "已播种过的 app 不重复播种");
    let ids: Vec<String> = h.svc.list().unwrap().into_iter().map(|p| p.id).collect();
    assert!(
        !ids.iter()
            .any(|id| id == mcpane_provider::OFFICIAL_CLAUDE_DESKTOP_ID)
    );
    assert!(!ids.iter().any(|id| id == "official-gemini"));

    // 用户可从"添加供应商"把官方项加回来（幂等）。
    let restored = h.svc.ensure_official(ClientApp::Gemini).unwrap();
    assert_eq!(restored.id, "official-gemini");
    assert_eq!(restored.category, ProviderCategory::Official);
    let again = h.svc.ensure_official(ClientApp::Gemini).unwrap();
    assert_eq!(again.id, restored.id);
    assert_eq!(
        h.svc
            .list()
            .unwrap()
            .iter()
            .filter(|p| p.id == "official-gemini")
            .count(),
        1,
        "重复 ensure 不产生副本"
    );
}

/// 升级路径：老状态里 current 已指向 official-*（那时官方项不落库）。即使探测说
/// 该客户端没装，也必须补种——否则 current 指向一个不存在的 provider，切换必炸。
#[test]
fn official_seed_covers_app_whose_current_is_official() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = serde_json::json!({
        "schemaVersion": 2,
        "providers": [],
        "current": { "gemini": "official-gemini" }
    });
    std::fs::write(
        dir.path().join("providers.json"),
        serde_json::to_string_pretty(&legacy).unwrap(),
    )
    .unwrap();

    let svc = ProviderService::new(
        JsonProviderStore::new(dir.path()),
        Arc::new(InMemorySecretStore::default()),
    );
    // 探测结果里没有 Gemini（本机确实没装 CLI），但 current 指着它。
    let seeded = svc.seed_officials(&[ClientApp::Claude]).unwrap();
    assert!(
        seeded.contains(&ClientApp::Gemini),
        "current 指向的官方项必须补种"
    );
    assert!(
        svc.list()
            .unwrap()
            .iter()
            .any(|p| p.id == "official-gemini"),
        "补种后 current 不再指向空气"
    );
}

/// 升级路径：老 providers.json 里的 officialOverrides 在播种时迁进官方 Profile，
/// 用户此前给官方订阅配过的附加片段不会因为改模型而丢失。
#[test]
fn official_seed_migrates_legacy_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = serde_json::json!({
        "schemaVersion": 2,
        "providers": [],
        "officialOverrides": {
            "claude": {
                "extraSettings": { "env": { "MY_FLAG": "1" } },
                "commonConfigEnabled": true
            }
        }
    });
    std::fs::write(
        dir.path().join("providers.json"),
        serde_json::to_string_pretty(&legacy).unwrap(),
    )
    .unwrap();

    let svc = ProviderService::new(
        JsonProviderStore::new(dir.path()),
        Arc::new(InMemorySecretStore::default()),
    );
    svc.seed_officials(&[ClientApp::Claude]).unwrap();

    let official = svc
        .list()
        .unwrap()
        .into_iter()
        .find(|p| p.id == OFFICIAL_CLAUDE_ID)
        .expect("官方项已播种");
    assert_eq!(official.extra_settings["env"]["MY_FLAG"], "1");
    assert!(official.common_config_enabled);
    // 迁移完即清空，不留双份事实源。
    let state = read_state(dir.path());
    assert!(
        state.get("officialOverrides").is_none(),
        "迁移后不再写出历史字段：{state}"
    );
}

/// 没装的客户端不播种官方项——用户没装 Gemini CLI 就不该看到"Google 官方登录"。
#[test]
fn official_seed_skips_missing_clients() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = Arc::new(InMemorySecretStore::default());
    let svc = ProviderService::new(JsonProviderStore::new(dir.path()), secrets);

    let seeded = svc.seed_officials(&[ClientApp::Claude]).unwrap();
    assert_eq!(seeded, vec![ClientApp::Claude]);
    let ids: Vec<String> = svc.list().unwrap().into_iter().map(|p| p.id).collect();
    assert_eq!(ids, vec![OFFICIAL_CLAUDE_ID.to_owned()]);

    // 之后用户装上 Gemini CLI → 下次启动补种（按 app 记账，不是全局 flag）。
    let seeded = svc
        .seed_officials(&[ClientApp::Claude, ClientApp::Gemini])
        .unwrap();
    assert_eq!(seeded, vec![ClientApp::Gemini]);
    assert_eq!(svc.list().unwrap().len(), 2);
}

#[test]
fn import_desktop_from_claude_derives_mode_per_provider() {
    let h = harness();
    // ① 角色模型全 claude-safe → direct 透传。
    let mut safe = ProviderProfile::new_relay(ClientApp::Claude, "Compute Token");
    safe.base_url = Some("https://computetoken.ai".into());
    safe.model_sonnet = Some("claude-sonnet-5".into());
    safe.model_opus = Some("claude-opus-4-8".into());
    let safe = h.svc.add_relay(safe).unwrap();
    h.svc
        .set_api_key(&safe.id, &Secret::new("sk-safe"))
        .unwrap();
    // ② 品牌模型（含 [1M] 后缀）→ proxy + 路由推导 + supports_1m 翻译。
    let mut branded = ProviderProfile::new_relay(ClientApp::Claude, "DeepSeek");
    branded.base_url = Some("https://api.deepseek.com/anthropic".into());
    branded.model = Some("deepseek-v4-pro[1M]".into());
    branded.model_haiku = Some("deepseek-v4-flash".into());
    h.svc.add_relay(branded).unwrap();
    // （无 base_url 的中转站 add_relay 层已拒绝，导入侧的空值跳过为纯防御分支。）

    let imported = h.svc.import_desktop_from_claude().unwrap();
    assert_eq!(imported, 2);

    let desktops: Vec<ProviderProfile> = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .filter(|p| p.app == ClientApp::ClaudeDesktop && p.category == ProviderCategory::Relay)
        .collect();
    assert_eq!(desktops.len(), 2);

    let direct = desktops.iter().find(|p| p.name == "Compute Token").unwrap();
    assert_eq!(direct.desktop_mode, mcpane_provider::DesktopMode::Direct);
    assert!(direct.desktop_routes.is_empty());
    // Key 复制为独立副本（引用不同、明文相同）。
    assert_ne!(direct.api_key_ref, safe.api_key_ref);
    let copied = h.secrets.get(direct.api_key_ref.as_ref().unwrap()).unwrap();
    assert_eq!(copied.expose(), "sk-safe");

    let proxied = desktops.iter().find(|p| p.name == "DeepSeek").unwrap();
    assert_eq!(proxied.desktop_mode, mcpane_provider::DesktopMode::Proxy);
    // sonnet/opus 回落兜底模型并剥 [1M]；haiku 用显式覆盖。
    // 导入路由恒声明 1M（cc-switch effective_supports_1m 对普通供应商默认 true）。
    let sonnet = proxied
        .desktop_routes
        .iter()
        .find(|r| r.route_id == "claude-sonnet-5")
        .unwrap();
    assert_eq!(sonnet.upstream_model, "deepseek-v4-pro");
    assert!(sonnet.supports_1m);
    let haiku = proxied
        .desktop_routes
        .iter()
        .find(|r| r.route_id == "claude-haiku-4-5")
        .unwrap();
    assert_eq!(haiku.upstream_model, "deepseek-v4-flash");
    assert!(haiku.supports_1m);

    // 幂等：再次导入同端点全部跳过。
    assert_eq!(h.svc.import_desktop_from_claude().unwrap(), 0);
}

// ---------- 批量收编（cc-switch 迁移） ----------

fn incoming(
    app: ClientApp,
    name: &str,
    base_url: &str,
    key: &str,
) -> (ProviderProfile, Option<String>) {
    let mut p = ProviderProfile::new_relay(app, name);
    p.base_url = Some(base_url.into());
    (p, Some(key.into()))
}

#[test]
fn import_relays_stores_keys_and_dedupes() {
    let h = harness();
    let outcome = h
        .svc
        .import_relays(vec![
            incoming(ClientApp::Claude, "中转甲", "https://a.test", "sk-a"),
            incoming(ClientApp::Codex, "中转乙", "https://b.test", "sk-b"),
        ])
        .unwrap();
    assert_eq!(outcome.imported, vec!["中转甲", "中转乙"]);
    assert!(outcome.skipped.is_empty() && outcome.failed.is_empty());

    let relays: Vec<_> = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .filter(|p| p.category == ProviderCategory::Relay)
        .collect();
    assert_eq!(relays.len(), 2);

    // 明文 Key 只进凭据库，落盘的 providers.json 里一个字符都不该有。
    let claude = relays.iter().find(|p| p.name == "中转甲").unwrap();
    let stored = h.secrets.get(claude.api_key_ref.as_ref().unwrap()).unwrap();
    assert_eq!(stored.expose(), "sk-a");
    assert!(!read_state(&h.mcpane_home).to_string().contains("sk-a"));

    // Codex 要拿到自己的表名，否则切走时无从清理。
    let codex = relays.iter().find(|p| p.name == "中转乙").unwrap();
    assert!(codex.codex_slug.is_some());

    // 幂等：同 app 同 base_url 再来一遍全部跳过。
    let again = h
        .svc
        .import_relays(vec![incoming(
            ClientApp::Claude,
            "换个名字同一家",
            "https://a.test",
            "sk-a",
        )])
        .unwrap();
    assert_eq!(again.skipped, vec!["换个名字同一家"]);
    assert_eq!(again.imported.len(), 0);
}

/// 附加片段带了受管键时丢片段、留供应商——base_url 与 Key 才是搬不动的东西。
#[test]
fn import_relays_drops_bad_fragment_but_keeps_provider() {
    let h = harness();
    let (mut profile, key) = incoming(ClientApp::Claude, "带受管键", "https://c.test", "sk-c");
    profile.extra_settings =
        serde_json::json!({ "env": { "ANTHROPIC_BASE_URL": "https://hijack" } });
    let outcome = h.svc.import_relays(vec![(profile, key)]).unwrap();
    assert_eq!(outcome.imported, vec!["带受管键"]);

    let imported = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .find(|p| p.name == "带受管键")
        .unwrap();
    assert_eq!(imported.base_url.as_deref(), Some("https://c.test"));
    assert!(imported.extra_settings.is_null());
}

/// 没有 base_url 的条目搬不动，但不能拖累同一批里的其他条目。
#[test]
fn import_relays_reports_failures_without_aborting() {
    let h = harness();
    let broken = (ProviderProfile::new_relay(ClientApp::Claude, "空壳"), None);
    let outcome = h
        .svc
        .import_relays(vec![
            broken,
            incoming(ClientApp::Claude, "好的那条", "https://d.test", "sk-d"),
        ])
        .unwrap();
    assert_eq!(outcome.imported, vec!["好的那条"]);
    assert_eq!(outcome.failed.len(), 1);
    assert!(outcome.failed[0].starts_with("空壳："));
}

/// 排在已有条目之后，不与用户手排的顺序抢位置。
#[test]
fn import_relays_appends_after_existing_order() {
    let h = harness();
    add_relay(&h.svc, ClientApp::Claude, "先有的", "https://first.test");
    h.svc
        .import_relays(vec![incoming(
            ClientApp::Claude,
            "后搬来的",
            "https://second.test",
            "sk-s",
        )])
        .unwrap();
    let list = h.svc.list().unwrap();
    let first = list.iter().find(|p| p.name == "先有的").unwrap();
    let second = list.iter().find(|p| p.name == "后搬来的").unwrap();
    assert!(second.sort_order > first.sort_order);
}

// ---------- 本地路由（ADR-022） ----------

fn routing(enabled: bool, queue: &[&str]) -> mcpane_provider::RoutingConfig {
    mcpane_provider::RoutingConfig {
        enabled,
        failover_queue: queue.iter().map(|s| (*s).to_owned()).collect(),
        ..Default::default()
    }
}

#[test]
fn claude_routing_points_live_at_router_and_restores_direct() {
    let h = harness();
    let mut p = ProviderProfile::new_relay(ClientApp::Claude, "中转 A");
    p.base_url = Some("https://relay-a.example.com".into());
    p.model = Some("glm-5".into());
    p.api_key_field = ClaudeAuthField::ApiKey;
    let relay = h.svc.add_relay(p).unwrap();
    h.svc
        .set_api_key(&relay.id, &Secret::new("sk-relay-a"))
        .unwrap();
    h.svc
        .switch(ClientApp::Claude, &relay.id, &h.engine, &h.paths)
        .unwrap();

    h.svc
        .set_routing(ClientApp::Claude, routing(true, &[]), &h.engine, &h.paths)
        .unwrap();
    let gateway = h.svc.desktop_gateway().unwrap().unwrap();
    let live = read_json(&h.paths.claude_settings);
    assert_eq!(
        live["env"]["ANTHROPIC_BASE_URL"],
        format!("http://127.0.0.1:{}/claude", gateway.listen_port)
    );
    assert_eq!(live["env"]["ANTHROPIC_AUTH_TOKEN"], gateway.token);
    assert!(live["env"].get("ANTHROPIC_API_KEY").is_none());
    assert_eq!(
        live["env"]["ANTHROPIC_MODEL"], "glm-5",
        "模型仍取当前供应商"
    );

    // 退出还原：直连地址与真实 Key 回到 live，路由开关不变。
    h.svc
        .restore_direct(ClientApp::Claude, &h.engine, &h.paths)
        .unwrap();
    let live = read_json(&h.paths.claude_settings);
    assert_eq!(
        live["env"]["ANTHROPIC_BASE_URL"],
        "https://relay-a.example.com"
    );
    assert_eq!(live["env"]["ANTHROPIC_API_KEY"], "sk-relay-a");
    assert!(h.svc.routing(ClientApp::Claude).unwrap().enabled);
    assert_eq!(h.svc.routed_apps().unwrap(), vec![ClientApp::Claude]);

    // 关闭路由：直连。
    h.svc
        .reapply_routing(ClientApp::Claude, &h.engine, &h.paths)
        .unwrap();
    h.svc
        .set_routing(ClientApp::Claude, routing(false, &[]), &h.engine, &h.paths)
        .unwrap();
    let live = read_json(&h.paths.claude_settings);
    assert_eq!(
        live["env"]["ANTHROPIC_BASE_URL"],
        "https://relay-a.example.com"
    );
}

#[test]
fn routing_tuning_is_validated_and_persisted() {
    let h = harness();
    let mut config = routing(false, &[]);
    config.tuning.first_byte_timeout_secs = 5;
    let err = h
        .svc
        .set_routing(ClientApp::Claude, config.clone(), &h.engine, &h.paths)
        .unwrap_err();
    assert!(err.to_string().contains("首字节超时"), "{err}");
    config.tuning.first_byte_timeout_secs = 180;
    config.tuning.failure_threshold = 2;
    h.svc
        .set_routing(ClientApp::Claude, config, &h.engine, &h.paths)
        .unwrap();
    let saved = h.svc.routing(ClientApp::Claude).unwrap();
    assert_eq!(saved.tuning.first_byte_timeout_secs, 180);
    assert_eq!(saved.tuning.failure_threshold, 2);
    // 旧数据没有 tuning 字段：反序列化取默认值。
    let legacy: mcpane_provider::RoutingConfig =
        serde_json::from_str(r#"{"enabled":true,"failoverQueue":[]}"#).unwrap();
    assert_eq!(legacy.tuning, mcpane_provider::RoutingTuning::default());
}

#[test]
fn adopted_tuning_never_overrides_user_customization() {
    let h = harness();
    let tuned = mcpane_provider::RoutingTuning {
        first_byte_timeout_secs: 180,
        ..Default::default()
    };
    assert!(h.svc.adopt_routing_tuning(ClientApp::Codex, tuned).unwrap());
    assert_eq!(h.svc.routing(ClientApp::Codex).unwrap().tuning, tuned);
    let other = mcpane_provider::RoutingTuning {
        cooldown_secs: 300,
        ..Default::default()
    };
    assert!(
        !h.svc.adopt_routing_tuning(ClientApp::Codex, other).unwrap(),
        "已不是默认值（用户或上次迁移调过）就不覆盖"
    );
    assert!(
        !h.svc.routing(ClientApp::Codex).unwrap().enabled,
        "迁移不开启路由"
    );
}

#[test]
fn route_body_override_must_be_an_object() {
    let h = harness();
    let mut p = ProviderProfile::new_relay(ClientApp::Claude, "中转");
    p.base_url = Some("https://relay.example.com".into());
    p.route_body_override = serde_json::json!(["not", "object"]);
    assert!(h.svc.add_relay(p.clone()).is_err());
    p.route_body_override = serde_json::json!({ "enable_thinking": false });
    let saved = h.svc.add_relay(p).unwrap();
    assert_eq!(saved.route_body_override["enable_thinking"], false);
}

/// 关路由时状态已落盘、live 还没改回直连就被强杀：启动自愈按 live 内容还原直连。
#[test]
fn stale_takeover_left_by_a_crash_is_healed_to_direct() {
    let h = harness();
    let mut p = ProviderProfile::new_relay(ClientApp::Claude, "中转 A");
    p.base_url = Some("https://relay-a.example.com".into());
    p.api_key_field = ClaudeAuthField::ApiKey;
    let relay = h.svc.add_relay(p).unwrap();
    h.svc
        .set_api_key(&relay.id, &Secret::new("sk-relay-a"))
        .unwrap();
    h.svc
        .switch(ClientApp::Claude, &relay.id, &h.engine, &h.paths)
        .unwrap();
    assert!(
        !h.svc
            .heal_stale_takeover(ClientApp::Claude, &h.engine, &h.paths)
            .unwrap(),
        "直连状态无需自愈"
    );
    h.svc
        .set_routing(ClientApp::Claude, routing(true, &[]), &h.engine, &h.paths)
        .unwrap();
    let routed_live = std::fs::read_to_string(&h.paths.claude_settings).unwrap();
    assert!(
        !h.svc
            .heal_stale_takeover(ClientApp::Claude, &h.engine, &h.paths)
            .unwrap(),
        "路由开着时 live 指向路由是正常接管"
    );
    h.svc
        .set_routing(ClientApp::Claude, routing(false, &[]), &h.engine, &h.paths)
        .unwrap();
    std::fs::write(&h.paths.claude_settings, routed_live).unwrap();

    assert!(
        h.svc
            .heal_stale_takeover(ClientApp::Claude, &h.engine, &h.paths)
            .unwrap()
    );
    let live = read_json(&h.paths.claude_settings);
    assert_eq!(
        live["env"]["ANTHROPIC_BASE_URL"],
        "https://relay-a.example.com"
    );
    assert_eq!(live["env"]["ANTHROPIC_API_KEY"], "sk-relay-a");
    let profile = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .find(|p| p.id == relay.id)
        .unwrap();
    assert_eq!(
        profile.base_url.as_deref(),
        Some("https://relay-a.example.com"),
        "残留的路由地址不能倒灌进供应商记录"
    );
}

#[test]
fn routed_backfill_does_not_leak_router_into_profile() {
    let h = harness();
    let a = add_relay(
        &h.svc,
        ClientApp::Claude,
        "中转 A",
        "https://relay-a.example.com",
    );
    h.svc.set_api_key(&a.id, &Secret::new("sk-a")).unwrap();
    let b = add_relay(
        &h.svc,
        ClientApp::Claude,
        "中转 B",
        "https://relay-b.example.com",
    );
    h.svc.set_api_key(&b.id, &Secret::new("sk-b")).unwrap();
    h.svc
        .switch(ClientApp::Claude, &a.id, &h.engine, &h.paths)
        .unwrap();
    h.svc
        .set_routing(
            ClientApp::Claude,
            routing(true, &[&b.id]),
            &h.engine,
            &h.paths,
        )
        .unwrap();
    // 路由中切到 B：A 的回填不得吃进 loopback 地址与网关 token。
    h.svc
        .switch(ClientApp::Claude, &b.id, &h.engine, &h.paths)
        .unwrap();
    let a_after = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .find(|p| p.id == a.id)
        .unwrap();
    assert_eq!(
        a_after.base_url.as_deref(),
        Some("https://relay-a.example.com")
    );
    assert_eq!(h.svc.reveal_key(&a.id).unwrap().expose(), "sk-a");
}

#[test]
fn codex_routing_writes_router_base_url_and_gateway_token() {
    let h = harness();
    std::fs::write(&h.paths.codex_auth, OFFICIAL_AUTH).unwrap();
    let relay = add_relay(
        &h.svc,
        ClientApp::Codex,
        "Relay A",
        "https://relay-a.example.com/v1",
    );
    h.svc
        .set_api_key(&relay.id, &Secret::new("sk-codex-a"))
        .unwrap();
    h.svc
        .switch(ClientApp::Codex, &relay.id, &h.engine, &h.paths)
        .unwrap();
    h.svc
        .set_routing(ClientApp::Codex, routing(true, &[]), &h.engine, &h.paths)
        .unwrap();
    let gateway = h.svc.desktop_gateway().unwrap().unwrap();
    let config = std::fs::read_to_string(&h.paths.codex_config).unwrap();
    assert!(config.contains(&format!(
        "base_url = \"http://127.0.0.1:{}/codex\"",
        gateway.listen_port
    )));
    assert_eq!(
        read_json(&h.paths.codex_auth)["OPENAI_API_KEY"],
        gateway.token
    );
    // 路由中重写同一项：回填不得把网关 token 写进供应商 Key。
    h.svc
        .reapply_routing(ClientApp::Codex, &h.engine, &h.paths)
        .unwrap();
    assert_eq!(h.svc.reveal_key(&relay.id).unwrap().expose(), "sk-codex-a");

    // 切回官方：官方 auth 恢复，不经路由。
    h.svc
        .switch(ClientApp::Codex, OFFICIAL_CODEX_ID, &h.engine, &h.paths)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&h.paths.codex_auth).unwrap(),
        OFFICIAL_AUTH
    );
}

#[test]
fn routing_queue_validates_and_is_cleaned_on_delete() {
    let h = harness();
    let claude = add_relay(&h.svc, ClientApp::Claude, "A", "https://a.example.com");
    let codex = add_relay(&h.svc, ClientApp::Codex, "C", "https://c.example.com/v1");
    let err = h
        .svc
        .set_routing(
            ClientApp::Claude,
            routing(false, &[&codex.id]),
            &h.engine,
            &h.paths,
        )
        .unwrap_err();
    assert!(matches!(err, ProviderError::InvalidOperation(_)));
    assert!(
        h.svc
            .set_routing(ClientApp::OpenCode, routing(true, &[]), &h.engine, &h.paths)
            .is_err()
    );

    let saved = h
        .svc
        .set_routing(
            ClientApp::Claude,
            routing(false, &[&claude.id, &claude.id]),
            &h.engine,
            &h.paths,
        )
        .unwrap();
    assert_eq!(saved.failover_queue, vec![claude.id.clone()], "去重");
    h.svc.delete(&claude.id).unwrap();
    assert!(
        h.svc
            .routing(ClientApp::Claude)
            .unwrap()
            .failover_queue
            .is_empty()
    );
}

#[test]
fn codex_chat_provider_needs_router_and_writes_responses_wire() {
    let h = harness();
    let mut p = ProviderProfile::new_relay(ClientApp::Codex, "DeepSeek");
    p.base_url = Some("https://api.deepseek.com/v1".into());
    p.codex_wire_api = mcpane_provider::CodexWireApi::Chat;
    let relay = h.svc.add_relay(p).unwrap();
    h.svc.set_api_key(&relay.id, &Secret::new("sk-ds")).unwrap();

    // 未开路由：拒绝（提示开启本地路由）。
    let err = h
        .svc
        .switch(ClientApp::Codex, &relay.id, &h.engine, &h.paths)
        .unwrap_err();
    assert!(err.to_string().contains("本地路由"), "{err}");

    h.svc
        .set_routing(ClientApp::Codex, routing(true, &[]), &h.engine, &h.paths)
        .unwrap();
    h.svc
        .switch(ClientApp::Codex, &relay.id, &h.engine, &h.paths)
        .unwrap();
    let config = std::fs::read_to_string(&h.paths.codex_config).unwrap();
    assert!(config.contains("wire_api = \"responses\""), "{config}");
    assert!(!config.contains("wire_api = \"chat\""));

    // 路由中切走（触发回填）：不得把 live 的 responses 倒灌成供应商协议。
    let other = add_relay(
        &h.svc,
        ClientApp::Codex,
        "Other",
        "https://o.example.com/v1",
    );
    h.svc.set_api_key(&other.id, &Secret::new("sk-o")).unwrap();
    h.svc
        .switch(ClientApp::Codex, &other.id, &h.engine, &h.paths)
        .unwrap();
    let profile = h
        .svc
        .list()
        .unwrap()
        .into_iter()
        .find(|p| p.id == relay.id)
        .unwrap();
    assert_eq!(profile.codex_wire_api, mcpane_provider::CodexWireApi::Chat);

    // 当前供应商依赖路由时不允许关闭路由。
    h.svc
        .switch(ClientApp::Codex, &relay.id, &h.engine, &h.paths)
        .unwrap();
    assert!(
        h.svc
            .set_routing(ClientApp::Codex, routing(false, &[]), &h.engine, &h.paths)
            .is_err()
    );
    assert!(h.svc.routing(ClientApp::Codex).unwrap().enabled);
}

#[test]
fn claude_chat_provider_needs_router() {
    let h = harness();
    let mut p = ProviderProfile::new_relay(ClientApp::Claude, "Chat 站");
    p.base_url = Some("https://chat.example.com/v1".into());
    p.claude_api_format = mcpane_provider::ClaudeApiFormat::OpenaiChat;
    let relay = h.svc.add_relay(p).unwrap();
    h.svc.set_api_key(&relay.id, &Secret::new("sk-c")).unwrap();

    let err = h
        .svc
        .switch(ClientApp::Claude, &relay.id, &h.engine, &h.paths)
        .unwrap_err();
    assert!(err.to_string().contains("本地路由"), "{err}");

    h.svc
        .set_routing(ClientApp::Claude, routing(true, &[]), &h.engine, &h.paths)
        .unwrap();
    h.svc
        .switch(ClientApp::Claude, &relay.id, &h.engine, &h.paths)
        .unwrap();
    let live = read_json(&h.paths.claude_settings);
    assert!(
        live["env"]["ANTHROPIC_BASE_URL"]
            .as_str()
            .unwrap()
            .ends_with("/claude")
    );
    assert!(
        h.svc
            .set_routing(ClientApp::Claude, routing(false, &[]), &h.engine, &h.paths)
            .is_err(),
        "当前供应商依赖路由时不允许关闭"
    );
}

// 平台拦截先于协议检查，其他平台会提前返回，断言不到「模型映射」。
#[cfg(any(target_os = "macos", windows))]
#[test]
fn claude_desktop_chat_provider_requires_proxy_mode() {
    let h = harness();
    let mut p = ProviderProfile::new_relay(ClientApp::ClaudeDesktop, "Chat 站");
    p.base_url = Some("https://chat.example.com/v1".into());
    p.claude_api_format = mcpane_provider::ClaudeApiFormat::OpenaiChat;
    let relay = h.svc.add_relay(p).unwrap();
    h.svc.set_api_key(&relay.id, &Secret::new("sk-c")).unwrap();
    let err = h
        .svc
        .switch(ClientApp::ClaudeDesktop, &relay.id, &h.engine, &h.paths)
        .unwrap_err();
    assert!(err.to_string().contains("模型映射"), "{err}");
}

/// Gemini / Grok：路由中写本地地址与网关 token，切走时回填不污染供应商记录。
#[test]
fn gemini_and_grok_routing_write_router_and_backfill_cleanly() {
    let h = harness();
    std::fs::create_dir_all(h.paths.grok_config.parent().unwrap()).unwrap();
    for (app, base) in [
        (ClientApp::Gemini, "https://gemini-a.example.com"),
        (ClientApp::Grok, "https://grok-a.example.com/v1"),
    ] {
        let a = add_relay(&h.svc, app, "Relay A", base);
        h.svc.set_api_key(&a.id, &Secret::new("sk-real-a")).unwrap();
        let b = add_relay(&h.svc, app, "Relay B", "https://b.example.com/v1");
        h.svc.set_api_key(&b.id, &Secret::new("sk-real-b")).unwrap();
        h.svc.switch(app, &a.id, &h.engine, &h.paths).unwrap();
        h.svc
            .set_routing(app, routing(true, &[&b.id]), &h.engine, &h.paths)
            .unwrap();

        let gateway = h.svc.desktop_gateway().unwrap().unwrap();
        let ns = mcpane_provider::router_namespace(app).unwrap();
        let router_url = format!("http://127.0.0.1:{}/{ns}", gateway.listen_port);
        let live = match app {
            ClientApp::Gemini => std::fs::read_to_string(&h.paths.gemini_env).unwrap(),
            _ => std::fs::read_to_string(&h.paths.grok_config).unwrap(),
        };
        assert!(live.contains(&router_url), "{app:?}: {live}");
        assert!(live.contains(&gateway.token), "{app:?}: {live}");
        assert!(!live.contains("sk-real-a"), "{app:?}: 真实 Key 不得进 live");

        // 路由中切到 B：A 的回填不得吃进 loopback 地址与网关 token。
        h.svc.switch(app, &b.id, &h.engine, &h.paths).unwrap();
        let a_after = h
            .svc
            .list()
            .unwrap()
            .into_iter()
            .find(|p| p.id == a.id)
            .unwrap();
        assert_eq!(a_after.base_url.as_deref(), Some(base), "{app:?}");
        assert_eq!(h.svc.reveal_key(&a.id).unwrap().expose(), "sk-real-a");

        // 退出还原：直连 B 的真实地址与 Key。
        h.svc.restore_direct(app, &h.engine, &h.paths).unwrap();
        let live = match app {
            ClientApp::Gemini => std::fs::read_to_string(&h.paths.gemini_env).unwrap(),
            _ => std::fs::read_to_string(&h.paths.grok_config).unwrap(),
        };
        assert!(live.contains("https://b.example.com/v1"), "{app:?}: {live}");
        assert!(live.contains("sk-real-b"), "{app:?}");
    }
}

/// 统一供应商的影子被移除（删除统一供应商 / 关闭某个应用）时，故障转移队列同步清理。
#[test]
fn unified_shadow_removal_cleans_failover_queues() {
    let h = harness();
    let mut unified = mcpane_provider::UnifiedProvider::new("网关");
    unified.base_url = "https://gw.example.com".into();
    let unified = h.svc.upsert_unified(unified, &h.engine, &h.paths).unwrap();
    let claude_shadow = format!("unified-claude-{}", unified.id);
    let codex_shadow = format!("unified-codex-{}", unified.id);
    h.svc
        .set_routing(
            ClientApp::Claude,
            routing(false, &[&claude_shadow]),
            &h.engine,
            &h.paths,
        )
        .unwrap();
    h.svc
        .set_routing(
            ClientApp::Codex,
            routing(false, &[&codex_shadow]),
            &h.engine,
            &h.paths,
        )
        .unwrap();

    // 关闭 Codex：只清 Codex 队列里的影子。
    let mut edited = unified.clone();
    edited.apps.codex = false;
    h.svc.upsert_unified(edited, &h.engine, &h.paths).unwrap();
    assert!(
        h.svc
            .routing(ClientApp::Codex)
            .unwrap()
            .failover_queue
            .is_empty()
    );
    assert_eq!(
        h.svc.routing(ClientApp::Claude).unwrap().failover_queue,
        vec![claude_shadow]
    );

    h.svc.delete_unified(&unified.id).unwrap();
    assert!(
        h.svc
            .routing(ClientApp::Claude)
            .unwrap()
            .failover_queue
            .is_empty()
    );
}

#[test]
fn append_failover_candidates_keeps_order_and_skips_invalid() {
    let h = harness();
    let a = add_relay(&h.svc, ClientApp::Claude, "A", "https://a.example.com");
    let b = add_relay(&h.svc, ClientApp::Claude, "B", "https://b.example.com");
    let c = add_relay(&h.svc, ClientApp::Codex, "C", "https://c.example.com/v1");
    h.svc
        .set_routing(
            ClientApp::Claude,
            routing(false, &[&b.id]),
            &h.engine,
            &h.paths,
        )
        .unwrap();
    let added = h
        .svc
        .append_failover_candidates(
            ClientApp::Claude,
            &[a.id.clone(), b.id.clone(), c.id.clone(), "missing".into()],
        )
        .unwrap();
    assert_eq!(added, 1, "已在队列的 B、跨 app 的 C、不存在的 id 都不加");
    let routing_after = h.svc.routing(ClientApp::Claude).unwrap();
    assert_eq!(routing_after.failover_queue, vec![b.id, a.id]);
    assert!(!routing_after.enabled, "迁移不动路由开关");
}

// ---------- 官方订阅不查额度 ----------

#[tokio::test]
async fn quota_never_queries_official_subscriptions() {
    let h = harness();
    for id in [OFFICIAL_CLAUDE_ID, OFFICIAL_CODEX_ID] {
        let err = h.svc.quota(id).await.unwrap_err();
        assert!(
            matches!(err, ProviderError::InvalidOperation(_)),
            "官方 {id} 应在发请求前被拒：{err:?}"
        );
    }
}
