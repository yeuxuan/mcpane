//! ClientAdapter：客户端差异的统一分发点。
//!
//! 此前 Claude / Codex 的差异散落在 service.rs 的 match 与对
//! mcpane_adapter_claude:: / mcpane_adapter_codex:: 自由函数的硬调用里，
//! 新增客户端（下阶段的 Gemini CLI）要全量补 match。本模块把差异点收敛为
//! trait：加客户端 = 新增一个 impl + `adapter_for` 一个分支。
//!
//! 纯重构，行为与原 match 逐位对齐；由 tests/service.rs 守护。

use std::path::{Path, PathBuf};

use mcpane_adapter_codex::AuthKind;
use mcpane_config::{ConfigFormat, PatchPlan};

use crate::domain::{ClientApp, ProviderState};
use crate::service::ClientPaths;

/// 客户端用户级配置路径（discovery 的统一出口）。
#[derive(Debug, Clone)]
pub struct ClientUserPaths {
    /// 供应商切换写入的 live 配置（Claude settings.json / Codex config.toml /
    /// Gemini settings.json）。
    pub live_config: PathBuf,
    /// 独立凭据文件（Codex auth.json）；env 凭据模型的客户端无此文件。
    pub auth_file: Option<PathBuf>,
    /// 独立 dotenv 凭据文件（Gemini `~/.gemini/.env`）；其他客户端无此文件。
    pub env_file: Option<PathBuf>,
}

/// 独立凭据文件的处理钩子（Codex auth.json：备份分类 + 中转内容生成）。
/// env 凭据模型的客户端（Claude）无此概念，`ClientAdapter::auth_handling`
/// 返回 None。
pub trait AuthHandling {
    /// 凭据文件在 ClientPaths 中的槽位。
    fn auth_file<'a>(&self, paths: &'a ClientPaths) -> &'a PathBuf;
    /// 凭据文件内容分类（官方登录 / 中转 Key / 未识别 / 缺失）。
    fn classify(&self, content: Option<&str>) -> AuthKind;
    /// 中转站凭据文件内容（API Key 的落盘形态）。
    fn relay_content(&self, api_key: &str) -> String;
}

pub trait ClientAdapter: Send + Sync {
    fn app(&self) -> ClientApp;

    /// 客户端显示名（告警/提示文案）。
    fn display_name(&self) -> &'static str;

    /// 发现用户级配置路径。Codex 遵循 `CODEX_HOME` 语义。
    fn discover_user_paths(&self, home: &Path) -> ClientUserPaths;

    /// live 配置在 ClientPaths 中的槽位与格式。
    fn live_config<'a>(&self, paths: &'a ClientPaths) -> (&'a PathBuf, ConfigFormat);

    /// 附加/通用配置片段的受管键校验：叶子路径触碰受管键时返回
    /// 用于报错的键名（认证/模型/路由键必须走结构化字段）。
    fn managed_key_violation(&self, path: &[String]) -> Option<String>;

    /// ProviderState 中该客户端的通用配置片段槽。
    fn common_snippet<'a>(&self, state: &'a ProviderState) -> Option<&'a serde_json::Value>;
    fn common_snippet_slot<'a>(
        &self,
        state: &'a mut ProviderState,
    ) -> &'a mut Option<serde_json::Value>;

    /// 中转站 env 补丁（env 凭据模型客户端写 live 配置的 env.*）；
    /// 无 env 模型的客户端（Codex 走 model_providers 表）返回 None。
    fn provider_env_patch(&self, pairs: &[(&str, &str)]) -> Option<PatchPlan>;
    /// 移除全部受管 env 键的补丁；语义同上。
    fn remove_provider_env_patch(&self) -> Option<PatchPlan>;

    /// 独立凭据文件的处理钩子；None = 该客户端凭据随 env 补丁走，无独立文件。
    fn auth_handling(&self) -> Option<&dyn AuthHandling>;
}

pub struct ClaudeAdapter;
pub struct CodexAdapter;
pub struct GeminiAdapter;
pub struct GrokAdapter;
pub struct OpenCodeAdapter;
pub struct OpenClawAdapter;
pub struct HermesAdapter;
pub struct ClaudeDesktopAdapter;

/// 按 app 取 adapter（新增客户端在此补一个分支）。
pub fn adapter_for(app: ClientApp) -> &'static dyn ClientAdapter {
    match app {
        ClientApp::Claude => &ClaudeAdapter,
        ClientApp::Codex => &CodexAdapter,
        ClientApp::Gemini => &GeminiAdapter,
        ClientApp::Grok => &GrokAdapter,
        ClientApp::OpenCode => &OpenCodeAdapter,
        ClientApp::OpenClaw => &OpenClawAdapter,
        ClientApp::Hermes => &HermesAdapter,
        ClientApp::ClaudeDesktop => &ClaudeDesktopAdapter,
    }
}

impl ClientAdapter for ClaudeAdapter {
    fn app(&self) -> ClientApp {
        ClientApp::Claude
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn discover_user_paths(&self, home: &Path) -> ClientUserPaths {
        let paths = mcpane_adapter_claude::discover_config_paths(home, None);
        ClientUserPaths {
            live_config: paths.user_settings,
            auth_file: None,
            env_file: None,
        }
    }

    fn live_config<'a>(&self, paths: &'a ClientPaths) -> (&'a PathBuf, ConfigFormat) {
        (&paths.claude_settings, ConfigFormat::Json)
    }

    fn managed_key_violation(&self, path: &[String]) -> Option<String> {
        (path.len() >= 2
            && path[0] == "env"
            && mcpane_adapter_claude::PROVIDER_ENV_KEYS.contains(&path[1].as_str()))
        .then(|| format!("env.{}", path[1]))
    }

    fn common_snippet<'a>(&self, state: &'a ProviderState) -> Option<&'a serde_json::Value> {
        state.claude_common_snippet.as_ref()
    }

    fn common_snippet_slot<'a>(
        &self,
        state: &'a mut ProviderState,
    ) -> &'a mut Option<serde_json::Value> {
        &mut state.claude_common_snippet
    }

    fn provider_env_patch(&self, pairs: &[(&str, &str)]) -> Option<PatchPlan> {
        Some(mcpane_adapter_claude::provider_env_patch(pairs))
    }

    fn remove_provider_env_patch(&self) -> Option<PatchPlan> {
        // 切回官方：清受管键 + 历史遗留键（SMALL_FAST_MODEL 等）。
        let keys: Vec<&str> = mcpane_adapter_claude::PROVIDER_ENV_KEYS
            .iter()
            .chain(mcpane_adapter_claude::LEGACY_ENV_KEYS.iter())
            .copied()
            .collect();
        Some(mcpane_adapter_claude::remove_provider_env_patch(&keys))
    }

    fn auth_handling(&self) -> Option<&dyn AuthHandling> {
        None
    }
}

/// Codex config.toml 中由结构化字段管理的顶层键：附加片段不得覆盖，
/// 否则切换清理与 slug 表管理会失效。
/// `experimental_bearer_token` 是 Codex 的明文凭据字段（顶层或 provider 表内），
/// 也一并拒绝：认证必须走 auth.json→Keychain 的结构化路径，否则明文 Key 会进
/// config.toml 且落入**不脱敏**的引擎快照（Codex 臂快照无 redactor）。
/// `model_context_window` / `model_auto_compact_token_limit` 由 1M 窗口开关管理。
const CODEX_MANAGED_TOP_KEYS: [&str; 7] = [
    "model_provider",
    "model_providers",
    "disable_response_storage",
    "model",
    "experimental_bearer_token",
    "model_context_window",
    "model_auto_compact_token_limit",
];

impl ClientAdapter for CodexAdapter {
    fn app(&self) -> ClientApp {
        ClientApp::Codex
    }

    fn display_name(&self) -> &'static str {
        "Codex"
    }

    fn discover_user_paths(&self, home: &Path) -> ClientUserPaths {
        let codex_home_override = std::env::var("CODEX_HOME").ok().map(PathBuf::from);
        let paths =
            mcpane_adapter_codex::discover_config_paths(home, codex_home_override.as_deref(), None);
        ClientUserPaths {
            live_config: paths.user_config,
            auth_file: Some(paths.auth_json),
            env_file: None,
        }
    }

    fn live_config<'a>(&self, paths: &'a ClientPaths) -> (&'a PathBuf, ConfigFormat) {
        (&paths.codex_config, ConfigFormat::Toml)
    }

    fn managed_key_violation(&self, path: &[String]) -> Option<String> {
        CODEX_MANAGED_TOP_KEYS
            .contains(&path[0].as_str())
            .then(|| path[0].clone())
    }

    fn common_snippet<'a>(&self, state: &'a ProviderState) -> Option<&'a serde_json::Value> {
        state.codex_common_snippet.as_ref()
    }

    fn common_snippet_slot<'a>(
        &self,
        state: &'a mut ProviderState,
    ) -> &'a mut Option<serde_json::Value> {
        &mut state.codex_common_snippet
    }

    fn provider_env_patch(&self, _pairs: &[(&str, &str)]) -> Option<PatchPlan> {
        None
    }

    fn remove_provider_env_patch(&self) -> Option<PatchPlan> {
        None
    }

    fn auth_handling(&self) -> Option<&dyn AuthHandling> {
        Some(self)
    }
}

impl ClientAdapter for GeminiAdapter {
    fn app(&self) -> ClientApp {
        ClientApp::Gemini
    }

    fn display_name(&self) -> &'static str {
        "Gemini CLI"
    }

    fn discover_user_paths(&self, home: &Path) -> ClientUserPaths {
        let paths = mcpane_adapter_gemini::discover_config_paths(home);
        ClientUserPaths {
            live_config: paths.settings,
            auth_file: None,
            env_file: Some(paths.env_file),
        }
    }

    fn live_config<'a>(&self, paths: &'a ClientPaths) -> (&'a PathBuf, ConfigFormat) {
        (&paths.gemini_settings, ConfigFormat::Json)
    }

    fn managed_key_violation(&self, path: &[String]) -> Option<String> {
        // settings.json 片段不得触碰 auth 类型（由切换流程独占管理）；
        // .env 受管键不经片段表达（片段只落 settings.json），无需在此拦截。
        (path.len() >= mcpane_adapter_gemini::AUTH_TYPE_PATH.len()
            && path
                .iter()
                .zip(mcpane_adapter_gemini::AUTH_TYPE_PATH.iter())
                .all(|(a, b)| a == b))
        .then(|| mcpane_adapter_gemini::AUTH_TYPE_PATH.join("."))
    }

    fn common_snippet<'a>(&self, state: &'a ProviderState) -> Option<&'a serde_json::Value> {
        state.gemini_common_snippet.as_ref()
    }

    fn common_snippet_slot<'a>(
        &self,
        state: &'a mut ProviderState,
    ) -> &'a mut Option<serde_json::Value> {
        &mut state.gemini_common_snippet
    }

    fn provider_env_patch(&self, _pairs: &[(&str, &str)]) -> Option<PatchPlan> {
        // Gemini 的 env 凭据在独立的 ~/.gemini/.env（非 live_config 内的 env 块），
        // 由 switch 臂经 Env 文档模型专门写入。
        None
    }

    fn remove_provider_env_patch(&self) -> Option<PatchPlan> {
        None
    }

    fn auth_handling(&self) -> Option<&dyn AuthHandling> {
        // 官方 OAuth 凭据（oauth_creds.json）由 Gemini CLI 自己管理：
        // 切中转站不备份，切回官方只改 settings.json 的 selectedType。
        None
    }
}

impl ClientAdapter for GrokAdapter {
    fn app(&self) -> ClientApp {
        ClientApp::Grok
    }

    fn display_name(&self) -> &'static str {
        "Grok CLI"
    }

    fn discover_user_paths(&self, home: &Path) -> ClientUserPaths {
        let paths = mcpane_adapter_grok::discover_config_paths(home);
        ClientUserPaths {
            live_config: paths.config,
            // 凭据就写在 config.toml 的模型表里，无独立 auth 文件；
            // 官方 OAuth 凭据由 Grok CLI 自管，我们不碰。
            auth_file: None,
            env_file: None,
        }
    }

    fn live_config<'a>(&self, paths: &'a ClientPaths) -> (&'a PathBuf, ConfigFormat) {
        (&paths.grok_config, ConfigFormat::Toml)
    }

    fn managed_key_violation(&self, path: &[String]) -> Option<String> {
        // 模型表与默认指针由切换流程独占：片段里改它们会和切换互相覆盖，
        // 必须走结构化字段（base_url / model / Key）。
        if path
            .first()
            .is_some_and(|k| k == mcpane_adapter_grok::MODEL_TABLE_KEY)
        {
            return Some(mcpane_adapter_grok::MODEL_TABLE_KEY.to_owned());
        }
        (path.len() >= mcpane_adapter_grok::MODELS_DEFAULT_PATH.len()
            && path
                .iter()
                .zip(mcpane_adapter_grok::MODELS_DEFAULT_PATH.iter())
                .all(|(a, b)| a == b))
        .then(|| mcpane_adapter_grok::MODELS_DEFAULT_PATH.join("."))
    }

    fn common_snippet<'a>(&self, state: &'a ProviderState) -> Option<&'a serde_json::Value> {
        state.grok_common_snippet.as_ref()
    }

    fn common_snippet_slot<'a>(
        &self,
        state: &'a mut ProviderState,
    ) -> &'a mut Option<serde_json::Value> {
        &mut state.grok_common_snippet
    }

    fn provider_env_patch(&self, _pairs: &[(&str, &str)]) -> Option<PatchPlan> {
        // Grok 没有 env 凭据模型：Key 写进模型表，由 switch 臂处理。
        None
    }

    fn remove_provider_env_patch(&self) -> Option<PatchPlan> {
        None
    }

    fn auth_handling(&self) -> Option<&dyn AuthHandling> {
        // 官方 OAuth 凭据由 Grok CLI 自己管理，切中转站不需要备份。
        None
    }
}

impl ClientAdapter for OpenCodeAdapter {
    fn app(&self) -> ClientApp {
        ClientApp::OpenCode
    }

    fn display_name(&self) -> &'static str {
        "OpenCode"
    }

    fn discover_user_paths(&self, home: &Path) -> ClientUserPaths {
        let paths = mcpane_adapter_opencode::discover_config_paths(home);
        ClientUserPaths {
            // Key 写在 provider 条目的 options.apiKey 里，无独立凭据/env 文件。
            live_config: paths.config,
            auth_file: None,
            env_file: None,
        }
    }

    fn live_config<'a>(&self, paths: &'a ClientPaths) -> (&'a PathBuf, ConfigFormat) {
        // JSONC，不是严格 JSON：OpenCode 官方文档明写「supports both JSON and
        // JSONC (JSON with Comments)」，示例本身就带 `//` 注释与尾逗号。用
        // serde_json 严格解析会在用户照官方写法配置时直接 Parse 失败，整个
        // OpenCode 供应商页打不开。走 Json5（JSONC 的超集）既能读，round-trip
        // 还能把注释原样留住——cc-switch 那边是 json5 读、标准 JSON 写，注释会丢。
        (&paths.opencode_config, ConfigFormat::Json5)
    }

    fn managed_key_violation(&self, path: &[String]) -> Option<String> {
        // provider 表由启用/移除流程独占；片段要改的是 mcp、theme 等其他键。
        (path
            .first()
            .is_some_and(|k| k == mcpane_adapter_opencode::PROVIDER_TABLE_KEY))
        .then(|| mcpane_adapter_opencode::PROVIDER_TABLE_KEY.to_owned())
    }

    fn common_snippet<'a>(&self, state: &'a ProviderState) -> Option<&'a serde_json::Value> {
        state.opencode_common_snippet.as_ref()
    }

    fn common_snippet_slot<'a>(
        &self,
        state: &'a mut ProviderState,
    ) -> &'a mut Option<serde_json::Value> {
        &mut state.opencode_common_snippet
    }

    fn provider_env_patch(&self, _pairs: &[(&str, &str)]) -> Option<PatchPlan> {
        None
    }

    fn remove_provider_env_patch(&self) -> Option<PatchPlan> {
        None
    }

    fn auth_handling(&self) -> Option<&dyn AuthHandling> {
        None
    }
}

impl ClientAdapter for OpenClawAdapter {
    fn app(&self) -> ClientApp {
        ClientApp::OpenClaw
    }

    fn display_name(&self) -> &'static str {
        "OpenClaw"
    }

    fn discover_user_paths(&self, home: &Path) -> ClientUserPaths {
        let paths = mcpane_adapter_openclaw::discover_config_paths(home);
        ClientUserPaths {
            live_config: paths.config,
            auth_file: None,
            env_file: None,
        }
    }

    fn live_config<'a>(&self, paths: &'a ClientPaths) -> (&'a PathBuf, ConfigFormat) {
        // 文件名是 .json，内容却是 JSON5（无引号键/单引号/注释），必须用 Json5
        // 解析——用严格 JSON 读它连它自带的初始配置都解析不了。
        (&paths.openclaw_config, ConfigFormat::Json5)
    }

    fn managed_key_violation(&self, path: &[String]) -> Option<String> {
        (path.len() >= mcpane_adapter_openclaw::PROVIDERS_PATH.len()
            && path
                .iter()
                .zip(mcpane_adapter_openclaw::PROVIDERS_PATH.iter())
                .all(|(a, b)| a == b))
        .then(|| mcpane_adapter_openclaw::PROVIDERS_PATH.join("."))
    }

    fn common_snippet<'a>(&self, state: &'a ProviderState) -> Option<&'a serde_json::Value> {
        state.openclaw_common_snippet.as_ref()
    }

    fn common_snippet_slot<'a>(
        &self,
        state: &'a mut ProviderState,
    ) -> &'a mut Option<serde_json::Value> {
        &mut state.openclaw_common_snippet
    }

    fn provider_env_patch(&self, _pairs: &[(&str, &str)]) -> Option<PatchPlan> {
        None
    }

    fn remove_provider_env_patch(&self) -> Option<PatchPlan> {
        None
    }

    fn auth_handling(&self) -> Option<&dyn AuthHandling> {
        None
    }
}

impl ClientAdapter for HermesAdapter {
    fn app(&self) -> ClientApp {
        ClientApp::Hermes
    }

    fn display_name(&self) -> &'static str {
        "Hermes"
    }

    fn discover_user_paths(&self, home: &Path) -> ClientUserPaths {
        let paths = mcpane_adapter_hermes::discover_config_paths(home);
        ClientUserPaths {
            live_config: paths.config,
            auth_file: None,
            env_file: None,
        }
    }

    fn live_config<'a>(&self, paths: &'a ClientPaths) -> (&'a PathBuf, ConfigFormat) {
        (&paths.hermes_config, ConfigFormat::Yaml)
    }

    fn managed_key_violation(&self, path: &[String]) -> Option<String> {
        (path
            .first()
            .is_some_and(|k| k == mcpane_adapter_hermes::CUSTOM_PROVIDERS_KEY))
        .then(|| mcpane_adapter_hermes::CUSTOM_PROVIDERS_KEY.to_owned())
    }

    fn common_snippet<'a>(&self, state: &'a ProviderState) -> Option<&'a serde_json::Value> {
        state.hermes_common_snippet.as_ref()
    }

    fn common_snippet_slot<'a>(
        &self,
        state: &'a mut ProviderState,
    ) -> &'a mut Option<serde_json::Value> {
        &mut state.hermes_common_snippet
    }

    fn provider_env_patch(&self, _pairs: &[(&str, &str)]) -> Option<PatchPlan> {
        None
    }

    fn remove_provider_env_patch(&self) -> Option<PatchPlan> {
        None
    }

    fn auth_handling(&self) -> Option<&dyn AuthHandling> {
        None
    }
}

impl ClientAdapter for ClaudeDesktopAdapter {
    fn app(&self) -> ClientApp {
        ClientApp::ClaudeDesktop
    }

    fn display_name(&self) -> &'static str {
        "Claude Desktop"
    }

    fn discover_user_paths(&self, home: &Path) -> ClientUserPaths {
        // live 视图对准我们的 profile（threep configLibrary 下）；
        // Desktop 无独立凭据/env 文件——网关 key 就在 profile 内（受管键隐藏）。
        let paths = crate::claude_desktop::discover_paths(home);
        ClientUserPaths {
            live_config: paths.profile,
            auth_file: None,
            env_file: None,
        }
    }

    fn live_config<'a>(&self, paths: &'a ClientPaths) -> (&'a PathBuf, ConfigFormat) {
        (&paths.claude_desktop.profile, ConfigFormat::Json)
    }

    fn managed_key_violation(&self, path: &[String]) -> Option<String> {
        // profile 由 MCPane 全量生成，网关键一律拒绝片段触碰。
        crate::claude_desktop::PROFILE_MANAGED_KEYS
            .contains(&path[0].as_str())
            .then(|| path[0].clone())
    }

    fn common_snippet<'a>(&self, state: &'a ProviderState) -> Option<&'a serde_json::Value> {
        state.desktop_common_snippet.as_ref()
    }

    fn common_snippet_slot<'a>(
        &self,
        state: &'a mut ProviderState,
    ) -> &'a mut Option<serde_json::Value> {
        &mut state.desktop_common_snippet
    }

    fn provider_env_patch(&self, _pairs: &[(&str, &str)]) -> Option<PatchPlan> {
        // 非 env 凭据模型：profile 整文件生成，不走补丁。
        None
    }

    fn remove_provider_env_patch(&self) -> Option<PatchPlan> {
        None
    }

    fn auth_handling(&self) -> Option<&dyn AuthHandling> {
        None
    }
}

impl AuthHandling for CodexAdapter {
    fn auth_file<'a>(&self, paths: &'a ClientPaths) -> &'a PathBuf {
        &paths.codex_auth
    }

    fn classify(&self, content: Option<&str>) -> AuthKind {
        mcpane_adapter_codex::classify_auth(content)
    }

    fn relay_content(&self, api_key: &str) -> String {
        mcpane_adapter_codex::relay_auth_content(api_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_key_violation_matches_previous_match_arms() {
        let claude = adapter_for(ClientApp::Claude);
        assert_eq!(
            claude.managed_key_violation(&["env".into(), "ANTHROPIC_BASE_URL".into()]),
            Some("env.ANTHROPIC_BASE_URL".into())
        );
        assert_eq!(
            claude.managed_key_violation(&["env".into(), "API_TIMEOUT_MS".into()]),
            None
        );
        assert_eq!(claude.managed_key_violation(&["model".into()]), None);

        let codex = adapter_for(ClientApp::Codex);
        assert_eq!(
            codex.managed_key_violation(&["model_provider".into()]),
            Some("model_provider".into())
        );
        assert_eq!(
            codex.managed_key_violation(&["model_reasoning_effort".into()]),
            None
        );
        assert_eq!(
            codex.managed_key_violation(&["model_context_window".into()]),
            Some("model_context_window".into())
        );
        assert_eq!(
            codex.managed_key_violation(&["model_auto_compact_token_limit".into()]),
            Some("model_auto_compact_token_limit".into())
        );
    }

    #[test]
    fn auth_handling_only_for_codex() {
        assert!(adapter_for(ClientApp::Claude).auth_handling().is_none());
        assert!(adapter_for(ClientApp::Codex).auth_handling().is_some());
    }

    #[test]
    fn live_config_formats() {
        let paths = ClientPaths {
            claude_settings: PathBuf::from("/h/.claude/settings.json"),
            codex_config: PathBuf::from("/h/.codex/config.toml"),
            codex_auth: PathBuf::from("/h/.codex/auth.json"),
            gemini_env: PathBuf::from("/h/.gemini/.env"),
            gemini_settings: PathBuf::from("/h/.gemini/settings.json"),
            grok_config: PathBuf::from("/h/.grok/config.toml"),
            opencode_config: PathBuf::from("/h/.config/opencode/opencode.json"),
            openclaw_config: PathBuf::from("/h/.openclaw/openclaw.json"),
            hermes_config: PathBuf::from("/h/.hermes/config.yaml"),
            claude_desktop: crate::claude_desktop::paths_from_dirs(
                Path::new("/h/Library/Application Support/Claude"),
                Path::new("/h/Library/Application Support/Claude-3p"),
            ),
        };
        let (claude_path, claude_format) = adapter_for(ClientApp::Claude).live_config(&paths);
        assert_eq!(claude_path, &paths.claude_settings);
        assert_eq!(claude_format, ConfigFormat::Json);
        let (codex_path, codex_format) = adapter_for(ClientApp::Codex).live_config(&paths);
        assert_eq!(codex_path, &paths.codex_config);
        assert_eq!(codex_format, ConfigFormat::Toml);
        let (desktop_path, desktop_format) =
            adapter_for(ClientApp::ClaudeDesktop).live_config(&paths);
        assert_eq!(desktop_path, &paths.claude_desktop.profile);
        assert_eq!(desktop_format, ConfigFormat::Json);
    }
}
