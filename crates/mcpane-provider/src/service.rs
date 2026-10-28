//! Provider Service（v2：官方订阅 + 中转站二分模型的切换引擎）。
//!
//! - `settings.json` / `config.toml` 写入统一走 mcpane-config 安全引擎
//!   （Diff、写前 Snapshot、失败恢复）；
//! - Codex `auth.json` 含 OAuth Token，**不走引擎**（引擎快照会把 Token
//!   明文落盘）：独立原子写，恢复源唯一指向系统凭据库中的备份。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mcpane_adapter_codex::AuthKind;
use mcpane_config::history::{HistoryStore, JsonlHistoryStore};
use mcpane_config::snapshot::{FsSnapshotStore, SnapshotStore};
use mcpane_config::{
    ConfigDocument, ConfigEngine, ConfigError, ConfigFormat, PatchOp, PatchPlan, RealFs,
};
use mcpane_secrets::{Secret, SecretError, SecretReference, SecretStore};

use crate::adapter::{AuthHandling, ClaudeAdapter, ClientAdapter, CodexAdapter, adapter_for};
use crate::claude_desktop;
use crate::domain::{
    CODEX_AUTO_COMPACT_LIMIT_1M, CODEX_CONTEXT_WINDOW_1M, CODEX_SLUG_PREFIX, ClaudeApiFormat,
    ClaudeAuthField, ClientApp, CodexWireApi, DesktopGateway, DesktopMode, DesktopRoute,
    OFFICIAL_CLAUDE_DESKTOP_ID, OFFICIAL_CLAUDE_ID, OFFICIAL_CODEX_ID, OFFICIAL_GEMINI_ID,
    OFFICIAL_GROK_ID, ProviderCategory, ProviderProfile, ProviderState, RoutingConfig,
    UnifiedProvider, codex_base_url_for_gateway, generate_codex_slug, unified_shadow_id,
};
use crate::store::{ProviderStore, StoreError};

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Secret(#[from] SecretError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("IO 错误：{0}")]
    Io(#[from] std::io::Error),
    #[error("连通性测试失败：{0}")]
    Connectivity(String),
    #[error("Provider 不存在：{0}")]
    NotFound(String),
    #[error("操作不允许：{0}")]
    InvalidOperation(String),
}

/// 批量收编的结果（名称维度，供 UI 直接展示）。
/// `failed` 与 `skipped` 分开：前者是"这条有问题"，后者是"你已经有了"。
#[derive(Debug, Default, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayImportOutcome {
    pub imported: Vec<String>,
    pub skipped: Vec<String>,
    pub failed: Vec<String>,
}

/// 切换涉及的客户端配置路径（由调用方经 adapter discovery 提供）。
#[derive(Debug, Clone)]
pub struct ClientPaths {
    pub claude_settings: PathBuf,
    pub codex_config: PathBuf,
    pub codex_auth: PathBuf,
    /// Gemini `~/.gemini/.env`（Provider 受管键）。
    pub gemini_env: PathBuf,
    /// Gemini `~/.gemini/settings.json`（auth 类型 + 附加片段目标）。
    pub gemini_settings: PathBuf,
    /// Grok `~/.grok/config.toml`（模型表 + 默认模型指针）。
    pub grok_config: PathBuf,
    /// OpenCode `~/.config/opencode/opencode.json`（additive 的 provider 表）。
    pub opencode_config: PathBuf,
    /// OpenClaw `~/.openclaw/openclaw.json`（additive；内容是 JSON5）。
    pub openclaw_config: PathBuf,
    /// Hermes `~/.hermes/config.yaml`（additive；供应商容器是数组）。
    pub hermes_config: PathBuf,
    /// Claude Desktop 落盘路径组（4 文件协同，见 claude_desktop 模块）。
    pub claude_desktop: crate::claude_desktop::ClaudeDesktopPaths,
}

/// 切换结果：`written=false` 表示 no-op；warnings 为非阻断性提示。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchOutcome {
    pub written: bool,
    pub warnings: Vec<String>,
}

type Engine = ConfigEngine<FsSnapshotStore, JsonlHistoryStore, RealFs>;

/// 切走前的上一个当前项快照（用于清理它写入的键）。
struct PreviousRelay {
    /// Codex：上一个中转站写入的顶层 model。
    model: Option<String>,
    /// 上一项写入的附加配置片段（含叠加的通用配置；Claude 落 settings.json，
    /// Codex 落 config.toml），切走时对"值仍与写入时一致"的叶子做深移除。
    fragment: serde_json::Value,
}

/// 路由接管时写入 live 的端点（本地路由地址 + 网关 token）。
struct RouterEndpoint {
    base_url: String,
    token: String,
}

/// 只提供 Chat Completions 的中转站：离开本地路由的协议转换就无法使用。
fn needs_router(profile: &ProviderProfile) -> bool {
    match profile.app {
        ClientApp::Codex => profile.codex_wire_api != CodexWireApi::Responses,
        ClientApp::Claude => profile.claude_api_format != ClaudeApiFormat::Anthropic,
        _ => false,
    }
}

/// 该 app 开启了路由则返回写入端点（首次使用时生成网关，随 state 一并落盘）。
fn router_endpoint(state: &mut ProviderState, app: ClientApp) -> Option<RouterEndpoint> {
    if !state.routing.get(&app).is_some_and(|r| r.enabled) {
        return None;
    }
    let gateway = state
        .desktop_gateway
        .get_or_insert_with(DesktopGateway::generate);
    Some(RouterEndpoint {
        base_url: crate::domain::router_base_url(app, gateway.listen_port)?,
        token: gateway.token.clone(),
    })
}

pub struct ProviderService<St: ProviderStore, Sec: SecretStore> {
    store: St,
    secrets: Sec,
}

impl<St: ProviderStore, Sec: SecretStore> ProviderService<St, Sec> {
    pub fn new(store: St, secrets: Sec) -> Self {
        Self { store, secrets }
    }

    // ---------- 查询 ----------

    /// 全量列表：每个 app 的官方项在前（sort_order = -1），其后是中转站。
    ///
    /// 官方项是 store 里的真实记录（见 [`Self::seed_officials`]），不再运行时合成——
    /// 没装的客户端不会凭空多出一条"官方订阅"，用户删掉的官方项也不会复活。
    pub fn list(&self) -> Result<Vec<ProviderProfile>, ProviderError> {
        let state = self.store.load()?;
        let mut result = Vec::with_capacity(state.providers.len());
        for app in ClientApp::ALL {
            let mut owned: Vec<ProviderProfile> = state
                .providers
                .iter()
                .filter(|p| p.app == app)
                .cloned()
                .collect();
            owned.sort_by_key(|p| p.sort_order);
            result.extend(owned);
        }
        Ok(result)
    }

    /// 首启播种官方项：只为 `installed` 里**确实装了的客户端**建一条官方 Profile，
    /// 且每个 app 只播种一次（`seeded_officials` 记账）。
    ///
    /// 对齐 cc-switch 的 `init_default_official_providers`：官方项是落库的普通记录，
    /// 用户删掉即不再复活（flag 已记）。与 cc-switch 的全局 flag 不同，这里按 app
    /// 记账——"先装 MCPane、后装 Gemini CLI"的用户下次启动会自动补种，不用手动加。
    ///
    /// 「当前生效项就是官方」的 app 一律补种，哪怕安装探测说没装：current 指向它
    /// 就是"用户确实在用"的硬证据（也是老版本升级上来的状态——官方项那时不落库，
    /// current 却已经记着 official-* 这个 id），不补种会留下一个指向空气的 current。
    ///
    /// 返回本次新播种的 app。
    pub fn seed_officials(&self, installed: &[ClientApp]) -> Result<Vec<ClientApp>, ProviderError> {
        let mut state = self.store.load()?;
        let mut targets: Vec<ClientApp> = installed.to_vec();
        for app in ClientApp::ALL {
            let in_use = state
                .current
                .get(&app)
                .is_some_and(|id| id == official_id_for(app));
            if in_use && app.has_official() && !targets.contains(&app) {
                targets.push(app);
            }
        }
        let mut seeded = Vec::new();
        let mut dirty = false;
        for app in &targets {
            // additive 客户端没有官方形态（OpenCode 自己不提供模型），
            // 给它播一条"官方订阅"只会是一张切不动的空卡片。
            if !app.has_official() {
                continue;
            }
            if !state.seeded_officials.insert(*app) {
                continue; // 已播种过（哪怕用户随后删了）
            }
            dirty = true; // 记账本身要落盘，否则下次启动会重播已删的官方项
            let id = official_id_for(*app);
            if state.providers.iter().any(|p| p.id == id) {
                continue; // 极端情况：同 id 已存在，尊重存量
            }
            let profile = official_seed(&mut state, *app);
            state.providers.push(profile);
            seeded.push(*app);
        }
        if dirty {
            self.store.save(&state)?;
        }
        Ok(seeded)
    }

    /// 确保某 app 的官方项存在（用户删掉后从"添加供应商"重新加回来）。
    /// 已存在则原样返回；不存在则按模板插入并记账。
    pub fn ensure_official(&self, app: ClientApp) -> Result<ProviderProfile, ProviderError> {
        if !app.has_official() {
            return Err(ProviderError::InvalidOperation(
                "该客户端没有官方订阅形态".into(),
            ));
        }
        let mut state = self.store.load()?;
        let id = official_id_for(app);
        if let Some(existing) = state.providers.iter().find(|p| p.id == id) {
            return Ok(existing.clone());
        }
        let profile = official_seed(&mut state, app);
        state.providers.push(profile.clone());
        state.seeded_officials.insert(app);
        self.store.save(&state)?;
        Ok(profile)
    }

    /// 各客户端配置目录的用户覆盖（app → 绝对路径；无覆盖的 app 不出现）。
    pub fn config_dir_overrides(&self) -> Result<BTreeMap<ClientApp, String>, ProviderError> {
        Ok(self.store.load()?.client_config_dirs)
    }

    /// 设置/清除某个客户端的配置目录覆盖。
    ///
    /// 空串 = 清除（回到默认位置）。非空必须是**存在的绝对目录**——写错了不该
    /// 静默生效，那会让用户以为"设了但没用"，实际是配置全写到了别处。
    /// Claude Desktop 的四个文件跨两个目录，不支持覆盖，在这里就挡住。
    pub fn set_config_dir_override(&self, app: ClientApp, dir: &str) -> Result<(), ProviderError> {
        if app == ClientApp::ClaudeDesktop {
            return Err(ProviderError::InvalidOperation(
                "Claude Desktop 的配置分布在多个目录，不支持整体覆盖".into(),
            ));
        }
        let trimmed = dir.trim();
        let mut state = self.store.load()?;
        if trimmed.is_empty() {
            state.client_config_dirs.remove(&app);
        } else {
            let path = std::path::Path::new(trimmed);
            if !path.is_absolute() {
                return Err(ProviderError::InvalidOperation(
                    "配置目录必须是绝对路径".into(),
                ));
            }
            if !path.is_dir() {
                return Err(ProviderError::InvalidOperation(format!(
                    "目录不存在：{trimmed}"
                )));
            }
            state.client_config_dirs.insert(app, trimmed.to_owned());
        }
        self.store.save(&state)?;
        Ok(())
    }

    /// 每个 app 当前生效的 Provider id。
    pub fn current(&self) -> Result<BTreeMap<ClientApp, String>, ProviderError> {
        Ok(self.store.load()?.current)
    }

    /// 首启存量收编：
    /// 若某 app 尚无任何 provider，把 live 里既有的第三方配置收编为可管理的
    /// Profile 并**设为当前生效项**。这样 `current` 如实反映"live 其实是中转"，
    /// 而非首启误显示官方生效，用户也能立刻在列表里看到并管理既有配置。
    /// 返回收编到的 Profile 名称；该 app 已有 provider、或 live 无可收编内容时
    /// 返回 None。
    ///
    /// 与首切收编安全阀（[`Self::switch`] 内）互补：本方法在启动时主动收编并置
    /// current，使首切收编的 `!current.contains_key` 守卫自然短路，二者不重复。
    pub fn import_live_on_startup(
        &self,
        app: ClientApp,
        engine: &Engine,
        paths: &ClientPaths,
    ) -> Result<Option<String>, ProviderError> {
        let mut state = self.store.load()?;
        // 守卫：该 app 已有**非官方** provider（用户已录入 / 先前已收编）→ 不重复导入。
        // 官方项是我们自己播种的，不能算"用户已有配置"，否则播种一次之后收编就
        // 永远短路（对齐 cc-switch `has_non_official_seed_provider` 的口径）。
        if state
            .providers
            .iter()
            .any(|p| p.app == app && p.category != ProviderCategory::Official)
        {
            return Ok(None);
        }
        let Some((id, name)) = self.import_live_as_provider(app, &mut state, engine, paths)? else {
            return Ok(None);
        };
        // 收编到的既有配置就是当前 live 生效项 → 置 current，令展示与真实一致。
        state.current.insert(app, id);
        self.store.save(&state)?;
        Ok(Some(name))
    }

    // ---------- 中转站 CRUD ----------

    pub fn add_relay(
        &self,
        mut profile: ProviderProfile,
    ) -> Result<ProviderProfile, ProviderError> {
        validate_relay_fields(&profile)?;
        profile.category = ProviderCategory::Relay;
        let mut state = self.store.load()?;
        if state.providers.iter().any(|p| p.id == profile.id) {
            return Err(ProviderError::InvalidOperation(format!(
                "Provider id 已存在：{}",
                profile.id
            )));
        }
        if profile.app == ClientApp::Codex && profile.codex_slug.is_none() {
            let taken: Vec<String> = state
                .providers
                .iter()
                .filter_map(|p| p.codex_slug.clone())
                .collect();
            profile.codex_slug = Some(generate_codex_slug(&profile.name, &taken));
        }
        state.providers.push(profile.clone());
        self.store.save(&state)?;
        Ok(profile)
    }

    /// 批量收编外部工具（cc-switch）迁出的中转站：Key 转存系统凭据库，一次
    /// load-save 落库。逐条走 [`Self::add_relay`] 也能做，但那是 N 次
    /// load-modify-save，中途失败会留下半截状态。
    ///
    /// 幂等：同 app 同 base_url 已有条目即跳过，重复导入只会得到一堆 skipped。
    pub fn import_relays(
        &self,
        incoming: Vec<(ProviderProfile, Option<String>)>,
    ) -> Result<RelayImportOutcome, ProviderError> {
        let mut state = self.store.load()?;
        let mut outcome = RelayImportOutcome::default();
        for (mut profile, key) in incoming {
            profile.category = ProviderCategory::Relay;
            // 校验必须排在去重前面：没有 base_url 的条目拿 `None` 去比，会撞上
            // 官方项（它的 base_url 恒为 None）而被当成"已经有了"静默跳过，
            // 用户就只看到一条无声消失的供应商。
            //
            // 附加片段带了受管键时丢掉片段本身，而不是丢掉整条供应商——
            // base_url 与 Key 才是用户真正搬不动的东西。
            if validate_relay_fields(&profile).is_err() {
                profile.extra_settings = serde_json::Value::Null;
                if let Err(e) = validate_relay_fields(&profile) {
                    outcome.failed.push(format!("{}：{e}", profile.name));
                    continue;
                }
            }
            if state
                .providers
                .iter()
                .any(|p| p.app == profile.app && p.base_url == profile.base_url)
            {
                outcome.skipped.push(profile.name);
                continue;
            }
            if let Some(key) = key {
                self.backfill_key(&mut profile, &key)?;
            }
            if profile.app == ClientApp::Codex {
                let taken: Vec<String> = state
                    .providers
                    .iter()
                    .filter_map(|p| p.codex_slug.clone())
                    .collect();
                profile.codex_slug = Some(generate_codex_slug(&profile.name, &taken));
            }
            // 排在该 app 已有条目之后，不与用户手排的顺序抢位置。
            profile.sort_order = state
                .providers
                .iter()
                .filter(|p| p.app == profile.app)
                .map(|p| p.sort_order)
                .max()
                .unwrap_or(-1)
                + 1;
            outcome.imported.push(profile.name.clone());
            state.providers.push(profile);
        }
        self.store.save(&state)?;
        Ok(outcome)
    }

    /// 更新中转站的可编辑字段（名称/地址/模型映射/认证字段/备注/官网/排序）。
    /// `codex_slug` 与 `api_key_ref` 为冻结字段，以存量为准。
    ///
    /// 保存即生效：
    /// 若编辑的是当前生效的 provider，落库后立即把新值重写进 live 配置，用户不必
    /// 再手动切换一次。复用 [`Self::switch`] 的写入臂——同一 provider 重新应用会
    /// 以上次 applied_fragment 为清理基准，删掉被编辑移除的键、写入新值，其他 app
    /// 不受影响。非当前 provider 只落库，下次切到它时自然生效。
    pub fn update_relay(
        &self,
        profile: ProviderProfile,
        engine: &Engine,
        paths: &ClientPaths,
    ) -> Result<(), ProviderError> {
        validate_relay_fields(&profile)?;
        let mut state = self.store.load()?;
        let slot = state
            .providers
            .iter_mut()
            .find(|p| p.id == profile.id)
            .ok_or_else(|| ProviderError::NotFound(profile.id.clone()))?;
        slot.name = profile.name;
        slot.base_url = profile.base_url;
        slot.model = profile.model;
        slot.model_haiku = profile.model_haiku;
        slot.model_sonnet = profile.model_sonnet;
        slot.model_opus = profile.model_opus;
        slot.model_fable = profile.model_fable;
        slot.model_subagent = profile.model_subagent;
        slot.api_key_field = profile.api_key_field;
        slot.codex_wire_api = profile.codex_wire_api;
        slot.extra_settings = profile.extra_settings;
        slot.common_config_enabled = profile.common_config_enabled;
        slot.endpoint_candidates = profile.endpoint_candidates;
        slot.models = profile.models;
        slot.headers = profile.headers;
        // 只有 Hermes 的配置里有这个字段；别家存了也落不到任何地方（同 upstream_api）。
        slot.context_length = profile
            .context_length
            .filter(|_| slot.app == ClientApp::Hermes);
        // Claude Desktop 的写入模式与四档路由——漏了这两行的话，用户在编辑面板
        // 改完 direct/proxy 或路由表、点保存，改动只是消失（既有缺口，字段清点时逮到）。
        slot.desktop_mode = profile.desktop_mode;
        slot.desktop_routes = profile.desktop_routes;
        // 客户端配置里没有这一维度时不收下：留着只会让人以为设置生效了。
        slot.upstream_api = profile
            .upstream_api
            .filter(|_| slot.app.supports_upstream_api());
        slot.codex_query_params = profile.codex_query_params;
        slot.codex_context_window_1m =
            slot.app == ClientApp::Codex && profile.codex_context_window_1m;
        slot.codex_auto_compact_limit = if slot.codex_context_window_1m {
            profile.codex_auto_compact_limit.filter(|&n| n > 0)
        } else {
            None
        };
        slot.icon = profile.icon;
        slot.notes = profile.notes;
        slot.website_url = profile.website_url;
        slot.sort_order = profile.sort_order;
        let id = slot.id.clone();
        self.store.save(&state)?;

        // 编辑的是当前生效项 → 立即重写 live（switch 从 store 重新加载已存的新值）。
        self.reapply_current(&id, engine, paths)?;
        Ok(())
    }

    /// 若 `id` 是某 app 当前生效项，则立即把（已落库的）最新配置重写进 live，
    /// 实现"编辑当前 provider 保存即生效"（对齐 cc-switch update：is_current →
    /// write_live_with_common_config）。各 mutation 路径（改字段/换 Key/编辑官方项）
    /// 复用此助手。provider id 全局唯一，至多命中一个 app。返回是否发生重写。
    pub fn reapply_current(
        &self,
        id: &str,
        engine: &Engine,
        paths: &ClientPaths,
    ) -> Result<bool, ProviderError> {
        let state = self.store.load()?;
        let Some(app) = state
            .current
            .iter()
            .find(|(_, current_id)| current_id.as_str() == id)
            .map(|(app, _)| *app)
        else {
            return Ok(false);
        };
        self.switch(app, id, engine, paths)?;
        Ok(true)
    }

    /// 重写某 app 当前生效项的 live（用于改全局通用配置后即时生效——通用配置
    /// 按 app 一份，与具体 provider id 无关，故按 app 定位当前项）。返回是否重写。
    fn reapply_current_app(
        &self,
        app: ClientApp,
        engine: &Engine,
        paths: &ClientPaths,
    ) -> Result<bool, ProviderError> {
        let Some(id) = self.store.load()?.current.get(&app).cloned() else {
            return Ok(false);
        };
        self.switch(app, &id, engine, paths)?;
        Ok(true)
    }

    /// 编辑官方订阅的可编辑项（附加配置片段 / 通用配置开关）。官方项已落库，
    /// 这里直接改那条记录；官方项不存在（用户删过）则报 NotFound，由 UI 引导重新添加。
    /// 编辑的是当前生效的官方项时立即重写 live（与 [`Self::update_relay`] 同口径）。
    pub fn update_official(
        &self,
        app: ClientApp,
        extra_settings: serde_json::Value,
        common_config_enabled: bool,
        engine: &Engine,
        paths: &ClientPaths,
    ) -> Result<(), ProviderError> {
        validate_fragment(app, &extra_settings, "附加配置")?;
        let id = official_id_for(app);
        let mut state = self.store.load()?;
        let slot = state
            .providers
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| ProviderError::NotFound(id.to_owned()))?;
        slot.extra_settings = extra_settings;
        slot.common_config_enabled = common_config_enabled;
        self.store.save(&state)?;
        self.reapply_current(id, engine, paths)?;
        Ok(())
    }

    /// 删除供应商（官方项同样可删，删了不会在下次启动复活），并连带清理其 Secret。
    /// 使用中的项不可删除：直接删会让 live 配置残留指向（Codex 的 slug 表更会因
    /// slug 随 profile 消失而永远清不掉），必须先切走再删。
    pub fn delete(&self, id: &str) -> Result<(), ProviderError> {
        let mut state = self.store.load()?;
        // 影子条目的生命周期归统一供应商管：直接删会在下次同步时复活，
        // 引导用户去统一供应商里删除或关闭对应应用。
        if id.starts_with(crate::domain::UNIFIED_ID_PREFIX)
            && state
                .unified
                .iter()
                .any(|u| id.ends_with(u.id.as_str()) && !u.id.is_empty())
        {
            return Err(ProviderError::InvalidOperation(
                "该条目由统一供应商管理，请在统一供应商中删除或关闭对应应用".into(),
            ));
        }
        if state.current.values().any(|current_id| current_id == id) {
            return Err(ProviderError::InvalidOperation(
                "供应商正在使用中，请先切换到其他供应商再删除".into(),
            ));
        }
        let index = state
            .providers
            .iter()
            .position(|p| p.id == id)
            .ok_or_else(|| ProviderError::NotFound(id.to_owned()))?;
        let removed = state.providers.remove(index);
        state.drop_from_failover_queues(id);
        if let Some(reference) = &removed.api_key_ref {
            self.secrets.delete(reference)?;
        }
        self.store.save(&state)?;
        Ok(())
    }

    // ---------- Secret ----------

    /// 录入/更新 API Key：明文只进系统凭据库，Profile 保存引用。
    pub fn set_api_key(&self, id: &str, key: &Secret) -> Result<(), ProviderError> {
        let mut state = self.store.load()?;
        let profile = state
            .providers
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| ProviderError::NotFound(id.to_owned()))?;
        let reference = profile
            .api_key_ref
            .clone()
            .unwrap_or_else(SecretReference::generate);
        self.secrets.set(&reference, key)?;
        profile.api_key_ref = Some(reference);
        self.store.save(&state)?;
        Ok(())
    }

    // ---------- 通用配置片段（对齐 cc-switch Common Config，按 app 各一份） ----------

    /// 读取某 app 的全局通用配置片段（None = 未配置）。
    pub fn common_snippet(
        &self,
        app: ClientApp,
    ) -> Result<Option<serde_json::Value>, ProviderError> {
        let state = self.store.load()?;
        Ok(adapter_for(app).common_snippet(&state).cloned())
    }

    /// 保存/清空某 app 的全局通用配置片段（Claude 为 settings.json 子集，
    /// Codex 为 config.toml 子树的 JSON 表达）。启用了 `common_config_enabled`
    /// 的当前生效项立即重写 live（对齐"保存即生效"），其余项下次切换时生效。
    pub fn set_common_snippet(
        &self,
        app: ClientApp,
        snippet: Option<serde_json::Value>,
        engine: &Engine,
        paths: &ClientPaths,
    ) -> Result<(), ProviderError> {
        if let Some(value) = &snippet {
            validate_fragment(app, value, "通用配置")?;
        }
        let mut state = self.store.load()?;
        let slot = adapter_for(app).common_snippet_slot(&mut state);
        *slot = snippet.filter(|v| !v.is_null());
        self.store.save(&state)?;
        // 当前项若启用了通用配置，effective_fragment 会纳入新片段——重写即生效；
        // 未启用则 switch 幂等无副作用。
        self.reapply_current_app(app, engine, paths)?;
        Ok(())
    }

    // ---------- 统一供应商（Universal Provider） ----------

    /// 统一供应商列表（按 sort_order）。
    pub fn list_unified(&self) -> Result<Vec<UnifiedProvider>, ProviderError> {
        let mut unified = self.store.load()?.unified;
        unified.sort_by_key(|u| u.sort_order);
        Ok(unified)
    }

    /// 新建/更新统一供应商，并立即同步影子条目（对齐 cc-switch 的
    /// upsert+sync；我们的影子是结构化派生数据，无需独立 sync 步骤）。
    /// `api_key_ref` 为冻结字段，以存量为准。
    /// 编辑后若某影子正当前生效，立即重写其 live（保存即生效）。
    pub fn upsert_unified(
        &self,
        mut unified: UnifiedProvider,
        engine: &Engine,
        paths: &ClientPaths,
    ) -> Result<UnifiedProvider, ProviderError> {
        if unified.name.trim().is_empty() {
            return Err(ProviderError::InvalidOperation("名称不能为空".into()));
        }
        if unified.base_url.trim().is_empty() {
            return Err(ProviderError::InvalidOperation(
                "统一供应商必须配置 base_url".into(),
            ));
        }
        let mut state = self.store.load()?;
        match state.unified.iter_mut().find(|u| u.id == unified.id) {
            Some(slot) => {
                unified.api_key_ref = slot.api_key_ref.clone();
                *slot = unified.clone();
            }
            None => state.unified.push(unified.clone()),
        }
        self.sync_unified_shadows(&mut state, &unified)?;
        self.store.save(&state)?;
        self.reapply_unified_shadows(&unified.id, engine, paths)?;
        Ok(unified)
    }

    /// 删除统一供应商：先确认影子条目未在使用中，再连带清理影子与全部 Secret。
    pub fn delete_unified(&self, id: &str) -> Result<(), ProviderError> {
        let mut state = self.store.load()?;
        let index = state
            .unified
            .iter()
            .position(|u| u.id == id)
            .ok_or_else(|| ProviderError::NotFound(id.to_owned()))?;
        for app in [ClientApp::Claude, ClientApp::Codex] {
            let shadow_id = unified_shadow_id(app, id);
            if state.current.values().any(|current| *current == shadow_id) {
                return Err(ProviderError::InvalidOperation(
                    "统一供应商正在使用中，请先切换到其他供应商再删除".into(),
                ));
            }
        }
        for app in [ClientApp::Claude, ClientApp::Codex] {
            let shadow_id = unified_shadow_id(app, id);
            if let Some(pos) = state.providers.iter().position(|p| p.id == shadow_id) {
                let removed = state.providers.remove(pos);
                if let Some(reference) = &removed.api_key_ref {
                    self.secrets.delete(reference)?;
                }
                state.drop_from_failover_queues(&shadow_id);
            }
        }
        let removed = state.unified.remove(index);
        if let Some(reference) = &removed.api_key_ref {
            self.secrets.delete(reference)?;
        }
        self.store.save(&state)?;
        Ok(())
    }

    /// 录入/更新统一供应商 API Key，并把明文副本下发到各影子条目的 Secret。
    /// 轮换后若某影子正当前生效，立即用新 Key 重写其 live（Key 轮换即时生效）。
    pub fn set_unified_api_key(
        &self,
        id: &str,
        key: &Secret,
        engine: &Engine,
        paths: &ClientPaths,
    ) -> Result<(), ProviderError> {
        let mut state = self.store.load()?;
        let unified = state
            .unified
            .iter_mut()
            .find(|u| u.id == id)
            .ok_or_else(|| ProviderError::NotFound(id.to_owned()))?;
        let reference = unified
            .api_key_ref
            .clone()
            .unwrap_or_else(SecretReference::generate);
        self.secrets.set(&reference, key)?;
        unified.api_key_ref = Some(reference);
        for app in [ClientApp::Claude, ClientApp::Codex] {
            let shadow_id = unified_shadow_id(app, id);
            if let Some(shadow) = state.providers.iter_mut().find(|p| p.id == shadow_id) {
                let shadow_ref = shadow
                    .api_key_ref
                    .clone()
                    .unwrap_or_else(SecretReference::generate);
                self.secrets.set(&shadow_ref, key)?;
                shadow.api_key_ref = Some(shadow_ref);
            }
        }
        self.store.save(&state)?;
        self.reapply_unified_shadows(id, engine, paths)?;
        Ok(())
    }

    /// 统一供应商变更（编辑字段 / 轮换 Key）后，把当前正生效的影子重写进 live。
    /// 两个影子各自独立判断是否当前项，reapply_current 内部对非当前项短路。
    fn reapply_unified_shadows(
        &self,
        unified_id: &str,
        engine: &Engine,
        paths: &ClientPaths,
    ) -> Result<(), ProviderError> {
        for app in [ClientApp::Claude, ClientApp::Codex] {
            let shadow_id = unified_shadow_id(app, unified_id);
            self.reapply_current(&shadow_id, engine, paths)?;
        }
        Ok(())
    }

    /// 把统一供应商物化成各 app 的影子条目（开=创建/覆盖，关=移除）。
    /// 影子是普通 ProviderProfile：切换、托盘、连通性测试全走既有机制。
    fn sync_unified_shadows(
        &self,
        state: &mut ProviderState,
        unified: &UnifiedProvider,
    ) -> Result<(), ProviderError> {
        // 前置校验：任何"待关闭且正在使用中"的影子直接拒绝，
        // 避免半途失败时已经删掉另一个 app 的 Secret（悬空引用）。
        // 统一供应商只物化 Claude/Codex 影子：Gemini 原生协议的多协议网关
        // 罕见，暂不纳入（unified_apps_enabled 对 Gemini 恒 false）。
        for app in [ClientApp::Claude, ClientApp::Codex] {
            let enabled = unified_apps_enabled(unified, app);
            let shadow_id = unified_shadow_id(app, &unified.id);
            if !enabled && state.current.values().any(|c| *c == shadow_id) {
                return Err(ProviderError::InvalidOperation(format!(
                    "「{}」在 {} 下正在使用中，请先切走再关闭该应用",
                    unified.name,
                    app_display(app)
                )));
            }
        }
        for app in [ClientApp::Claude, ClientApp::Codex] {
            let shadow_id = unified_shadow_id(app, &unified.id);
            let enabled = unified_apps_enabled(unified, app);
            let existing_pos = state.providers.iter().position(|p| p.id == shadow_id);
            if !enabled {
                if let Some(pos) = existing_pos {
                    let removed = state.providers.remove(pos);
                    if let Some(reference) = &removed.api_key_ref {
                        self.secrets.delete(reference)?;
                    }
                    state.drop_from_failover_queues(&shadow_id);
                }
                continue;
            }

            let mut shadow = match existing_pos {
                Some(pos) => state.providers[pos].clone(),
                None => {
                    let mut fresh = ProviderProfile::new_relay(app, unified.name.clone());
                    fresh.id = shadow_id.clone();
                    fresh
                }
            };
            shadow.name = unified.name.clone();
            shadow.notes = unified.notes.clone();
            shadow.website_url = unified.website_url.clone();
            shadow.sort_order = unified.sort_order;
            shadow.api_key_field = ClaudeAuthField::AuthToken;
            match app {
                ClientApp::Claude => {
                    // Claude：base_url 原样，模型角色映射透传。
                    shadow.base_url = Some(unified.base_url.clone());
                    shadow.model = unified.model.clone();
                    shadow.model_haiku = unified.model_haiku.clone();
                    shadow.model_sonnet = unified.model_sonnet.clone();
                    shadow.model_opus = unified.model_opus.clone();
                    shadow.model_fable = unified.model_fable.clone();
                }
                ClientApp::Codex => {
                    // Codex：三态 /v1 补全；NewAPI 型网关反代 OpenAI，走 responses。
                    shadow.base_url = Some(codex_base_url_for_gateway(&unified.base_url));
                    shadow.model = unified.codex_model.clone();
                    shadow.codex_wire_api = CodexWireApi::Responses;
                    if shadow.codex_slug.is_none() {
                        let taken: Vec<String> = state
                            .providers
                            .iter()
                            .filter_map(|p| p.codex_slug.clone())
                            .collect();
                        shadow.codex_slug = Some(generate_codex_slug(&unified.name, &taken));
                    }
                }
                // 不可达：unified_apps_enabled 对 Gemini / Grok / Claude Desktop
                // 恒 false（上方已 continue）。
                ClientApp::Gemini
                | ClientApp::Grok
                | ClientApp::OpenCode
                | ClientApp::OpenClaw
                | ClientApp::Hermes
                | ClientApp::ClaudeDesktop => {
                    shadow.base_url = Some(unified.base_url.clone());
                    shadow.model = unified.model.clone();
                }
            }
            // Key 副本：统一供应商已录入 Key 时下发到影子自己的 Secret。
            if let Some(unified_ref) = &unified.api_key_ref {
                let secret = self.secrets.get(unified_ref)?;
                let shadow_ref = shadow
                    .api_key_ref
                    .clone()
                    .unwrap_or_else(SecretReference::generate);
                self.secrets.set(&shadow_ref, &secret)?;
                shadow.api_key_ref = Some(shadow_ref);
            }
            match existing_pos {
                Some(pos) => state.providers[pos] = shadow,
                None => state.providers.push(shadow),
            }
        }
        Ok(())
    }

    // ---------- 端点测速（对齐 cc-switch test_api_endpoints） ----------

    // 见文件底部的自由函数 `test_endpoints`（无需 store/secrets，独立可调）。

    // ---------- Coding Plan 配额查询 ----------

    /// 查询 Provider 用量/余额：Coding Plan（Kimi/智谱/MiniMax）→ 按余额计费的
    /// 中转站（DeepSeek 等），按 base_url 依次识别；官方订阅与都不命中的一律不支持。
    /// 中转站 Key 从系统凭据库物化，仅在请求瞬时存在。
    pub async fn quota(&self, id: &str) -> Result<crate::quota::ProviderQuota, ProviderError> {
        let state = self.store.load()?;
        let profile = state
            .providers
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| ProviderError::NotFound(id.to_owned()))?;

        // 官方订阅一律不查：额度接口属于官方服务，MCPane 不向其发任何请求。
        if profile.category == ProviderCategory::Official {
            return Err(ProviderError::InvalidOperation(
                "官方订阅不支持用量查询".into(),
            ));
        }

        let base_url = require_base_url(profile)?;
        if let Some(vendor) = crate::quota::detect_coding_plan(base_url) {
            let key = self.materialize_key(profile)?;
            return crate::quota::query_quota(vendor, base_url, key.expose())
                .await
                .map_err(ProviderError::Connectivity);
        }
        if let Some(vendor) = crate::balance::detect_balance_provider(base_url) {
            let key = self.materialize_key(profile)?;
            return crate::balance::query_balance(vendor, base_url, key.expose())
                .await
                .map_err(ProviderError::Connectivity);
        }
        Err(ProviderError::InvalidOperation(
            "该供应商不支持用量查询".into(),
        ))
    }

    /// 揭示某已存 provider 的 Key 明文（编辑时拉取模型用；仅服务端调用，Key 用完即弃、
    /// 不回传前端）。拉取本身由命令层用**表单传入的 base_url** 调 free fn，
    /// 以便未保存的地址改动也能即时生效，并让 fetch 错误原样透出（不套连通性前缀）。
    pub fn reveal_key(&self, id: &str) -> Result<Secret, ProviderError> {
        let state = self.store.load()?;
        let profile = state
            .providers
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| ProviderError::NotFound(id.to_owned()))?;
        self.materialize_key(profile)
    }

    // ---------- 连通性 ----------

    /// 连通性测试：对中转站 base_url 发起真实 HTTP 请求。
    /// 能得到任何 HTTP 响应（含 401/404）即视为网络可达；协议级验证属 P1。
    pub async fn test_connectivity(&self, id: &str) -> Result<u16, ProviderError> {
        let state = self.store.load()?;
        let profile = state
            .providers
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| ProviderError::NotFound(id.to_owned()))?;
        let url = profile
            .base_url
            .as_deref()
            .ok_or_else(|| ProviderError::Connectivity("未配置 base_url".into()))?;
        let response = crate::http_client(url)
            .get(url)
            .timeout(std::time::Duration::from_secs(8))
            .send()
            .await
            .map_err(|e| ProviderError::Connectivity(e.to_string()))?;
        Ok(response.status().as_u16())
    }

    // ---------- 切换 ----------

    /// 切换某个 app 的当前 Provider（官方订阅或中转站）。
    pub fn switch(
        &self,
        app: ClientApp,
        provider_id: &str,
        engine: &Engine,
        paths: &ClientPaths,
    ) -> Result<SwitchOutcome, ProviderError> {
        self.switch_with(app, provider_id, engine, paths, true)
    }

    /// `allow_router = false` 时无视路由开关按直连写 live（MCPane 退出前还原用）。
    fn switch_with(
        &self,
        app: ClientApp,
        provider_id: &str,
        engine: &Engine,
        paths: &ClientPaths,
        allow_router: bool,
    ) -> Result<SwitchOutcome, ProviderError> {
        let mut state = self.store.load()?;
        let target = resolve_target(&state, app, provider_id)?;
        let mut warnings = Vec::new();
        let adapter = adapter_for(app);

        // 首次接管（current 为 None）先收编 live 里既有的第三方配置：
        // 后续写入会覆盖这些值，而快照脱敏的前提是"Key 可从凭据库重新物化"——
        // 不收编，用户在 MCPane 之外配置的 Key 会在第一次切换时不可恢复地丢失。
        // 收编失败即中止切换：继续走下去就是销毁不可恢复的凭据。
        if !state.current.contains_key(&app)
            && let Some((_, name)) = self.import_live_as_provider(app, &mut state, engine, paths)?
        {
            // 立即持久化：切换臂失败也不能丢掉刚收编的 Profile。
            self.store.save(&state)?;
            warnings.push(format!(
                "检测到 {} 已有第三方配置，已导入为「{name}」，可随时切回",
                app_display(app)
            ));
        }

        // 切走前把 live 配置回填到当前项（保留用户手改；失败不阻断）。
        let snippet = adapter.common_snippet(&state).cloned();
        let (live_path, live_format) = adapter.live_config(paths);
        let current_id = state.current.get(&app).cloned();
        // 清理基准：优先用上次切换实际写入的片段（applied_fragments），
        // profile 在两次切换之间被编辑过也能准确清理；老状态无记录时
        // 回退为按当前 profile 反推。
        let applied = state.applied_fragments.get(&app).cloned();
        let previous_relay = match current_id {
            Some(ref id) if *id != target.id => {
                if let Some(pos) = state.providers.iter().position(|p| p.id == *id) {
                    if state.providers[pos].category == ProviderCategory::Official {
                        // 官方项没有 base_url/Key 可回填，只把附加片段与 live 对齐后
                        // 作为清理基准——走 relay 的回填会把 live 里的第三方值
                        // 倒灌进官方记录，把它污染成一条中转站。
                        if let Ok(loaded) = engine.load(live_path, live_format)
                            && let Ok(doc) = ConfigDocument::parse(&loaded.content, live_format)
                        {
                            backfill_fragment(&mut state.providers[pos].extra_settings, &doc);
                        }
                    } else if let Err(error) =
                        self.backfill(app, &mut state.providers[pos], engine, paths)
                    {
                        warnings.push(format!("回填当前配置失败（已忽略）：{error}"));
                    }
                    Some(PreviousRelay {
                        model: state.providers[pos].model.clone(),
                        fragment: applied.unwrap_or_else(|| {
                            effective_fragment(&state.providers[pos], snippet.as_ref())
                        }),
                    })
                } else {
                    None
                }
            }
            // 同一供应商重新启用：不回填，但仍以上次写入的片段为清理基准
            //（片段被编辑删掉的键要清残留）。
            Some(_) => applied.map(|fragment| PreviousRelay {
                model: None,
                fragment,
            }),
            None => None,
        };
        let target_effective = effective_fragment(&target, snippet.as_ref());
        let router = if allow_router && target.category == ProviderCategory::Relay {
            router_endpoint(&mut state, app)
        } else {
            None
        };

        let written = match (app, target.category) {
            (ClientApp::Claude, ProviderCategory::Relay) => self.switch_claude_relay(
                &target,
                router.as_ref(),
                previous_relay.as_ref(),
                &target_effective,
                engine,
                paths,
            )?,
            (ClientApp::Claude, ProviderCategory::Official) => self.switch_claude_official(
                previous_relay.as_ref(),
                &target_effective,
                engine,
                paths,
            )?,
            (ClientApp::Codex, ProviderCategory::Relay) => self.switch_codex_relay(
                &target,
                router.as_ref(),
                &mut state,
                previous_relay.as_ref(),
                &target_effective,
                engine,
                paths,
                &mut warnings,
            )?,
            (ClientApp::Codex, ProviderCategory::Official) => self.switch_codex_official(
                &mut state,
                previous_relay.as_ref(),
                &target_effective,
                engine,
                paths,
                &mut warnings,
            )?,
            // OpenCode 是 additive：Relay = 写进配置（"启用"）；
            // 它没有官方形态，Official 分支由 resolve_target 之前就挡住了。
            (ClientApp::Hermes, ProviderCategory::Relay) => self.enable_additive_provider(
                &target,
                previous_relay.as_ref(),
                &target_effective,
                engine,
                paths,
                &mut warnings,
            )?,
            (ClientApp::Hermes, ProviderCategory::Official) => {
                return Err(ProviderError::InvalidOperation(
                    "Hermes 没有官方订阅形态".into(),
                ));
            }
            (ClientApp::OpenClaw, ProviderCategory::Relay) => self.enable_additive_provider(
                &target,
                previous_relay.as_ref(),
                &target_effective,
                engine,
                paths,
                &mut warnings,
            )?,
            (ClientApp::OpenClaw, ProviderCategory::Official) => {
                return Err(ProviderError::InvalidOperation(
                    "OpenClaw 没有官方订阅形态".into(),
                ));
            }
            (ClientApp::OpenCode, ProviderCategory::Relay) => self.enable_additive_provider(
                &target,
                previous_relay.as_ref(),
                &target_effective,
                engine,
                paths,
                &mut warnings,
            )?,
            (ClientApp::OpenCode, ProviderCategory::Official) => {
                return Err(ProviderError::InvalidOperation(
                    "OpenCode 没有官方订阅形态".into(),
                ));
            }
            (ClientApp::Grok, ProviderCategory::Relay) => self.switch_grok_relay(
                &target,
                router.as_ref(),
                previous_relay.as_ref(),
                &target_effective,
                engine,
                paths,
            )?,
            (ClientApp::Grok, ProviderCategory::Official) => self.switch_grok_official(
                &state,
                previous_relay.as_ref(),
                &target_effective,
                engine,
                paths,
            )?,
            (ClientApp::Gemini, ProviderCategory::Relay) => self.switch_gemini_relay(
                &target,
                router.as_ref(),
                previous_relay.as_ref(),
                &target_effective,
                engine,
                paths,
                &mut warnings,
            )?,
            (ClientApp::Gemini, ProviderCategory::Official) => self.switch_gemini_official(
                previous_relay.as_ref(),
                &target_effective,
                engine,
                paths,
                &mut warnings,
            )?,
            (ClientApp::ClaudeDesktop, ProviderCategory::Relay) => {
                self.switch_claude_desktop_relay(&target, &mut state, paths, &mut warnings)?
            }
            (ClientApp::ClaudeDesktop, ProviderCategory::Official) => {
                self.switch_claude_desktop_official(paths, &mut warnings)?
            }
        };

        state.applied_fragments.insert(app, target_effective);
        state.current.insert(app, target.id.clone());
        self.store.save(&state)?;
        Ok(SwitchOutcome { written, warnings })
    }

    /// Claude 中转站：env 写 base_url + token（Keychain 物化）+ 可选 model，
    /// 并深合并附加配置片段（含叠加的通用配置）。
    /// 写进 live 的（地址, 凭据）：路由接管时为本地路由 + 网关 token，否则为真实值。
    fn relay_endpoint(
        &self,
        profile: &ProviderProfile,
        router: Option<&RouterEndpoint>,
    ) -> Result<(String, String), ProviderError> {
        Ok(match router {
            Some(router) => (router.base_url.clone(), router.token.clone()),
            None => (
                require_base_url(profile)?.to_owned(),
                self.materialize_key(profile)?.expose().to_owned(),
            ),
        })
    }

    fn switch_claude_relay(
        &self,
        profile: &ProviderProfile,
        router: Option<&RouterEndpoint>,
        previous_relay: Option<&PreviousRelay>,
        effective: &serde_json::Value,
        engine: &Engine,
        paths: &ClientPaths,
    ) -> Result<bool, ProviderError> {
        if profile.claude_api_format != ClaudeApiFormat::Anthropic && router.is_none() {
            return Err(ProviderError::InvalidOperation(
                "该供应商使用 OpenAI 协议接口，Claude Code 无法直连；\
                 请在供应商页开启本地路由（由 MCPane 转换协议）"
                    .into(),
            ));
        }
        // 路由接管：live 指向本地路由 + 网关 token，真实地址与 Key 只进路由内存。
        let (base_url, token, auth_field) = match router {
            Some(router) => (
                router.base_url.clone(),
                router.token.clone(),
                ClaudeAuthField::AuthToken,
            ),
            None => (
                require_base_url(profile)?.to_owned(),
                self.materialize_key(profile)?.expose().to_owned(),
                profile.api_key_field,
            ),
        };

        let loaded = engine.load(&paths.claude_settings, ConfigFormat::Json)?;
        let live_doc = ConfigDocument::parse(&loaded.content, ConfigFormat::Json).ok();

        // 认证键按 profile 形态写入（AUTH_TOKEN 或 API_KEY），另一形态清除。
        let mut pairs: Vec<(&str, &str)> = vec![
            ("ANTHROPIC_BASE_URL", base_url.as_str()),
            (auth_field.env_key(), token.as_str()),
        ];
        // 模型映射：主模型有值时，未显式覆盖的 haiku/sonnet/opus/fable 回落到主模型
        //（对齐 cc-switch normalize_claude_models）——中转站通常不托管官方默认模型名
        // claude-haiku-* / -sonnet- / -opus- / -fable-，不回落会让后台/子代理流量以
        // 官方默认名打中转、命中不存在的模型 404。有效值写入、无值清除（覆盖上一站残留）。
        //
        // fable 的回落链是 fable → opus → 主模型（镜像 cc-switch
        // parseModelsFromConfig 与 Desktop 代理里 fable 降级到 opus 档的方向）：
        // 显式钉过 opus 时它比主模型更接近顶配档语义。
        // subagent 不回落——空着就让 Claude Code 走正常别名解析（即上面四个槽）。
        let model_envs: [(&str, Option<&String>); 6] = [
            ("ANTHROPIC_MODEL", profile.model.as_ref()),
            (
                "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                profile.model_haiku.as_ref().or(profile.model.as_ref()),
            ),
            (
                "ANTHROPIC_DEFAULT_SONNET_MODEL",
                profile.model_sonnet.as_ref().or(profile.model.as_ref()),
            ),
            (
                "ANTHROPIC_DEFAULT_OPUS_MODEL",
                profile.model_opus.as_ref().or(profile.model.as_ref()),
            ),
            (
                "ANTHROPIC_DEFAULT_FABLE_MODEL",
                profile
                    .model_fable
                    .as_ref()
                    .or(profile.model_opus.as_ref())
                    .or(profile.model.as_ref()),
            ),
            (
                "CLAUDE_CODE_SUBAGENT_MODEL",
                profile.model_subagent.as_ref(),
            ),
        ];
        for &(key_name, value) in &model_envs {
            if let Some(model) = value {
                pairs.push((key_name, model.as_str()));
            }
        }
        let mut plan = ClaudeAdapter
            .provider_env_patch(&pairs)
            .unwrap_or_else(|| PatchPlan::new(Vec::new()));
        plan.ops.push(PatchOp::Remove {
            path: vec!["env".into(), auth_field.other_env_key().into()],
        });
        for &(key_name, value) in &model_envs {
            if value.is_none() {
                plan.ops.push(PatchOp::Remove {
                    path: vec!["env".into(), key_name.to_owned()],
                });
            }
        }
        // 历史遗留键（SMALL_FAST_MODEL 等）：迁移用户 live 里的残留一并清除，
        // 否则会持续把 haiku 流量路由到旧中转站的模型名。
        for key_name in mcpane_adapter_claude::LEGACY_ENV_KEYS {
            plan.ops.push(PatchOp::Remove {
                path: vec!["env".into(), key_name.to_owned()],
            });
        }
        // 附加配置片段：先按值匹配清掉上一个中转站的片段残留（不动用户手改），
        // 再深合并写入本次的有效片段（profile 片段 ⊕ 通用配置，后者覆盖同名键）。
        let new_leaves = fragment_leaves(effective);
        if let (Some(previous), Some(doc)) = (previous_relay, live_doc.as_ref()) {
            plan.ops
                .extend(fragment_removal_ops(doc, &previous.fragment, &new_leaves));
        }
        for (path, value) in new_leaves {
            plan.ops.push(PatchOp::Set { path, value });
        }

        // 快照脱敏：快照存的是**写前**内容，必须同时脱敏写前 live 里的旧
        // token（relay→relay 时是上一个中转站的 Key）与即将写入的新 token。
        // Key 均可从系统凭据库重新物化（切走前已 backfill），允许剔除。
        let redactions = claude_snapshot_redactions(&loaded.content, Some(token.as_str()));
        let redactor = move |content: &str| redact_all(content, &redactions);
        let outcome = engine.apply(
            &loaded,
            &plan,
            &format!("切换 Claude Code 到中转站「{}」", profile.name),
            None,
            Some(&redactor),
        )?;
        Ok(outcome.written)
    }

    /// Claude 官方订阅：移除 MCPane 管理的全部 env 键（OAuth 登录自然生效），
    /// 并深合并官方订阅自己的附加配置片段（官方同样可编辑，对齐 cc-switch）。
    fn switch_claude_official(
        &self,
        previous_relay: Option<&PreviousRelay>,
        effective: &serde_json::Value,
        engine: &Engine,
        paths: &ClientPaths,
    ) -> Result<bool, ProviderError> {
        let mut plan = ClaudeAdapter
            .remove_provider_env_patch()
            .unwrap_or_else(|| PatchPlan::new(Vec::new()));
        let loaded = engine.load(&paths.claude_settings, ConfigFormat::Json)?;
        let new_leaves = fragment_leaves(effective);
        // 上一个中转站的附加配置片段：按值匹配深移除（不动用户手改）。
        if let Some(previous) = previous_relay
            && let Ok(doc) = ConfigDocument::parse(&loaded.content, ConfigFormat::Json)
        {
            plan.ops
                .extend(fragment_removal_ops(&doc, &previous.fragment, &new_leaves));
        }
        for (path, value) in new_leaves {
            plan.ops.push(PatchOp::Set { path, value });
        }
        // relay→official 的写前快照里有上一个中转站的 token，同样必须脱敏。
        let redactions = claude_snapshot_redactions(&loaded.content, None);
        let redactor = move |content: &str| redact_all(content, &redactions);
        let outcome = engine.apply(
            &loaded,
            &plan,
            "切换 Claude Code 回官方订阅",
            None,
            Some(&redactor),
        )?;
        Ok(outcome.written)
    }

    /// Codex 中转站：备份官方 auth → 写 relay auth.json → 改 config.toml
    ///（含附加配置片段的深合并与上一项片段的按值深移除）。
    #[allow(clippy::too_many_arguments)]
    fn switch_codex_relay(
        &self,
        profile: &ProviderProfile,
        router: Option<&RouterEndpoint>,
        state: &mut ProviderState,
        previous_relay: Option<&PreviousRelay>,
        effective: &serde_json::Value,
        engine: &Engine,
        paths: &ClientPaths,
        warnings: &mut Vec<String>,
    ) -> Result<bool, ProviderError> {
        // Codex 2026-02 起 `wire_api = "chat"` 是反序列化硬错误（openai/codex#7782），
        // 写进去的不是「功能受限」而是整份 config.toml 拒绝加载、Codex 起不来。
        // 这里拦下比事后让用户对着 Codex 的报错猜是谁写的强。
        // 路由接管时由本地路由做 Responses ↔ Chat 转换，live 恒写 responses（ADR-022）。
        if profile.codex_wire_api != CodexWireApi::Responses && router.is_none() {
            return Err(ProviderError::InvalidOperation(
                "该供应商使用 Chat Completions / Anthropic 协议，Codex 无法直连；\
                 请在供应商页开启本地路由（由 MCPane 转换协议），或在编辑器里改为 Responses"
                    .into(),
            ));
        }
        let (base_url, key) = match router {
            Some(router) => (router.base_url.clone(), Secret::new(router.token.clone())),
            None => (
                require_base_url(profile)?.to_owned(),
                self.materialize_key(profile)?,
            ),
        };
        let slug = profile
            .codex_slug
            .clone()
            .ok_or_else(|| ProviderError::InvalidOperation("Codex 中转站缺少 slug".into()))?;

        // (a) 备份判定。
        let auth: &dyn AuthHandling = &CodexAdapter;
        let original_auth = read_optional(auth.auth_file(paths))?;
        match auth.classify(original_auth.as_deref()) {
            AuthKind::Official => {
                // 当前文件是官方登录，必然比旧备份新（用户可能重新 login 过）→ 覆盖备份。
                let content = original_auth.clone().unwrap_or_default();
                let reference = state
                    .codex_auth_backup
                    .clone()
                    .unwrap_or_else(SecretReference::generate);
                self.secrets.set(&reference, &Secret::new(content))?;
                state.codex_auth_backup = Some(reference);
                // 先持久化备份引用再写 auth.json：中途崩溃也不丢官方登录。
                self.store.save(&*state)?;
            }
            AuthKind::Relay => {} // relay→relay 再切：绝不覆盖已有官方备份。
            AuthKind::Unknown => {
                if state.codex_auth_backup.is_none() {
                    let content = original_auth.clone().unwrap_or_default();
                    let reference = SecretReference::generate();
                    self.secrets.set(&reference, &Secret::new(content))?;
                    state.codex_auth_backup = Some(reference);
                    self.store.save(&*state)?;
                }
                warnings.push("auth.json 格式未识别，已保守备份到系统凭据库".into());
            }
            AuthKind::Missing => {
                warnings.push("未检测到 Codex 登录，切回官方订阅时需要重新 codex login".into());
            }
        }

        // (b) 写 relay auth.json（独立原子写，禁走引擎快照）。
        write_auth_atomically(auth.auth_file(paths), &auth.relay_content(key.expose()))?;

        // (c) config.toml：指向我们的 model_providers 表，并清理其他 MCPane 表。
        let mut ops = vec![
            PatchOp::Set {
                path: vec!["model_providers".into(), slug.clone(), "name".into()],
                value: serde_json::json!(profile.name),
            },
            PatchOp::Set {
                path: vec!["model_providers".into(), slug.clone(), "base_url".into()],
                value: serde_json::json!(base_url),
            },
            PatchOp::Set {
                path: vec!["model_providers".into(), slug.clone(), "wire_api".into()],
                value: serde_json::json!(if router.is_some() {
                    CodexWireApi::Responses.as_str()
                } else {
                    profile.codex_wire_api.as_str()
                }),
            },
            // 把认证路由到 auth.json 的 OPENAI_API_KEY（对齐 cc-switch：
            // 自定义 provider 不带此字段时 Codex 不会读取 auth.json 凭据）。
            PatchOp::Set {
                path: vec![
                    "model_providers".into(),
                    slug.clone(),
                    "requires_openai_auth".into(),
                ],
                value: serde_json::json!(true),
            },
            PatchOp::Set {
                path: vec!["model_provider".into()],
                value: serde_json::json!(slug),
            },
            // 中转站不支持 OpenAI 服务端响应存储，必须让 Codex 每轮携带完整
            // 上下文（对齐 cc-switch：全部中转预设均写此键）。
            PatchOp::Set {
                path: vec!["disable_response_storage".into()],
                value: serde_json::json!(true),
            },
        ];
        // query_params（Azure OpenAI 的 api-version 等）：有值整表写入，
        // 无值清除残留（同 slug 复用时上一次的参数不残留）。
        if profile.codex_query_params.is_empty() {
            ops.push(PatchOp::Remove {
                path: vec![
                    "model_providers".into(),
                    slug.clone(),
                    "query_params".into(),
                ],
            });
        } else {
            ops.push(PatchOp::Set {
                path: vec![
                    "model_providers".into(),
                    slug.clone(),
                    "query_params".into(),
                ],
                value: serde_json::json!(profile.codex_query_params),
            });
        }
        for other in codex_relay_slugs(state, Some(&profile.id)) {
            ops.push(PatchOp::Remove {
                path: vec!["model_providers".into(), other],
            });
        }
        match &profile.model {
            Some(model) => ops.push(PatchOp::Set {
                path: vec!["model".into()],
                value: serde_json::json!(model),
            }),
            None => {
                // 只清我们上一个中转站写的 model，用户自设值不动。
                if previous_relay.is_some_and(|p| p.model.is_some()) {
                    ops.push(PatchOp::Remove {
                        path: vec!["model".into()],
                    });
                }
            }
        }
        // 1M 窗口按**目标** profile 写/删，不靠 PreviousRelay：同一 id 再 apply
        // 时 previous.model 会被置空，关掉 1M 必须真把键从 live 删掉。
        ops.extend(codex_context_window_ops(
            profile.codex_context_window_1m,
            profile.codex_auto_compact_limit,
        ));

        let loaded = engine.load(&paths.codex_config, ConfigFormat::Toml)?;
        // 附加配置片段：先按值匹配清上一项残留，再深合并写入有效片段。
        let new_leaves = fragment_leaves(effective);
        if let Some(previous) = previous_relay
            && let Ok(doc) = ConfigDocument::parse(&loaded.content, ConfigFormat::Toml)
        {
            ops.extend(fragment_removal_ops(&doc, &previous.fragment, &new_leaves));
        }
        for (path, value) in new_leaves {
            ops.push(PatchOp::Set { path, value });
        }
        let apply_result = engine.apply(
            &loaded,
            &PatchPlan::new(ops),
            &format!("切换 Codex 到中转站「{}」", profile.name),
            Some(&validate_codex_no_plaintext_key),
            None,
        );

        // (d) 失败补偿：config.toml 没写成 → 把 auth.json 恢复原样。
        // 恢复本身失败时合并两个错误——根因（config 写入失败）不能被恢复
        // 错误吞掉，且要说清当前落在什么状态。
        match apply_result {
            Ok(outcome) => {
                let _ = outcome;
                Ok(true)
            }
            Err(error) => {
                let restore = match &original_auth {
                    Some(content) => write_auth_atomically(&paths.codex_auth, content),
                    None => {
                        let _ = std::fs::remove_file(&paths.codex_auth);
                        Ok(())
                    }
                };
                match restore {
                    Ok(()) => Err(error.into()),
                    Err(restore_error) => Err(ProviderError::InvalidOperation(format!(
                        "config.toml 写入失败：{error}；且 auth.json 恢复失败：\
                         {restore_error}。当前 auth.json 是中转 Key、config 未指向中转，\
                         Codex 会表现为未登录，重试切换即可修复"
                    ))),
                }
            }
        }
    }

    /// Codex 官方订阅：恢复备份的 auth.json + 清理 config.toml，
    /// 并深合并官方订阅自己的附加配置片段（官方同样可编辑，对齐 cc-switch）。
    #[allow(clippy::too_many_arguments)]
    fn switch_codex_official(
        &self,
        state: &mut ProviderState,
        previous_relay: Option<&PreviousRelay>,
        effective: &serde_json::Value,
        engine: &Engine,
        paths: &ClientPaths,
        warnings: &mut Vec<String>,
    ) -> Result<bool, ProviderError> {
        // (a) 恢复 auth.json。备份不可用（引用悬空/内容损坏）时降级为无备份
        // 路径并给出提示，绝不能让用户永远切不回官方。
        // Keychain 备份的删除推迟到 config.toml 也写成之后（事务尾部）：
        // 半途失败时"备份仍在"是最后的安全网。
        let original_auth = read_optional(&paths.codex_auth)?;
        let mut auth_written = false;
        let mut restored = false;
        let mut restored_backup: Option<SecretReference> = None;
        if let Some(reference) = state.codex_auth_backup.clone() {
            match self.load_valid_auth_backup(&reference) {
                Ok(content) => {
                    write_auth_atomically(&paths.codex_auth, &content)?;
                    auth_written = true;
                    restored = true;
                    restored_backup = Some(reference);
                }
                Err(reason) => {
                    // 引用悬空或内容损坏：备份已不可用，清掉引用降级处理。
                    let _ = self.secrets.delete(&reference);
                    state.codex_auth_backup = None;
                    self.store.save(&*state)?;
                    warnings.push(format!("官方登录备份不可用（{reason}），已丢弃"));
                }
            }
        }
        if !restored {
            let auth: &dyn AuthHandling = &CodexAdapter;
            match auth.classify(original_auth.as_deref()) {
                AuthKind::Official => {} // 本来就是官方登录，不动。
                AuthKind::Relay => {
                    // 内容是我们写的中转 Key，留着会让 Codex 用错凭据。
                    std::fs::remove_file(&paths.codex_auth)?;
                    auth_written = true;
                    warnings.push("无官方登录备份，已清除中转凭据，请运行 codex login".into());
                }
                AuthKind::Unknown => {
                    warnings.push("auth.json 内容未识别，未做改动".into());
                }
                AuthKind::Missing => {
                    warnings.push("未检测到 Codex 登录，请运行 codex login".into());
                }
            }
        }

        // (b) config.toml：移除 model_provider 与全部 MCPane 表。
        let mut ops = vec![PatchOp::Remove {
            path: vec!["model_provider".into()],
        }];
        for slug in codex_relay_slugs(state, None) {
            ops.push(PatchOp::Remove {
                path: vec!["model_providers".into(), slug],
            });
        }
        if let Some(previous) = previous_relay {
            // 只清我们上一个中转站写入的顶层键，用户自设值不动。
            ops.push(PatchOp::Remove {
                path: vec!["disable_response_storage".into()],
            });
            if previous.model.is_some() {
                ops.push(PatchOp::Remove {
                    path: vec!["model".into()],
                });
            }
        }
        // 官方订阅不该留下中转站的 1M 窗口；始终删除，不依赖 previous_relay。
        ops.extend(codex_context_window_ops(false, None));
        let loaded = engine.load(&paths.codex_config, ConfigFormat::Toml)?;
        // 附加配置片段：清上一项残留 + 写官方订阅自己的有效片段。
        let new_leaves = fragment_leaves(effective);
        if let Some(previous) = previous_relay
            && let Ok(doc) = ConfigDocument::parse(&loaded.content, ConfigFormat::Toml)
        {
            ops.extend(fragment_removal_ops(&doc, &previous.fragment, &new_leaves));
        }
        for (path, value) in new_leaves {
            ops.push(PatchOp::Set { path, value });
        }
        let apply_result = engine.apply(
            &loaded,
            &PatchPlan::new(ops),
            "切换 Codex 回官方订阅",
            None,
            None,
        );

        // (c) 失败补偿（与 relay 臂对称）：config.toml 没写成 → auth.json 回滚
        // 到切换前内容。不回滚会留下"官方 OAuth token + config 仍指向中转"的
        // 半态——此时运行 Codex 会把官方 token 发给中转站。
        let outcome = match apply_result {
            Ok(outcome) => outcome,
            Err(error) => {
                if auth_written {
                    let restore = match &original_auth {
                        Some(content) => write_auth_atomically(&paths.codex_auth, content),
                        None => {
                            let _ = std::fs::remove_file(&paths.codex_auth);
                            Ok(())
                        }
                    };
                    if let Err(restore_error) = restore {
                        return Err(ProviderError::InvalidOperation(format!(
                            "config.toml 写入失败：{error}；且 auth.json 回滚失败：\
                             {restore_error}。官方登录备份仍在系统凭据库，重试切换即可恢复"
                        )));
                    }
                }
                return Err(error.into());
            }
        };

        // (d) 事务尾部才销毁备份：auth.json 与 config.toml 都已落定。
        // 删除失败不影响切换结果（备份内容与刚恢复的官方登录相同，留着无害，
        // 下次切中转站会基于届时最新的官方登录覆盖它），降级为提示。
        if let Some(reference) = restored_backup {
            match self.secrets.delete(&reference) {
                Ok(()) => {
                    state.codex_auth_backup = None;
                    self.store.save(&*state)?;
                }
                Err(error) => {
                    warnings.push(format!("清理官方登录备份失败（已忽略）：{error}"));
                }
            }
        }
        Ok(auth_written || outcome.written)
    }

    /// Gemini 中转站：`.env` 写受管三键（base_url/key/model），
    /// `settings.json` 写 auth 类型 + 附加配置片段。两文件无事务，
    /// settings 失败时把 `.env` 恢复原样（对齐 Codex 臂的补偿策略）。
    #[allow(clippy::too_many_arguments)]
    fn switch_gemini_relay(
        &self,
        profile: &ProviderProfile,
        router: Option<&RouterEndpoint>,
        previous_relay: Option<&PreviousRelay>,
        effective: &serde_json::Value,
        engine: &Engine,
        paths: &ClientPaths,
        warnings: &mut Vec<String>,
    ) -> Result<bool, ProviderError> {
        let (base_url, token) = self.relay_endpoint(profile, router)?;

        // (a) .env：受管键写入。原文本留作 settings 失败时的补偿恢复源。
        let env_loaded = engine.load(&paths.gemini_env, ConfigFormat::Env)?;
        let original_env = (!env_loaded.missing).then(|| env_loaded.content.clone());
        let mut pairs: Vec<(&str, &str)> = vec![
            ("GOOGLE_GEMINI_BASE_URL", base_url.as_str()),
            ("GEMINI_API_KEY", token.as_str()),
        ];
        if let Some(model) = &profile.model {
            pairs.push(("GEMINI_MODEL", model));
        }
        let mut env_plan = mcpane_adapter_gemini::provider_env_patch(&pairs);
        if profile.model.is_none() {
            env_plan.ops.push(PatchOp::Remove {
                path: vec!["GEMINI_MODEL".into()],
            });
        }
        // 快照存写前内容：旧 .env 里的上一个 Key 与即将写入的新 Key 都要脱敏。
        let redactions = gemini_snapshot_redactions(&env_loaded.content, Some(token.as_str()));
        let redactor = move |content: &str| redact_all(content, &redactions);
        let env_outcome = engine.apply(
            &env_loaded,
            &env_plan,
            &format!("切换 Gemini CLI 到中转站「{}」（.env）", profile.name),
            None,
            Some(&redactor),
        )?;

        // (b) settings.json：auth 类型 + 片段清理/合并。
        let settings_result = self.apply_gemini_settings(
            mcpane_adapter_gemini::AUTH_TYPE_API_KEY,
            previous_relay,
            effective,
            engine,
            paths,
            &format!("切换 Gemini CLI 到中转站「{}」", profile.name),
        );
        match settings_result {
            Ok(settings_written) => Ok(env_outcome.written || settings_written),
            Err(error) => {
                // 补偿：settings 没写成 → .env 恢复原样（原不存在则删除）。
                let restore = match &original_env {
                    Some(content) => write_auth_atomically(&paths.gemini_env, content),
                    None => {
                        let _ = std::fs::remove_file(&paths.gemini_env);
                        Ok(())
                    }
                };
                if restore.is_err() {
                    warnings.push("settings.json 写入失败且 .env 恢复失败，请在历史页回滚".into());
                }
                Err(error)
            }
        }
    }

    /// Gemini 官方登录：`.env` 移除受管键，`settings.json` 把 auth 类型
    /// 设回 oauth-personal（OAuth 凭据由 CLI 自己管理，无需恢复）。
    /// OpenCode「启用」：把这条供应商写进 `provider.<id>` 子树。
    ///
    /// additive 语义——不动配置里其他任何 provider，也不存在"切走上一个"。
    /// 想让它从 OpenCode 消失走 [`Self::remove_from_live`]，那是显式动作。
    fn enable_additive_provider(
        &self,
        profile: &ProviderProfile,
        previous_relay: Option<&PreviousRelay>,
        effective: &serde_json::Value,
        engine: &Engine,
        paths: &ClientPaths,
        warnings: &mut Vec<String>,
    ) -> Result<bool, ProviderError> {
        let base_url = require_base_url(profile)?.to_owned();
        let key = self.materialize_key(profile)?;
        let token = key.expose().to_owned();
        let models = additive_models(profile);
        if models.is_empty() {
            return Err(ProviderError::InvalidOperation(
                "additive 客户端的供应商必须至少填一个模型名".into(),
            ));
        }

        let app = profile.app;
        let entry_id = additive_provider_id(profile);
        let (live_path, live_format) = adapter_for(app).live_config(paths);
        let loaded = engine.load(live_path, live_format)?;
        // 上游协议：两家字段名不同、值不同，同一个维度映射过去（默认 OpenAI 兼容）。
        let anthropic = app.supports_upstream_api()
            && profile.upstream_api == Some(crate::domain::UpstreamApi::Anthropic);
        let mut plan = match app {
            ClientApp::Hermes => {
                // 数组容器：先读出现有的 custom_providers，整块改写回去。
                let live_doc = ConfigDocument::parse(&loaded.content, live_format).ok();
                // v12+ 字典里已有同名键：数组条目会遮蔽它（Hermes 与呈现层
                // 都以数组为准）。这是上游确认的合法 overlay 语义，不阻断，
                // 但要让用户知道 Web UI 里那条从此不生效。
                let shadows_dict = live_doc
                    .as_ref()
                    .and_then(|doc| doc.get(&[mcpane_adapter_hermes::PROVIDERS_DICT_KEY]))
                    .and_then(|dict| dict.as_object().map(|map| map.contains_key(&entry_id)))
                    .unwrap_or(false);
                if shadows_dict {
                    warnings.push(format!(
                        "「{entry_id}」与 Hermes 自身管理的同名供应商重名，启用后将遮蔽 Web UI 里的那条"
                    ));
                }
                let existing = live_doc
                    .and_then(|doc| doc.get(&[mcpane_adapter_hermes::CUSTOM_PROVIDERS_KEY]));
                mcpane_adapter_hermes::upsert_patch(
                    existing.as_ref(),
                    &mcpane_adapter_hermes::ProviderEntry {
                        name: &entry_id,
                        base_url: &base_url,
                        api_key: &token,
                        api_mode: if anthropic {
                            mcpane_adapter_hermes::ANTHROPIC_API_MODE
                        } else {
                            mcpane_adapter_hermes::DEFAULT_API_MODE
                        },
                        models: &models,
                        context_length: profile.context_length,
                    },
                )
            }
            ClientApp::OpenClaw => mcpane_adapter_openclaw::provider_entry_patch(
                &mcpane_adapter_openclaw::ProviderEntry {
                    id: &entry_id,
                    base_url: &base_url,
                    api_key: &token,
                    api: if anthropic {
                        mcpane_adapter_openclaw::ANTHROPIC_API
                    } else {
                        mcpane_adapter_openclaw::DEFAULT_API
                    },
                    models: &models,
                    context_window: profile.context_length,
                    headers: &profile.headers,
                },
            ),
            // OpenCode 及后续同形态的 additive 客户端。
            _ => mcpane_adapter_opencode::provider_entry_patch(
                &mcpane_adapter_opencode::ProviderEntry {
                    id: &entry_id,
                    name: &profile.name,
                    npm: if anthropic {
                        mcpane_adapter_opencode::ANTHROPIC_NPM
                    } else {
                        mcpane_adapter_opencode::DEFAULT_NPM
                    },
                    base_url: &base_url,
                    api_key: &token,
                    models: &models,
                    headers: &profile.headers,
                },
            ),
        };
        // OpenClaw：只写 provider 表等于"这家可用"，还得把模型注册进目录、
        // 并在默认模型未设时指过去，否则用户启用完 OpenClaw 毫无变化。
        if app == ClientApp::OpenClaw {
            let live = ConfigDocument::parse(&loaded.content, live_format).ok();
            let existing_primary = live
                .as_ref()
                .and_then(|doc| doc.get(&mcpane_adapter_openclaw::DEFAULT_MODEL_PRIMARY_PATH))
                .and_then(|v| v.as_str().map(str::to_owned));
            let existing_catalog: Vec<String> = live
                .as_ref()
                .and_then(|doc| doc.get(&mcpane_adapter_openclaw::MODEL_CATALOG_PATH))
                .and_then(|v| v.as_object().map(|map| map.keys().cloned().collect()))
                .unwrap_or_default();
            plan.ops.extend(
                mcpane_adapter_openclaw::default_model_patch(
                    &entry_id,
                    &models,
                    existing_primary.as_deref(),
                    &existing_catalog,
                )
                .ops,
            );
        }
        let new_leaves = fragment_leaves(effective);
        if let Some(previous) = previous_relay
            && let Ok(doc) = ConfigDocument::parse(&loaded.content, live_format)
        {
            plan.ops
                .extend(fragment_removal_ops(&doc, &previous.fragment, &new_leaves));
        }
        for (path, value) in new_leaves {
            plan.ops.push(PatchOp::Set { path, value });
        }
        // Key 明文落在客户端配置里：新旧 Key 都要在写前快照中脱敏。
        let redactions = additive_snapshot_redactions(app, &loaded.content, Some(token.as_str()));
        let redactor = move |content: &str| redact_all(content, &redactions);
        let outcome = engine.apply(
            &loaded,
            &plan,
            &format!(
                "启用 {} 供应商「{}」",
                adapter_for(app).display_name(),
                profile.name
            ),
            None,
            Some(&redactor),
        )?;
        Ok(outcome.written)
    }

    /// 从 live 配置移除一条 additive 供应商（供应商记录仍留在列表里，可再启用）。
    /// 对齐 cc-switch 的 `removeFromLiveConfig`：这是"从配置移除"，不是"删除"。
    pub fn remove_from_live(
        &self,
        id: &str,
        engine: &Engine,
        paths: &ClientPaths,
    ) -> Result<bool, ProviderError> {
        let mut state = self.store.load()?;
        let profile = state
            .providers
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| ProviderError::NotFound(id.to_owned()))?;
        if !profile.app.is_additive() {
            return Err(ProviderError::InvalidOperation(
                "只有 additive 客户端支持「从配置移除」，其他客户端请切换到别的供应商".into(),
            ));
        }
        let app = profile.app;
        let entry_id = additive_provider_id(&profile);
        let (live_path, live_format) = adapter_for(app).live_config(paths);
        let loaded = engine.load(live_path, live_format)?;
        let plan = match app {
            ClientApp::Hermes => {
                let existing = ConfigDocument::parse(&loaded.content, live_format)
                    .ok()
                    .and_then(|doc| doc.get(&[mcpane_adapter_hermes::CUSTOM_PROVIDERS_KEY]));
                mcpane_adapter_hermes::remove_patch(existing.as_ref(), &entry_id)
            }
            ClientApp::OpenClaw => {
                // 连带清模型目录条目与指向本家的默认模型指针，别留死引用；
                // 目录/指针空了不留 `{}`/`{"model":{}}` 空壳（P0-3 顺带清理）。
                let live_doc = ConfigDocument::parse(&loaded.content, live_format).ok();
                let existing_primary = live_doc
                    .as_ref()
                    .and_then(|doc| doc.get(&mcpane_adapter_openclaw::DEFAULT_MODEL_PRIMARY_PATH))
                    .and_then(|v| v.as_str().map(str::to_owned));
                let existing_catalog: Vec<String> = live_doc
                    .as_ref()
                    .and_then(|doc| doc.get(&mcpane_adapter_openclaw::MODEL_CATALOG_PATH))
                    .and_then(|v| v.as_object().map(|map| map.keys().cloned().collect()))
                    .unwrap_or_default();
                let mut plan = mcpane_adapter_openclaw::remove_provider_patch(
                    &entry_id,
                    &additive_models(&profile),
                    existing_primary.as_deref(),
                    &existing_catalog,
                );
                // `agents.defaults.model` 目前只装得下 `primary` 一个键
                //（没有其他已知子字段）；指针是我们的且清掉后这张表会变成
                // `{}`，一并删掉表本身，避免留一个没有实际内容的空对象。
                let prefix = format!("{entry_id}/");
                let model_pointer_table_now_empty = existing_primary
                    .as_deref()
                    .is_some_and(|p| p.starts_with(&prefix))
                    && live_doc
                        .as_ref()
                        .and_then(|doc| {
                            doc.get(&mcpane_adapter_openclaw::DEFAULT_MODEL_PRIMARY_PATH[..3])
                        })
                        .and_then(|v| v.as_object().map(|obj| obj.len()))
                        == Some(1);
                if model_pointer_table_now_empty {
                    plan.ops.push(PatchOp::Remove {
                        path: mcpane_adapter_openclaw::DEFAULT_MODEL_PRIMARY_PATH[..3]
                            .iter()
                            .map(|s| (*s).to_owned())
                            .collect(),
                    });
                }
                plan
            }
            _ => mcpane_adapter_opencode::remove_provider_patch(&entry_id),
        };
        let redactions = additive_snapshot_redactions(app, &loaded.content, None);
        let redactor = move |content: &str| redact_all(content, &redactions);
        let outcome = engine.apply(
            &loaded,
            &plan,
            &format!(
                "从 {} 配置移除「{}」",
                adapter_for(app).display_name(),
                profile.name
            ),
            None,
            Some(&redactor),
        )?;
        // 移除的正是"当前项"时清掉指针，避免 UI 显示一个已不在配置里的当前项。
        if state.current.get(&profile.app).is_some_and(|c| c == id) {
            state.current.remove(&profile.app);
            self.store.save(&state)?;
        }
        Ok(outcome.written)
    }

    /// live 配置里已启用的供应商 id（additive 客户端的"启用态"来源）。
    pub fn live_provider_ids(
        &self,
        app: ClientApp,
        paths: &ClientPaths,
    ) -> Result<Vec<String>, ProviderError> {
        if !app.is_additive() {
            return Ok(Vec::new());
        }
        let (live_path, live_format) = adapter_for(app).live_config(paths);
        let content = std::fs::read_to_string(live_path).unwrap_or_default();
        let live = match app {
            ClientApp::Hermes => ConfigDocument::parse(&content, live_format)
                .ok()
                .map(|doc| {
                    mcpane_adapter_hermes::provider_names(
                        doc.get(&[mcpane_adapter_hermes::CUSTOM_PROVIDERS_KEY])
                            .as_ref(),
                    )
                })
                .unwrap_or_default(),
            ClientApp::OpenClaw => ConfigDocument::parse(&content, live_format)
                .ok()
                .and_then(|doc| doc.get(&mcpane_adapter_openclaw::PROVIDERS_PATH))
                .map(|providers| mcpane_adapter_openclaw::provider_ids(Some(&providers)))
                .unwrap_or_default(),
            _ => ConfigDocument::parse(&content, live_format)
                .ok()
                .and_then(|doc| doc.get(&[mcpane_adapter_opencode::PROVIDER_TABLE_KEY]))
                .map(|providers| mcpane_adapter_opencode::provider_ids(Some(&providers)))
                .unwrap_or_default(),
        };
        // 回映射到我们的 profile id：live 里的键是 slug，UI 认的是 profile id。
        let state = self.store.load()?;
        Ok(state
            .providers
            .iter()
            .filter(|p| p.app == app && live.contains(&additive_provider_id(p)))
            .map(|p| p.id.clone())
            .collect())
    }

    /// Hermes v12+ `providers:` 字典里的外部供应商（Hermes Web UI 托管）。
    ///
    /// 只读呈现用：这些条目没有 profile id、不进 store，前端以"Hermes 托管"
    /// 分组展示且不给任何操作入口。同名遮蔽（数组赢）在 adapter 层已处理；
    /// 文件缺失/解析失败容错为空表——与 [`Self::live_provider_ids`] 同口径。
    pub fn hermes_external_providers(
        &self,
        paths: &ClientPaths,
    ) -> Result<Vec<mcpane_adapter_hermes::DictProvider>, ProviderError> {
        let content = std::fs::read_to_string(&paths.hermes_config).unwrap_or_default();
        Ok(ConfigDocument::parse(&content, ConfigFormat::Yaml)
            .ok()
            .map(|doc| {
                mcpane_adapter_hermes::dict_provider_entries(
                    doc.get(&[mcpane_adapter_hermes::PROVIDERS_DICT_KEY])
                        .as_ref(),
                    doc.get(&[mcpane_adapter_hermes::CUSTOM_PROVIDERS_KEY])
                        .as_ref(),
                )
            })
            .unwrap_or_default())
    }

    /// Grok 切中转站：写 `[model."mcpane-<slug>"]` 表 + 把 `[models].default`
    /// 指过去，并清掉上一个中转站留下的表与片段。
    ///
    /// Key 明文落在 config.toml 里（Grok CLI 没有独立凭据文件），所以写前快照
    /// 必须脱敏——新旧两个 Key 都要，否则历史里会留下可读的明文。
    fn switch_grok_relay(
        &self,
        profile: &ProviderProfile,
        router: Option<&RouterEndpoint>,
        previous_relay: Option<&PreviousRelay>,
        effective: &serde_json::Value,
        engine: &Engine,
        paths: &ClientPaths,
    ) -> Result<bool, ProviderError> {
        let (base_url, token) = self.relay_endpoint(profile, router)?;
        let slug = mcpane_adapter_grok::table_slug(&profile.name);
        // Grok CLI 要求模型表必须有 model 字段；用户没填就退到 xAI 当前主力模型，
        // 空着会让 CLI 整份配置校验失败。
        let model = profile
            .model
            .clone()
            .unwrap_or_else(|| "grok-4.5".to_owned());

        let loaded = engine.load(&paths.grok_config, ConfigFormat::Toml)?;
        let mut plan =
            mcpane_adapter_grok::model_table_patch(&mcpane_adapter_grok::GrokModelTable {
                slug: &slug,
                name: &profile.name,
                model: &model,
                base_url: &base_url,
                api_key: Some(token.as_str()),
                env_key: None,
                api_backend: mcpane_adapter_grok::DEFAULT_API_BACKEND,
                context_window: mcpane_adapter_grok::DEFAULT_CONTEXT_WINDOW,
            });
        let new_leaves = fragment_leaves(effective);
        if let Some(previous) = previous_relay
            && let Ok(doc) = ConfigDocument::parse(&loaded.content, ConfigFormat::Toml)
        {
            plan.ops
                .extend(fragment_removal_ops(&doc, &previous.fragment, &new_leaves));
        }
        // 上一个中转站的表：换供应商时表名不同，留着就是死配置。
        for stale in stale_grok_slugs(&loaded.content, &slug) {
            plan.ops.push(PatchOp::Remove {
                path: vec![mcpane_adapter_grok::MODEL_TABLE_KEY.to_owned(), stale],
            });
        }
        for (path, value) in new_leaves {
            plan.ops.push(PatchOp::Set { path, value });
        }
        let redactions = grok_snapshot_redactions(&loaded.content, Some(token.as_str()));
        let redactor = move |content: &str| redact_all(content, &redactions);
        let outcome = engine.apply(
            &loaded,
            &plan,
            &format!("切换 Grok CLI 到中转站「{}」", profile.name),
            None,
            Some(&redactor),
        )?;
        Ok(outcome.written)
    }

    /// Grok 切回官方：移除我们写的全部模型表；`[models].default` 只有在它确实
    /// 指向我们的表时才碰——用户自己设的默认模型不能被顺手抹掉，碰的时候也
    /// 优先**还原**成 MCPane 接管前的原值而不是直接清空（P0-3：早期实现无
    /// 条件删除指针，永久丢失用户在装 MCPane 之前就设好的默认模型；参见
    /// [`find_prior_grok_default`]）。官方凭据（xAI OAuth）由 Grok CLI 自管，
    /// 无需恢复任何备份。
    fn switch_grok_official(
        &self,
        _state: &ProviderState,
        previous_relay: Option<&PreviousRelay>,
        effective: &serde_json::Value,
        engine: &Engine,
        paths: &ClientPaths,
    ) -> Result<bool, ProviderError> {
        let loaded = engine.load(&paths.grok_config, ConfigFormat::Toml)?;
        let doc = ConfigDocument::parse(&loaded.content, ConfigFormat::Toml).ok();
        let default_is_ours = doc
            .as_ref()
            .and_then(|d| d.get(&["models", "default"]))
            .and_then(|v| v.as_str().map(str::to_owned))
            .is_some_and(|slug| slug.starts_with(mcpane_adapter_grok::SLUG_PREFIX));

        let mut plan = PatchPlan::new(Vec::new());
        for slug in stale_grok_slugs(&loaded.content, "") {
            plan.ops.push(PatchOp::Remove {
                path: vec![mcpane_adapter_grok::MODEL_TABLE_KEY.to_owned(), slug],
            });
        }
        if default_is_ours {
            let default_path = || {
                mcpane_adapter_grok::MODELS_DEFAULT_PATH
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect()
            };
            // 只在原来指向的表还在（没被用户自己删掉）时才还原，否则指针会
            // 变成指向空气的死引用，比清空还糟。
            let restore_to =
                find_prior_grok_default(engine, &paths.grok_config).filter(|previous| {
                    doc.as_ref().is_some_and(|d| {
                        d.get(&[mcpane_adapter_grok::MODEL_TABLE_KEY, previous])
                            .is_some()
                    })
                });
            plan.ops.push(match restore_to {
                Some(previous) => PatchOp::Set {
                    path: default_path(),
                    value: serde_json::Value::String(previous),
                },
                None => PatchOp::Remove {
                    path: default_path(),
                },
            });
        }
        let new_leaves = fragment_leaves(effective);
        if let Some(previous) = previous_relay
            && let Some(doc) = doc.as_ref()
        {
            plan.ops
                .extend(fragment_removal_ops(doc, &previous.fragment, &new_leaves));
        }
        for (path, value) in new_leaves {
            plan.ops.push(PatchOp::Set { path, value });
        }
        // 切走的是中转形态，旧表里的 Key 仍要脱敏后才能进快照。
        let redactions = grok_snapshot_redactions(&loaded.content, None);
        let redactor = move |content: &str| redact_all(content, &redactions);
        let outcome = engine.apply(
            &loaded,
            &plan,
            "切换 Grok CLI 回官方登录",
            None,
            Some(&redactor),
        )?;
        Ok(outcome.written)
    }

    fn switch_gemini_official(
        &self,
        previous_relay: Option<&PreviousRelay>,
        effective: &serde_json::Value,
        engine: &Engine,
        paths: &ClientPaths,
        warnings: &mut Vec<String>,
    ) -> Result<bool, ProviderError> {
        let env_loaded = engine.load(&paths.gemini_env, ConfigFormat::Env)?;
        let original_env = (!env_loaded.missing).then(|| env_loaded.content.clone());
        let env_plan = mcpane_adapter_gemini::remove_provider_env_patch(
            &mcpane_adapter_gemini::PROVIDER_ENV_KEYS,
        );
        let redactions = gemini_snapshot_redactions(&env_loaded.content, None);
        let redactor = move |content: &str| redact_all(content, &redactions);
        let env_outcome = engine.apply(
            &env_loaded,
            &env_plan,
            "切换 Gemini CLI 回官方登录（.env）",
            None,
            Some(&redactor),
        )?;

        let settings_result = self.apply_gemini_settings(
            mcpane_adapter_gemini::AUTH_TYPE_OAUTH,
            previous_relay,
            effective,
            engine,
            paths,
            "切换 Gemini CLI 回官方登录",
        );
        match settings_result {
            Ok(settings_written) => Ok(env_outcome.written || settings_written),
            Err(error) => {
                let restore = match &original_env {
                    Some(content) => write_auth_atomically(&paths.gemini_env, content),
                    None => {
                        let _ = std::fs::remove_file(&paths.gemini_env);
                        Ok(())
                    }
                };
                if restore.is_err() {
                    warnings.push("settings.json 写入失败且 .env 恢复失败，请在历史页回滚".into());
                }
                Err(error)
            }
        }
    }

    /// Gemini settings.json 的公共写入：auth 类型 + 上一片段清理 + 新片段合并。
    fn apply_gemini_settings(
        &self,
        auth_type: &str,
        previous_relay: Option<&PreviousRelay>,
        effective: &serde_json::Value,
        engine: &Engine,
        paths: &ClientPaths,
        description: &str,
    ) -> Result<bool, ProviderError> {
        let loaded = engine.load(&paths.gemini_settings, ConfigFormat::Json)?;
        let mut plan = mcpane_adapter_gemini::auth_type_patch(auth_type);
        let new_leaves = fragment_leaves(effective);
        if let Some(previous) = previous_relay
            && let Ok(doc) = ConfigDocument::parse(&loaded.content, ConfigFormat::Json)
        {
            plan.ops
                .extend(fragment_removal_ops(&doc, &previous.fragment, &new_leaves));
        }
        for (path, value) in new_leaves {
            plan.ops.push(PatchOp::Set { path, value });
        }
        let outcome = engine.apply(&loaded, &plan, description, None, None)?;
        Ok(outcome.written)
    }

    /// 首次接管某 app 前，把 live 配置里既有的第三方（中转）形态收编为一个
    /// relay Profile：Key 存入系统凭据库，base_url/model 等如实带入。
    /// 返回导入的 Profile 名称；无可收编内容返回 None。
    ///
    /// live 解析失败按"无可收编"处理——此时切换臂随后也会因同一解析错误
    /// 中止，不会发生覆盖。已有同 app 同 base_url 的 Profile 时跳过，
    /// 避免与用户手动录入的条目重复。
    fn import_live_as_provider(
        &self,
        app: ClientApp,
        state: &mut ProviderState,
        engine: &Engine,
        paths: &ClientPaths,
    ) -> Result<Option<(String, String)>, ProviderError> {
        const IMPORTED_NAME: &str = "既有配置（自动导入）";
        let mut profile = ProviderProfile::new_relay(app, IMPORTED_NAME);
        let live_key: Option<String> = match app {
            ClientApp::Claude => {
                let loaded = engine.load(&paths.claude_settings, ConfigFormat::Json)?;
                let Ok(doc) = ConfigDocument::parse(&loaded.content, ConfigFormat::Json) else {
                    return Ok(None);
                };
                crate::reverse::fill_from_claude(&mut profile, &doc)
            }
            ClientApp::Codex => {
                // 只有 auth.json 明确是中转形态（OPENAI_API_KEY）才收编；
                // 官方登录走 relay 臂的备份路径，Unknown 走保守备份路径。
                let auth: &dyn AuthHandling = &CodexAdapter;
                let auth_content = read_optional(&paths.codex_auth)?;
                if auth.classify(auth_content.as_deref()) != AuthKind::Relay {
                    return Ok(None);
                }
                let key = auth_content
                    .as_deref()
                    .and_then(|c| serde_json::from_str::<serde_json::Value>(c).ok())
                    .and_then(|v| v["OPENAI_API_KEY"].as_str().map(str::to_owned));
                // config.toml 侧尽力而为：live 指向的 provider 表若在（其他工具
                // 写的表名也认），带走 base_url/wire_api/model。
                let loaded = engine.load(&paths.codex_config, ConfigFormat::Toml)?;
                if let Ok(doc) = ConfigDocument::parse(&loaded.content, ConfigFormat::Toml) {
                    crate::reverse::fill_from_codex_config(&mut profile, &doc);
                }
                key
            }
            ClientApp::Gemini => {
                let env_loaded = engine.load(&paths.gemini_env, ConfigFormat::Env)?;
                let Ok(doc) = ConfigDocument::parse(&env_loaded.content, ConfigFormat::Env) else {
                    return Ok(None);
                };
                crate::reverse::fill_from_gemini_env(&mut profile, &doc)
            }
            ClientApp::OpenCode | ClientApp::OpenClaw | ClientApp::Hermes => {
                // additive：live 里可能有多条第三方 provider，逐条收编没有意义
                //（用户要的是"我列表里那几条"，不是"配置里所有的"）。首启不收编，
                // 由用户显式添加/启用——这与 cc-switch 把 OpenCode 排除在
                // import_default_config 之外的判断一致。
                return Ok(None);
            }
            ClientApp::Grok => {
                // 只收编"默认模型指向某张表"的形态：表里带 base_url + api_key
                // 就是第三方中转，官方登录态没有这些。
                let loaded = engine.load(&paths.grok_config, ConfigFormat::Toml)?;
                let Ok(doc) = ConfigDocument::parse(&loaded.content, ConfigFormat::Toml) else {
                    return Ok(None);
                };
                let Some(default_slug) = doc
                    .get(&["models", "default"])
                    .and_then(|v| v.as_str().map(str::to_owned))
                else {
                    return Ok(None);
                };
                let field = |name: &str| {
                    doc.get(&[mcpane_adapter_grok::MODEL_TABLE_KEY, &default_slug, name])
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .filter(|v| !v.is_empty())
                };
                let Some(base_url) = field("base_url") else {
                    return Ok(None);
                };
                profile.base_url = Some(base_url);
                profile.model = field("model");
                field("api_key")
            }
            ClientApp::ClaudeDesktop => {
                // 只收编生效中（threep "3p" + appliedId）的三方 profile；
                // 指向 loopback 的（本机代理网关，key 是网关 token 而非上游凭据）
                // 由 read_active_import 内部排除。
                let Some(imported) = claude_desktop::read_active_import(&paths.claude_desktop)
                else {
                    return Ok(None);
                };
                profile.base_url = Some(imported.base_url);
                profile.desktop_mode = DesktopMode::Direct;
                profile.desktop_routes = imported.routes;
                Some(imported.api_key)
            }
        };

        // 既无凭据也无 base_url：live 是官方形态或空配置，无需收编。
        if live_key.is_none() && profile.base_url.is_none() {
            return Ok(None);
        }
        // 与已有条目（用户手动录入过同一家中转）去重。
        if profile.base_url.is_some()
            && state
                .providers
                .iter()
                .any(|p| p.app == app && p.base_url == profile.base_url)
        {
            return Ok(None);
        }
        if let Some(key) = live_key {
            self.backfill_key(&mut profile, &key)?;
        }
        if app == ClientApp::Codex {
            let taken: Vec<String> = state
                .providers
                .iter()
                .filter_map(|p| p.codex_slug.clone())
                .collect();
            profile.codex_slug = Some(generate_codex_slug(&profile.name, &taken));
        }
        let name = profile.name.clone();
        let id = profile.id.clone();
        state.providers.push(profile);
        Ok(Some((id, name)))
    }

    /// 切走前把 live 配置回填到当前中转站（保留用户对配置文件的手改）。
    fn backfill(
        &self,
        app: ClientApp,
        profile: &mut ProviderProfile,
        engine: &Engine,
        paths: &ClientPaths,
    ) -> Result<(), ProviderError> {
        match app {
            ClientApp::OpenCode | ClientApp::OpenClaw | ClientApp::Hermes => {
                // additive 无"切走"动作（移除是显式的），不需要回填。
            }
            ClientApp::Grok => {
                let loaded = engine.load(&paths.grok_config, ConfigFormat::Toml)?;
                let doc = ConfigDocument::parse(&loaded.content, ConfigFormat::Toml)?;
                // 只有 live 仍指向该供应商自己的表才回填（表名由名称派生）。
                let slug = mcpane_adapter_grok::table_slug(&profile.name);
                let live_default = doc
                    .get(&["models", "default"])
                    .and_then(|v| v.as_str().map(str::to_owned));
                if live_default.as_deref() != Some(slug.as_str()) {
                    return Ok(());
                }
                let field = |name: &str| {
                    doc.get(&[mcpane_adapter_grok::MODEL_TABLE_KEY, &slug, name])
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .filter(|v| !v.is_empty())
                };
                // 路由接管中：live 是 loopback 地址 + 网关 token，不能倒灌。
                let routed =
                    field("base_url").is_some_and(|url| crate::domain::is_router_url(&url));
                if !routed && let Some(base_url) = field("base_url") {
                    profile.base_url = Some(base_url);
                }
                profile.model = field("model");
                backfill_fragment(&mut profile.extra_settings, &doc);
                if !routed && let Some(live_key) = field("api_key") {
                    self.backfill_key(profile, &live_key)?;
                }
            }
            ClientApp::Claude => {
                let loaded = engine.load(&paths.claude_settings, ConfigFormat::Json)?;
                let doc = ConfigDocument::parse(&loaded.content, ConfigFormat::Json)?;
                // 只有 live 仍是中转形态（有 base_url）才回填。
                let Some(live_base) = doc
                    .get(&["env", "ANTHROPIC_BASE_URL"])
                    .and_then(|v| v.as_str().map(str::to_owned))
                else {
                    return Ok(());
                };
                // 路由接管中：live 是 loopback 地址 + 网关 token，不能倒灌。
                let routed = crate::domain::is_router_url(&live_base);
                if !routed {
                    profile.base_url = Some(live_base);
                }
                let env_str = |key: &str| {
                    doc.get(&["env", key])
                        .and_then(|v| v.as_str().map(str::to_owned))
                };
                profile.model = env_str("ANTHROPIC_MODEL");
                profile.model_haiku = env_str("ANTHROPIC_DEFAULT_HAIKU_MODEL");
                profile.model_sonnet = env_str("ANTHROPIC_DEFAULT_SONNET_MODEL");
                profile.model_opus = env_str("ANTHROPIC_DEFAULT_OPUS_MODEL");
                profile.model_fable = env_str("ANTHROPIC_DEFAULT_FABLE_MODEL");
                profile.model_subagent = env_str("CLAUDE_CODE_SUBAGENT_MODEL");
                // 附加配置片段：只对片段里已有的叶子回填 live 值
                //（live 删了就同步删，用户自设的其他键不在管理范围，不回填）。
                backfill_fragment(&mut profile.extra_settings, &doc);
                // 认证形态跟随 live：用户手改成另一种键也如实回填。
                if routed {
                } else if let Some(live_token) = env_str("ANTHROPIC_AUTH_TOKEN") {
                    profile.api_key_field = ClaudeAuthField::AuthToken;
                    self.backfill_key(profile, &live_token)?;
                } else if let Some(live_token) = env_str("ANTHROPIC_API_KEY") {
                    profile.api_key_field = ClaudeAuthField::ApiKey;
                    self.backfill_key(profile, &live_token)?;
                }
            }
            ClientApp::Codex => {
                let loaded = engine.load(&paths.codex_config, ConfigFormat::Toml)?;
                let doc = ConfigDocument::parse(&loaded.content, ConfigFormat::Toml)?;
                let live_provider = doc
                    .get(&["model_provider"])
                    .and_then(|v| v.as_str().map(str::to_owned));
                let mut routed = false;
                // 只有 live 仍指向该中转站的表才回填。
                if live_provider.as_deref() == profile.codex_slug.as_deref()
                    && let Some(slug) = profile.codex_slug.clone()
                {
                    if let Some(live_base) = doc
                        .get(&["model_providers", &slug, "base_url"])
                        .and_then(|v| v.as_str().map(str::to_owned))
                    {
                        // 路由接管中：live 是 loopback 地址 + 网关 token，不能倒灌。
                        routed = crate::domain::is_router_url(&live_base);
                        if !routed {
                            profile.base_url = Some(live_base);
                        }
                    }
                    if !routed
                        && let Some(live_wire) = doc
                            .get(&["model_providers", &slug, "wire_api"])
                            .and_then(|v| v.as_str().map(str::to_owned))
                    {
                        profile.codex_wire_api = if live_wire == "chat" {
                            CodexWireApi::Chat
                        } else {
                            CodexWireApi::Responses
                        };
                    }
                    profile.model = doc
                        .get(&["model"])
                        .and_then(|v| v.as_str().map(str::to_owned));
                    // query_params 跟随 live（用户手改/删除 api-version 也如实回填）。
                    profile.codex_query_params = doc
                        .get(&["model_providers", &slug, "query_params"])
                        .and_then(|v| v.as_object().cloned())
                        .map(|map| {
                            map.into_iter()
                                .filter_map(|(k, v)| Some((k, v.as_str()?.to_owned())))
                                .collect()
                        })
                        .unwrap_or_default();
                    crate::reverse::apply_codex_context_window(profile, &doc);
                    // 附加配置片段：只回填片段里已有叶子的 live 值。
                    backfill_fragment(&mut profile.extra_settings, &doc);
                }
                let auth: &dyn AuthHandling = &CodexAdapter;
                let auth_content = read_optional(auth.auth_file(paths))?;
                if !routed
                    && auth.classify(auth_content.as_deref()) == AuthKind::Relay
                    && let Some(content) = auth_content
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(&content)
                    && let Some(live_key) = value["OPENAI_API_KEY"].as_str()
                {
                    self.backfill_key(profile, live_key)?;
                }
            }
            ClientApp::Gemini => {
                let env_loaded = engine.load(&paths.gemini_env, ConfigFormat::Env)?;
                let env_doc = ConfigDocument::parse(&env_loaded.content, ConfigFormat::Env)?;
                let env_str = |key: &str| {
                    env_doc
                        .get(&[key])
                        .and_then(|v| v.as_str().map(str::to_owned))
                };
                // 只有 live 仍是中转形态（有 base_url）才回填。
                let Some(live_base) = env_str("GOOGLE_GEMINI_BASE_URL") else {
                    return Ok(());
                };
                // 路由接管中：live 是 loopback 地址 + 网关 token，不能倒灌。
                let routed = crate::domain::is_router_url(&live_base);
                if !routed {
                    profile.base_url = Some(live_base);
                }
                profile.model = env_str("GEMINI_MODEL");
                if !routed && let Some(live_key) = env_str("GEMINI_API_KEY") {
                    self.backfill_key(profile, &live_key)?;
                }
                // 附加配置片段：只回填片段里已有叶子的 live 值（settings.json）。
                let loaded = engine.load(&paths.gemini_settings, ConfigFormat::Json)?;
                let doc = ConfigDocument::parse(&loaded.content, ConfigFormat::Json)?;
                backfill_fragment(&mut profile.extra_settings, &doc);
            }
            ClientApp::ClaudeDesktop => {
                // profile 由 MCPane 全量生成；只在 direct 模式回填被外部改过的
                // base_url/key（proxy 模式 profile 指 loopback，网关 token 非上游
                // 凭据，无可回填内容）。
                if profile.desktop_mode != DesktopMode::Direct {
                    return Ok(());
                }
                let Some(imported) = claude_desktop::read_active_import(&paths.claude_desktop)
                else {
                    return Ok(());
                };
                profile.base_url = Some(imported.base_url);
                self.backfill_key(profile, &imported.api_key)?;
            }
        }
        Ok(())
    }

    /// live Key 与凭据库现值不同才写回（避免无谓的 Keychain 写入）。
    fn backfill_key(
        &self,
        profile: &mut ProviderProfile,
        live_key: &str,
    ) -> Result<(), ProviderError> {
        let unchanged = profile
            .api_key_ref
            .as_ref()
            .and_then(|r| self.secrets.get(r).ok())
            .is_some_and(|stored| stored.expose() == live_key);
        if unchanged {
            return Ok(());
        }
        let reference = profile
            .api_key_ref
            .clone()
            .unwrap_or_else(SecretReference::generate);
        self.secrets.set(&reference, &Secret::new(live_key))?;
        profile.api_key_ref = Some(reference);
        Ok(())
    }

    /// 读取并校验官方 auth 备份；不可用时返回原因（供降级路径提示）。
    fn load_valid_auth_backup(&self, reference: &SecretReference) -> Result<String, String> {
        let secret = self
            .secrets
            .get(reference)
            .map_err(|e| format!("读取失败：{e}"))?;
        serde_json::from_str::<serde_json::Value>(secret.expose())
            .map_err(|e| format!("内容损坏：{e}"))?;
        Ok(secret.expose().to_owned())
    }

    /// 从 Claude Code 供应商批量导入 Claude Desktop 供应商（对齐 cc-switch
    /// import_claude_desktop_providers_from_claude）。逐条推导模式：
    /// 角色模型全为 claude-safe（或未配置）→ direct 透传；否则 → proxy 并由
    /// 角色映射推导四档路由（`[1M]` 后缀在边界单向翻译为 supports_1m）。
    /// 同端点已有 Desktop 条目跳过；统一供应商影子跳过（生命周期归 unified）。
    /// Key 复制为独立 Secret 副本。返回导入条数。
    pub fn import_desktop_from_claude(&self) -> Result<usize, ProviderError> {
        let mut state = self.store.load()?;
        let sources: Vec<ProviderProfile> = state
            .providers
            .iter()
            .filter(|p| p.app == ClientApp::Claude)
            .filter(|p| !p.id.starts_with(crate::domain::UNIFIED_ID_PREFIX))
            .cloned()
            .collect();
        let mut next_sort = state
            .providers
            .iter()
            .filter(|p| p.app == ClientApp::ClaudeDesktop)
            .map(|p| p.sort_order + 1)
            .max()
            .unwrap_or(0);
        let mut imported = 0usize;
        for source in sources {
            let Some(base_url) = source.base_url.clone().filter(|url| !url.trim().is_empty())
            else {
                continue;
            };
            if state
                .providers
                .iter()
                .any(|p| p.app == ClientApp::ClaudeDesktop && p.base_url == Some(base_url.clone()))
            {
                continue;
            }
            // 角色模型（未显式覆盖的回落到兜底模型，与 Claude 切换臂同语义）。
            // route id 取自 `DEFAULT_PROXY_ROUTE_IDS`（sonnet→opus→haiku→fable），
            // 不再手写字面量，杜绝换代时漏改。
            let role_models: [(&str, Option<&String>); 4] = [
                (
                    claude_desktop::DEFAULT_PROXY_ROUTE_IDS[0],
                    source.model_sonnet.as_ref().or(source.model.as_ref()),
                ),
                (
                    claude_desktop::DEFAULT_PROXY_ROUTE_IDS[1],
                    source.model_opus.as_ref().or(source.model.as_ref()),
                ),
                (
                    claude_desktop::DEFAULT_PROXY_ROUTE_IDS[2],
                    source.model_haiku.as_ref().or(source.model.as_ref()),
                ),
                (
                    claude_desktop::DEFAULT_PROXY_ROUTE_IDS[3],
                    source
                        .model_fable
                        .as_ref()
                        .or(source.model_opus.as_ref())
                        .or(source.model.as_ref()),
                ),
            ];
            let filled: Vec<(&str, &str)> = role_models
                .iter()
                .filter_map(|(route_id, value)| {
                    value
                        .map(|v| v.trim())
                        .filter(|v| !v.is_empty())
                        .map(|v| (*route_id, v))
                })
                .collect();
            let all_safe = filled
                .iter()
                .all(|(_, model)| claude_desktop::is_claude_safe_model_id(model));

            let mut dest =
                ProviderProfile::new_relay(ClientApp::ClaudeDesktop, source.name.clone());
            dest.base_url = Some(base_url);
            dest.api_key_field = source.api_key_field;
            dest.endpoint_candidates = source.endpoint_candidates.clone();
            dest.icon = source.icon.clone();
            dest.notes = source.notes.clone();
            dest.website_url = source.website_url.clone();
            dest.sort_order = next_sort;
            if filled.is_empty() || all_safe {
                dest.desktop_mode = DesktopMode::Direct;
            } else {
                dest.desktop_mode = DesktopMode::Proxy;
                dest.desktop_routes = filled
                    .iter()
                    .map(|(route_id, raw_model)| {
                        let stripped = claude_desktop::strip_one_m_suffix(raw_model);
                        DesktopRoute {
                            route_id: (*route_id).to_owned(),
                            upstream_model: stripped.to_owned(),
                            label_override: Some(stripped.to_owned()),
                            // cc-switch add_route：effective_supports_1m =
                            // supports_1m_default(普通供应商恒 true) || 有 [1M] 标记。
                            // 导入路由默认声明 1M——上游若不支持长上下文会自然报错，
                            // 而漏声明会让 Desktop 直接隐藏 1M 选项（用户验证过的取舍）。
                            supports_1m: true,
                        }
                    })
                    .collect();
            }
            if let Some(reference) = &source.api_key_ref {
                let secret = self.secrets.get(reference)?;
                let dest_ref = SecretReference::generate();
                self.secrets.set(&dest_ref, &secret)?;
                dest.api_key_ref = Some(dest_ref);
            }
            state.providers.push(dest);
            next_sort += 1;
            imported += 1;
        }
        if imported > 0 {
            self.store.save(&state)?;
        }
        Ok(imported)
    }

    // ---------- 本地路由（ADR-022） ----------

    /// 某 app 的路由配置（未配置过为默认值：关闭、空队列）。
    pub fn routing(&self, app: ClientApp) -> Result<RoutingConfig, ProviderError> {
        Ok(self
            .store
            .load()?
            .routing
            .get(&app)
            .cloned()
            .unwrap_or_default())
    }

    /// 保存路由配置并立即按新配置重写当前项的 live（开/关接管即时生效）。
    /// 队列只收同 app 的中转站；官方登录不经路由（避免账号风控）。
    pub fn set_routing(
        &self,
        app: ClientApp,
        mut config: RoutingConfig,
        engine: &Engine,
        paths: &ClientPaths,
    ) -> Result<RoutingConfig, ProviderError> {
        if !app.supports_routing() {
            return Err(ProviderError::InvalidOperation(format!(
                "{} 暂不支持本地路由",
                app_display(app)
            )));
        }
        config
            .tuning
            .validate()
            .map_err(ProviderError::InvalidOperation)?;
        let mut state = self.store.load()?;
        let mut seen = std::collections::BTreeSet::new();
        for id in &config.failover_queue {
            let valid = state
                .providers
                .iter()
                .any(|p| p.id == *id && p.app == app && p.category == ProviderCategory::Relay);
            if !valid {
                return Err(ProviderError::InvalidOperation(format!(
                    "故障转移队列只能包含 {} 的中转站：{id}",
                    app_display(app)
                )));
            }
        }
        config.failover_queue.retain(|id| seen.insert(id.clone()));
        let current_needs_router = state
            .current
            .get(&app)
            .and_then(|id| state.providers.iter().find(|p| p.id == *id))
            .is_some_and(|p| p.category == ProviderCategory::Relay && needs_router(p));
        if !config.enabled && current_needs_router {
            return Err(ProviderError::InvalidOperation(
                "当前供应商使用 Chat Completions 协议，依赖本地路由转换；请先切换到其他供应商再关闭".into(),
            ));
        }
        state.routing.insert(app, config.clone());
        if config.enabled {
            state
                .desktop_gateway
                .get_or_insert_with(DesktopGateway::generate);
        }
        self.store.save(&state)?;
        self.reapply_current_app(app, engine, paths)?;
        Ok(config)
    }

    /// 把候选按顺序并入某 app 的故障转移队列（迁移用）：只追加队列里还没有的同 app
    /// 中转站，保留用户已有顺序，不动路由开关。返回新加入的数量。
    pub fn append_failover_candidates(
        &self,
        app: ClientApp,
        ids: &[String],
    ) -> Result<usize, ProviderError> {
        let mut state = self.store.load()?;
        let valid: Vec<String> =
            ids.iter()
                .filter(|id| {
                    state.providers.iter().any(|p| {
                        p.id == **id && p.app == app && p.category == ProviderCategory::Relay
                    })
                })
                .cloned()
                .collect();
        let routing = state.routing.entry(app).or_default();
        let before = routing.failover_queue.len();
        for id in valid {
            if !routing.failover_queue.contains(&id) {
                routing.failover_queue.push(id);
            }
        }
        let added = routing.failover_queue.len() - before;
        if added > 0 {
            self.store.save(&state)?;
        }
        Ok(added)
    }

    /// 迁移用：该 app 仍是默认调参时采用外部（cc-switch）的调参；用户在 MCPane 里
    /// 调过的不覆盖。只改存储，不重写 live。返回是否采用。
    pub fn adopt_routing_tuning(
        &self,
        app: ClientApp,
        tuning: crate::domain::RoutingTuning,
    ) -> Result<bool, ProviderError> {
        tuning.validate().map_err(ProviderError::InvalidOperation)?;
        let mut state = self.store.load()?;
        let routing = state.routing.entry(app).or_default();
        if routing.tuning != crate::domain::RoutingTuning::default() || routing.tuning == tuning {
            return Ok(false);
        }
        routing.tuning = tuning;
        self.store.save(&state)?;
        Ok(true)
    }

    /// 开启了路由的 app（启动重新接管 / 退出还原直连用）。
    pub fn routed_apps(&self) -> Result<Vec<ClientApp>, ProviderError> {
        Ok(self
            .store
            .load()?
            .routing
            .iter()
            .filter(|(app, config)| config.enabled && app.supports_routing())
            .map(|(app, _)| *app)
            .collect())
    }

    /// 未开路由的 app 其 live 却仍指向本地路由（关路由时写盘与改 live 之间被强杀 / 断电）：
    /// 按直连重写当前项。以 live 内容为准而非开关，开关与 live 失配时也能自愈。
    pub fn heal_stale_takeover(
        &self,
        app: ClientApp,
        engine: &Engine,
        paths: &ClientPaths,
    ) -> Result<bool, ProviderError> {
        let state = self.store.load()?;
        if !app.supports_routing() || state.routing.get(&app).is_some_and(|r| r.enabled) {
            return Ok(false);
        }
        let live_path = match app {
            ClientApp::Claude => &paths.claude_settings,
            ClientApp::Codex => &paths.codex_config,
            ClientApp::Gemini => &paths.gemini_env,
            ClientApp::Grok => &paths.grok_config,
            _ => return Ok(false),
        };
        let content = std::fs::read_to_string(live_path).unwrap_or_default();
        let points_at_router = content
            .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '='))
            .any(crate::domain::is_router_url);
        if !points_at_router {
            return Ok(false);
        }
        self.reapply_current_app(app, engine, paths)
    }

    /// 按路由开关重写当前项 live（启动时重新接管、改端口后刷新地址）。
    pub fn reapply_routing(
        &self,
        app: ClientApp,
        engine: &Engine,
        paths: &ClientPaths,
    ) -> Result<bool, ProviderError> {
        self.reapply_current_app(app, engine, paths)
    }

    /// 当前项按直连重写 live，不改路由开关（MCPane 退出前调用，保证客户端
    /// 在路由不在时仍可用；下次启动由 [`Self::reapply_routing`] 重新接管）。
    pub fn restore_direct(
        &self,
        app: ClientApp,
        engine: &Engine,
        paths: &ClientPaths,
    ) -> Result<bool, ProviderError> {
        let Some(id) = self.store.load()?.current.get(&app).cloned() else {
            return Ok(false);
        };
        self.switch_with(app, &id, engine, paths, false)?;
        Ok(true)
    }

    /// Claude Desktop 本地网关配置（proxy 模式的 token/端口）；未生成过为 None。
    pub fn desktop_gateway(&self) -> Result<Option<DesktopGateway>, ProviderError> {
        Ok(self.store.load()?.desktop_gateway)
    }

    /// 确保网关存在（首次生成并持久化，之后幂等返回）。
    pub fn ensure_desktop_gateway(&self) -> Result<DesktopGateway, ProviderError> {
        let mut state = self.store.load()?;
        if let Some(gateway) = &state.desktop_gateway {
            return Ok(gateway.clone());
        }
        let gateway = DesktopGateway::generate();
        state.desktop_gateway = Some(gateway.clone());
        self.store.save(&state)?;
        Ok(gateway)
    }

    /// 更新网关监听端口（1024-65535；重启代理/重新切换后生效）。
    pub fn set_desktop_proxy_port(&self, port: u16) -> Result<DesktopGateway, ProviderError> {
        if port < 1024 {
            return Err(ProviderError::InvalidOperation(
                "端口需在 1024-65535 之间".into(),
            ));
        }
        let mut state = self.store.load()?;
        let gateway = state
            .desktop_gateway
            .get_or_insert_with(DesktopGateway::generate);
        gateway.listen_port = port;
        let gateway = gateway.clone();
        self.store.save(&state)?;
        Ok(gateway)
    }

    /// Claude Desktop 中转站：按模式写 gateway profile（4 文件协同 + 快照回滚，
    /// 见 claude_desktop 模块）。不走 mcpane-config 引擎——profile 由 MCPane 全量
    /// 生成（含明文网关 key），引擎快照会把 key 落盘且无脱敏路径。
    fn switch_claude_desktop_relay(
        &self,
        profile: &ProviderProfile,
        state: &mut ProviderState,
        paths: &ClientPaths,
        warnings: &mut Vec<String>,
    ) -> Result<bool, ProviderError> {
        if !claude_desktop::is_supported_platform() {
            return Err(ProviderError::InvalidOperation(
                "当前平台暂不支持 Claude Desktop 3P 配置写入".into(),
            ));
        }
        if profile.claude_api_format != ClaudeApiFormat::Anthropic
            && profile.desktop_mode == DesktopMode::Direct
        {
            return Err(ProviderError::InvalidOperation(
                "该供应商使用 OpenAI 协议接口，需开启「模型映射」经本地路由转换协议".into(),
            ));
        }
        match profile.desktop_mode {
            DesktopMode::Direct => {
                let base_url = require_base_url(profile)?;
                let key = self.materialize_key(profile)?;
                claude_desktop::apply_direct(
                    &paths.claude_desktop,
                    base_url,
                    key.expose(),
                    &profile.desktop_routes,
                )
                .map_err(ProviderError::InvalidOperation)?;
                warnings.push("切换成功，重启 Claude Desktop 后生效".into());
            }
            DesktopMode::Proxy => {
                let gateway = state
                    .desktop_gateway
                    .get_or_insert_with(DesktopGateway::generate)
                    .clone();
                claude_desktop::apply_proxy(
                    &paths.claude_desktop,
                    gateway.listen_port,
                    &gateway.token,
                    &profile.desktop_routes,
                )
                .map_err(ProviderError::InvalidOperation)?;
                warnings.push("切换成功，请保持 MCPane 运行，并重启 Claude Desktop 后生效".into());
            }
        }
        Ok(true)
    }

    /// Claude Desktop 官方：恢复内置 1P 登录（"1p" + 清理网关残留）。
    /// 恢复同样需要重启 Desktop 才切回官方推理（对齐 cc-switch 的非 proxy toast）。
    fn switch_claude_desktop_official(
        &self,
        paths: &ClientPaths,
        warnings: &mut Vec<String>,
    ) -> Result<bool, ProviderError> {
        if !claude_desktop::is_supported_platform() {
            return Err(ProviderError::InvalidOperation(
                "当前平台暂不支持 Claude Desktop 3P 配置写入".into(),
            ));
        }
        claude_desktop::restore_official(&paths.claude_desktop)
            .map_err(ProviderError::InvalidOperation)?;
        warnings.push("切换成功，重启 Claude Desktop 后生效".into());
        Ok(true)
    }

    fn materialize_key(&self, profile: &ProviderProfile) -> Result<Secret, ProviderError> {
        let reference = profile.api_key_ref.as_ref().ok_or_else(|| {
            ProviderError::InvalidOperation(format!("中转站「{}」尚未录入 API Key", profile.name))
        })?;
        Ok(self.secrets.get(reference)?)
    }
}

// ---------- 无状态辅助 ----------

/// 统一供应商在某 app 下是否物化影子。Gemini / Claude Desktop 暂不支持（恒 false；
/// Desktop 有独立的模式/路由维度，网关条目直接建 Desktop 供应商更贴合）。
fn unified_apps_enabled(unified: &UnifiedProvider, app: ClientApp) -> bool {
    match app {
        ClientApp::Claude => unified.apps.claude,
        ClientApp::Codex => unified.apps.codex,
        ClientApp::Gemini
        | ClientApp::ClaudeDesktop
        | ClientApp::Grok
        | ClientApp::OpenCode
        | ClientApp::OpenClaw
        | ClientApp::Hermes => false,
    }
}

fn official_id_for(app: ClientApp) -> &'static str {
    match app {
        ClientApp::Claude => OFFICIAL_CLAUDE_ID,
        ClientApp::Codex => OFFICIAL_CODEX_ID,
        ClientApp::Gemini => OFFICIAL_GEMINI_ID,
        ClientApp::ClaudeDesktop => OFFICIAL_CLAUDE_DESKTOP_ID,
        ClientApp::Grok => OFFICIAL_GROK_ID,
        ClientApp::OpenCode => crate::domain::OFFICIAL_OPENCODE_ID,
        ClientApp::OpenClaw => crate::domain::OFFICIAL_OPENCLAW_ID,
        ClientApp::Hermes => crate::domain::OFFICIAL_HERMES_ID,
    }
}

/// 播种用的官方 Profile：套用（并消费）老版本遗留的可编辑覆盖项，
/// 使升级用户在官方项落库后仍保有此前编辑过的附加配置。
fn official_seed(state: &mut ProviderState, app: ClientApp) -> ProviderProfile {
    let mut profile = ProviderProfile::official(app);
    if let Some(override_slot) = state.official_overrides.remove(&app) {
        profile.extra_settings = override_slot.extra_settings;
        profile.common_config_enabled = override_slot.common_config_enabled;
    }
    profile
}

fn resolve_target(
    state: &ProviderState,
    app: ClientApp,
    provider_id: &str,
) -> Result<ProviderProfile, ProviderError> {
    state
        .providers
        .iter()
        .find(|p| p.id == provider_id && p.app == app)
        .cloned()
        .ok_or_else(|| ProviderError::NotFound(provider_id.to_owned()))
}

fn validate_relay_fields(profile: &ProviderProfile) -> Result<(), ProviderError> {
    if profile.name.trim().is_empty() {
        return Err(ProviderError::InvalidOperation("名称不能为空".into()));
    }
    if profile
        .base_url
        .as_deref()
        .is_none_or(|url| url.trim().is_empty())
    {
        return Err(ProviderError::InvalidOperation(
            "中转站必须配置 base_url".into(),
        ));
    }
    validate_fragment(profile.app, &profile.extra_settings, "附加配置")?;
    if !profile.route_body_override.is_null() && !profile.route_body_override.is_object() {
        return Err(ProviderError::InvalidOperation(
            "请求体覆盖必须是 JSON 对象".into(),
        ));
    }
    Ok(())
}

/// 附加配置片段 / 通用配置片段的公共校验：
/// 必须是 JSON object（或 Null 表示空），且不得覆盖受管键
///（认证/模型/路由键必须走结构化字段，否则切换清理与快照脱敏都会失效；
/// 受管键集合由各客户端 adapter 定义）。
fn validate_fragment(
    app: ClientApp,
    fragment: &serde_json::Value,
    label: &str,
) -> Result<(), ProviderError> {
    if fragment.is_null() {
        return Ok(());
    }
    if !fragment.is_object() {
        return Err(ProviderError::InvalidOperation(format!(
            "{label}片段必须是 JSON 对象"
        )));
    }
    let adapter = adapter_for(app);
    for (path, _) in fragment_leaves(fragment) {
        if let Some(key) = adapter.managed_key_violation(&path) {
            return Err(ProviderError::InvalidOperation(format!(
                "{label}片段不允许包含受管键：{key}"
            )));
        }
    }
    Ok(())
}

/// Codex 1M 窗口：按目标 profile 写/删顶层键。
fn codex_context_window_ops(enabled: bool, compact: Option<u64>) -> Vec<PatchOp> {
    if enabled {
        let compact = compact
            .filter(|&n| n > 0)
            .unwrap_or(CODEX_AUTO_COMPACT_LIMIT_1M);
        vec![
            PatchOp::Set {
                path: vec!["model_context_window".into()],
                value: serde_json::json!(CODEX_CONTEXT_WINDOW_1M),
            },
            PatchOp::Set {
                path: vec!["model_auto_compact_token_limit".into()],
                value: serde_json::json!(compact),
            },
        ]
    } else {
        vec![
            PatchOp::Remove {
                path: vec!["model_context_window".into()],
            },
            PatchOp::Remove {
                path: vec!["model_auto_compact_token_limit".into()],
            },
        ]
    }
}

/// 全部 Codex 中转站的 slug（排除 `except` 指定的 Provider），用于清理。
fn codex_relay_slugs(state: &ProviderState, except: Option<&str>) -> Vec<String> {
    state
        .providers
        .iter()
        .filter(|p| p.app == ClientApp::Codex && except.is_none_or(|id| p.id != id))
        .filter_map(|p| p.codex_slug.clone())
        .filter(|slug| slug.starts_with(CODEX_SLUG_PREFIX))
        .collect()
}

/// 中转站的有效附加片段：profile 片段 ⊕ 通用配置（启用时；同名键以通用配置为准，
/// 对齐 cc-switch deepMerge(snippet over config) 语义）。
fn effective_fragment(
    profile: &ProviderProfile,
    common_snippet: Option<&serde_json::Value>,
) -> serde_json::Value {
    let mut result = if profile.extra_settings.is_object() {
        profile.extra_settings.clone()
    } else {
        serde_json::Value::Object(serde_json::Map::new())
    };
    if profile.common_config_enabled
        && let Some(snippet) = common_snippet
        && snippet.is_object()
    {
        json_deep_merge(&mut result, snippet);
    }
    result
}

/// overlay 深合并进 base：对象递归合并，非对象值由 overlay 覆盖。
fn json_deep_merge(base: &mut serde_json::Value, overlay: &serde_json::Value) {
    if let (serde_json::Value::Object(base_map), serde_json::Value::Object(overlay_map)) =
        (&mut *base, overlay)
    {
        for (key, value) in overlay_map {
            match base_map.get_mut(key) {
                Some(slot) if slot.is_object() && value.is_object() => {
                    json_deep_merge(slot, value);
                }
                _ => {
                    base_map.insert(key.clone(), value.clone());
                }
            }
        }
    } else {
        *base = overlay.clone();
    }
}

/// 递归收集片段的全部叶子（路径 → 值）；空对象视作一个叶子。
fn fragment_leaves(fragment: &serde_json::Value) -> Vec<(Vec<String>, serde_json::Value)> {
    let mut leaves = Vec::new();
    if fragment.is_object() {
        collect_fragment_leaves(&mut Vec::new(), fragment, &mut leaves);
    }
    leaves
}

fn collect_fragment_leaves(
    prefix: &mut Vec<String>,
    value: &serde_json::Value,
    out: &mut Vec<(Vec<String>, serde_json::Value)>,
) {
    match value {
        serde_json::Value::Object(map) if !map.is_empty() => {
            for (key, child) in map {
                prefix.push(key.clone());
                collect_fragment_leaves(prefix, child, out);
                prefix.pop();
            }
        }
        _ if prefix.is_empty() => {} // 顶层非对象不产生叶子（validate 已拦截）
        _ => out.push((prefix.clone(), value.clone())),
    }
}

/// 读取文档中某路径的值（ConfigDocument::get 需要 &str 切片）。
fn doc_value_at(doc: &ConfigDocument, path: &[String]) -> Option<serde_json::Value> {
    let segs: Vec<&str> = path.iter().map(String::as_str).collect();
    doc.get(&segs)
}

/// 生成上一片段的清理 ops（对齐 cc-switch deepRemove 语义）：
/// - 对象节点：live 值与片段完全一致 → 整体移除（避免留下空父级）；
///   `env` 节点除外（受管认证/模型键与片段共存其中，只做叶子级移除）；
/// - 叶子：live 值仍与写入时一致才移除（不动用户手改）；
/// - `new_leaves`（本次将重写的叶子）覆盖的路径跳过移除。
fn fragment_removal_ops(
    doc: &ConfigDocument,
    fragment: &serde_json::Value,
    new_leaves: &[(Vec<String>, serde_json::Value)],
) -> Vec<PatchOp> {
    fn walk(
        prefix: &mut Vec<String>,
        frag: &serde_json::Value,
        doc: &ConfigDocument,
        new_leaves: &[(Vec<String>, serde_json::Value)],
        ops: &mut Vec<PatchOp>,
    ) {
        let overwritten = |path: &[String]| {
            new_leaves
                .iter()
                .any(|(p, _)| p.len() >= path.len() && p[..path.len()] == *path)
        };
        match frag {
            serde_json::Value::Object(map) if !map.is_empty() => {
                let live = if prefix.is_empty() {
                    None
                } else {
                    doc_value_at(doc, prefix)
                };
                let is_env_node = prefix.len() == 1 && prefix[0] == "env";
                if !prefix.is_empty()
                    && !is_env_node
                    && !overwritten(prefix)
                    && live.as_ref() == Some(frag)
                {
                    ops.push(PatchOp::Remove {
                        path: prefix.clone(),
                    });
                    return;
                }
                for (key, child) in map {
                    prefix.push(key.clone());
                    walk(prefix, child, doc, new_leaves, ops);
                    prefix.pop();
                }
            }
            _ => {
                if !prefix.is_empty()
                    && !overwritten(prefix)
                    && doc_value_at(doc, prefix).is_some_and(|live| live == *frag)
                {
                    ops.push(PatchOp::Remove {
                        path: prefix.clone(),
                    });
                }
            }
        }
    }
    let mut ops = Vec::new();
    if fragment.is_object() {
        walk(&mut Vec::new(), fragment, doc, new_leaves, &mut ops);
    }
    ops
}

/// 回填：片段里已有的叶子按 live 值更新，live 已删的叶子同步删除（并清空父级空对象）。
fn backfill_fragment(fragment: &mut serde_json::Value, doc: &ConfigDocument) {
    let leaves = fragment_leaves(fragment);
    for (path, _) in leaves {
        match doc_value_at(doc, &path) {
            Some(live) => set_fragment_leaf(fragment, &path, live),
            None => remove_fragment_leaf(fragment, &path),
        }
    }
    if fragment.as_object().is_some_and(serde_json::Map::is_empty) {
        *fragment = serde_json::Value::Null;
    }
}

fn set_fragment_leaf(fragment: &mut serde_json::Value, path: &[String], value: serde_json::Value) {
    let mut cursor = fragment;
    for seg in &path[..path.len() - 1] {
        let Some(next) = cursor.get_mut(seg) else {
            return;
        };
        cursor = next;
    }
    if let (Some(map), Some(last)) = (cursor.as_object_mut(), path.last()) {
        map.insert(last.clone(), value);
    }
}

fn remove_fragment_leaf(fragment: &mut serde_json::Value, path: &[String]) {
    fn inner(value: &mut serde_json::Value, path: &[String]) -> bool {
        let Some(map) = value.as_object_mut() else {
            return false;
        };
        if path.len() == 1 {
            map.remove(&path[0]);
        } else if let Some(child) = map.get_mut(&path[0])
            && inner(child, &path[1..])
        {
            map.remove(&path[0]);
        }
        map.is_empty()
    }
    inner(fragment, path);
}

/// 单个端点的测速结果（对齐 cc-switch EndpointLatency）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointLatency {
    pub url: String,
    /// HTTP 状态码（任何响应都算网络可达，含 401/404）。
    pub status: Option<u16>,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
}

/// 并发测量各端点的 HTTP 首响应延迟（8s 超时；对齐 cc-switch 的多端点测速）。
pub async fn test_endpoints(urls: Vec<String>) -> Vec<EndpointLatency> {
    let handles: Vec<_> = urls
        .into_iter()
        .map(|url| {
            // 按各自的目标取客户端：本地端点（自建网关 / 本机模型）强制直连，
            // 公网端点才走代理。
            let client = crate::http_client(&url);
            tokio::spawn(async move {
                let started = std::time::Instant::now();
                match client
                    .get(&url)
                    .timeout(std::time::Duration::from_secs(8))
                    .send()
                    .await
                {
                    Ok(response) => EndpointLatency {
                        url,
                        status: Some(response.status().as_u16()),
                        latency_ms: Some(started.elapsed().as_millis() as u64),
                        error: None,
                    },
                    Err(error) => EndpointLatency {
                        url,
                        status: None,
                        latency_ms: None,
                        error: Some(error.to_string()),
                    },
                }
            })
        })
        .collect();
    let mut results = Vec::with_capacity(handles.len());
    for handle in handles {
        if let Ok(result) = handle.await {
            results.push(result);
        }
    }
    results
}

fn app_display(app: ClientApp) -> &'static str {
    adapter_for(app).display_name()
}

fn require_base_url(profile: &ProviderProfile) -> Result<&str, ProviderError> {
    profile.base_url.as_deref().ok_or_else(|| {
        ProviderError::InvalidOperation(format!("中转站「{}」未配置 base_url", profile.name))
    })
}

/// Claude settings 快照脱敏集：写前 live 中受管 env 键里的 token 值 +
/// 可选的新 token。快照存写前内容，只脱敏新 token 会漏掉旧中转站的 Key。
fn claude_snapshot_redactions(loaded_content: &str, new_token: Option<&str>) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    if let Ok(doc) = ConfigDocument::parse(loaded_content, ConfigFormat::Json) {
        for key in ["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"] {
            if let Some(value) = doc
                .get(&["env", key])
                .and_then(|v| v.as_str().map(str::to_owned))
                && !value.is_empty()
                && !tokens.contains(&value)
            {
                tokens.push(value);
            }
        }
    }
    if let Some(token) = new_token
        && !token.is_empty()
        && !tokens.iter().any(|t| t == token)
    {
        tokens.push(token.to_owned());
    }
    tokens
}

/// Gemini `.env` 快照脱敏集：写前 live 里的旧 GEMINI_API_KEY 值 + 可选的新 Key。
fn gemini_snapshot_redactions(env_content: &str, new_token: Option<&str>) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    if let Ok(doc) = ConfigDocument::parse(env_content, ConfigFormat::Env)
        && let Some(value) = doc
            .get(&["GEMINI_API_KEY"])
            .and_then(|v| v.as_str().map(str::to_owned))
        && !value.is_empty()
    {
        tokens.push(value);
    }
    if let Some(token) = new_token
        && !token.is_empty()
        && !tokens.iter().any(|t| t == token)
    {
        tokens.push(token.to_owned());
    }
    tokens
}

/// additive 客户端的 provider 键：用名称派生的 slug，用户在配置文件里
/// 一眼能认出是哪家（uuid 就完全读不出来了）。
/// additive 客户端要写进配置的模型列表（去空白、去重、保序）。
///
/// additive 客户端只认显式声明过的模型，而且要的是**一组**——用 `models`
/// 字段承载。老数据（本字段加入之前建的）没有它，回退到单值 `model`，
/// 免得升级后已有供应商突然启用不了。
fn additive_models(profile: &ProviderProfile) -> Vec<String> {
    if profile.models.is_empty() {
        profile.model.iter().cloned().collect()
    } else {
        profile.models.clone()
    }
    .into_iter()
    .map(|m| m.trim().to_owned())
    .filter(|m| !m.is_empty())
    .fold(Vec::new(), |mut acc, m| {
        if !acc.contains(&m) {
            acc.push(m);
        }
        acc
    })
}

fn additive_provider_id(profile: &ProviderProfile) -> String {
    let sanitized: String = profile
        .name
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned();
    if sanitized.is_empty() {
        // 按字符截取而非字节切片：id 通常是 ASCII，但这里不依赖这个假设
        // （非 ASCII id 的字节切片会在 char 边界内 panic）。
        let prefix: String = profile.id.chars().take(8).collect();
        format!("mcpane-{prefix}")
    } else {
        sanitized
    }
}

/// additive 客户端的快照脱敏：配置里每条 provider 的 Key + 即将写入的新 Key。
/// 两家的 Key 位置不同（OpenCode 在 `options.apiKey`，OpenClaw 在 `apiKey`），
/// 但都是明文落在配置文件里——漏一处历史里就留下可读凭据。
fn additive_snapshot_redactions(
    app: ClientApp,
    config_content: &str,
    new_token: Option<&str>,
) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let format = match app {
        ClientApp::OpenClaw => ConfigFormat::Json5,
        ClientApp::Hermes => ConfigFormat::Yaml,
        _ => ConfigFormat::Json,
    };
    // Hermes 的容器是数组，键位置也不同，单独走一条。
    if app == ClientApp::Hermes {
        let mut tokens: Vec<String> = ConfigDocument::parse(config_content, format)
            .ok()
            .and_then(|doc| doc.get(&[mcpane_adapter_hermes::CUSTOM_PROVIDERS_KEY]))
            .and_then(|list| list.as_array().cloned())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("api_key").and_then(|v| v.as_str()))
                    .filter(|key| !key.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if let Some(token) = new_token
            && !token.is_empty()
            && !tokens.iter().any(|t| t == token)
        {
            tokens.push(token.to_owned());
        }
        return tokens;
    }
    let table = ConfigDocument::parse(config_content, format)
        .ok()
        .and_then(|doc| {
            if app == ClientApp::OpenClaw {
                doc.get(&mcpane_adapter_openclaw::PROVIDERS_PATH)
            } else {
                doc.get(&[mcpane_adapter_opencode::PROVIDER_TABLE_KEY])
            }
        });
    if let Some(table) = table.as_ref().and_then(|v| v.as_object()) {
        for entry in table.values() {
            let key = if app == ClientApp::OpenClaw {
                entry.get("apiKey")
            } else {
                entry.get("options").and_then(|o| o.get("apiKey"))
            };
            if let Some(key) = key.and_then(|v| v.as_str())
                && !key.is_empty()
                && !tokens.iter().any(|t| t == key)
            {
                tokens.push(key.to_owned());
            }
        }
    }
    if let Some(token) = new_token
        && !token.is_empty()
        && !tokens.iter().any(|t| t == token)
    {
        tokens.push(token.to_owned());
    }
    tokens
}

/// Grok 快照脱敏：config.toml 里我们写过的每张模型表的 api_key，
/// 加上即将写入的新 Key。Grok 没有独立凭据文件，Key 就明晃晃地躺在配置里。
fn grok_snapshot_redactions(config_content: &str, new_token: Option<&str>) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    if let Ok(doc) = ConfigDocument::parse(config_content, ConfigFormat::Toml) {
        for slug in stale_grok_slugs(config_content, "") {
            if let Some(value) = doc
                .get(&[mcpane_adapter_grok::MODEL_TABLE_KEY, &slug, "api_key"])
                .and_then(|v| v.as_str().map(str::to_owned))
                && !value.is_empty()
                && !tokens.contains(&value)
            {
                tokens.push(value);
            }
        }
    }
    if let Some(token) = new_token
        && !token.is_empty()
        && !tokens.iter().any(|t| t == token)
    {
        tokens.push(token.to_owned());
    }
    tokens
}

/// 断开连接（切回官方）时找回 MCPane 接管 `[models].default` 之前用户自己
/// 设的原值（P0-3）。
///
/// Grok 没有独立的"官方形态"备份——凭据由 CLI 自管，指针一旦被
/// `PatchOp::Remove` 清空就无法凭空恢复。但引擎的写入流水线本就在每次
/// 写入前落一份写前快照（`ConfigEngine::apply` 步骤 4「Snapshot」，见
/// `mcpane-config/src/engine.rs`），这份快照记录的正是"这次改动之前"的
/// 完整文件内容——不需要为此新增任何持久化字段，复用现成的即可。
///
/// 由近及远走操作历史，第一条快照里 `default` **不是** mcpane 表的，就是
/// "MCPane 第一次接管这个指针"那一刻之前的状态；中间几次 mcpane 中转站
/// 互切（A→B→C）的快照都会被正确跳过，因为它们的 `default` 全部指向
/// mcpane 自己的表。
///
/// 找不到（从未被 MCPane 写过、历史被按大小轮转掉、或读取失败）返回
/// `None`，调用方据此回落到清空指针——不比早期实现更差，只是不再是唯一
/// 结果。
fn find_prior_grok_default(engine: &Engine, grok_config_path: &Path) -> Option<String> {
    let records = engine.history().list().ok()?;
    for record in records.iter().rev() {
        if record.target_path.as_path() != grok_config_path {
            continue;
        }
        let Some(snapshot) = record.snapshot.as_ref() else {
            continue;
        };
        let Ok(content) = engine.snapshots().load(snapshot) else {
            continue;
        };
        let Ok(doc) = ConfigDocument::parse(&content, ConfigFormat::Toml) else {
            continue;
        };
        let default = doc
            .get(&["models", "default"])
            .and_then(|v| v.as_str().map(str::to_owned));
        match default {
            Some(slug) if slug.starts_with(mcpane_adapter_grok::SLUG_PREFIX) => continue,
            other => return other,
        }
    }
    None
}

/// config.toml 里由 MCPane 写入、且不是 `keep` 的模型表名。
/// 只认 `mcpane-` 前缀，用户手写的表一律不动。
fn stale_grok_slugs(config_content: &str, keep: &str) -> Vec<String> {
    let Ok(doc) = ConfigDocument::parse(config_content, ConfigFormat::Toml) else {
        return Vec::new();
    };
    doc.get(&[mcpane_adapter_grok::MODEL_TABLE_KEY])
        .and_then(|tables| tables.as_object().cloned())
        .map(|tables| {
            tables
                .keys()
                .filter(|slug| {
                    slug.starts_with(mcpane_adapter_grok::SLUG_PREFIX) && slug.as_str() != keep
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn redact_all(content: &str, secrets: &[String]) -> String {
    let mut out = content.to_owned();
    for secret in secrets {
        out = out.replace(secret, "[REDACTED:mcpane-api-key]");
    }
    out
}

fn read_optional(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// auth.json 独立原子写：temp + fsync + rename，unix 下 0600。
/// 不走 ConfigEngine——引擎快照会把 OAuth Token 明文落盘。
fn write_auth_atomically(path: &Path, content: &str) -> std::io::Result<()> {
    mcpane_config::atomic_io::atomic_write(path, content, true, true)
}

/// Codex 侧验证：config.toml 不允许出现疑似明文 Key（Key 只进 auth.json）。
fn validate_codex_no_plaintext_key(doc: &ConfigDocument) -> Result<(), String> {
    if let Some(serde_json::Value::Object(providers)) = doc.get(&["model_providers"]) {
        for (slug, table) in providers {
            if let Some(obj) = table.as_object() {
                for (key, value) in obj {
                    let text = value.as_str().unwrap_or_default();
                    if key != "env_key" && (text.starts_with("sk-") || text.starts_with("Bearer "))
                    {
                        return Err(format!("model_providers.{slug}.{key} 疑似明文 Key"));
                    }
                }
            }
        }
    }
    Ok(())
}
