//! Hub 聚合状态（TASK-404 Registry / TASK-406 Router 数据面）。
//!
//! - 下游实例注册表（server_id → DownstreamHandle）；
//! - 当前 Catalog Revision（Session 在创建时 pin 住快照，ADR-004）；
//! - Tool / Prompt / Resource 禁用集（调用时二次权限校验，对已 pin 的会话即时生效）；
//! - 请求日志仓（TASK-407，可选：无 SQLite 时仅 tracing）。
//!
//! 锁策略：全部 std RwLock（临界区只做克隆/替换，绝不跨 await 持有），
//! 使 Session 工厂（同步闭包）能直接 pin 快照。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, RwLock};

use mcpane_hub_catalog::{CatalogOverrides, CatalogRevision, ServerTools, build_revision};
use mcpane_hub_downstream::oauth::OAuthLogins;
use mcpane_hub_downstream::{DownstreamError, DownstreamHandle, SpawnSpec};
use mcpane_net::ProxyConfig;
use mcpane_storage::LogRepo;

/// 下游状态机（TASK-404）：Stopped 由"未注册"表达。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServerStatus {
    Starting,
    Ready,
    Degraded,
    Failed,
    /// 远程服务器要求 OAuth 登录（ADR-024）。不是故障：等用户登录，健康监控不对它重连。
    NeedsAuth,
}

/// 注册到 Hub 的下游 Server（运行时视图）。
///
/// `handle` 为 `None`：曾尝试连接但未成功（或已清空死句柄），仍占注册表槽位，
/// 让诊断能报 `starting`/`failed`，健康监控也能按 `spec` 重试——不再靠前端
/// 「Hub 可达但诊断缺条目 → 合成启动中」永久撒谎。
#[derive(Clone)]
pub struct RegisteredServer {
    pub server_id: String,
    pub slug: String,
    pub spec: SpawnSpec,
    pub handle: Option<Arc<DownstreamHandle>>,
    pub status: Arc<RwLock<ServerStatus>>,
    pub last_error: Arc<RwLock<Option<String>>>,
    /// 失败原因代号（`DownstreamError::failure_kind`），界面据此显示具体原因。
    pub last_error_kind: Arc<RwLock<Option<String>>>,
    /// 服务器 401 时的 `WWW-Authenticate` 原文，OAuth 登录据此发现授权服务器。
    pub auth_challenge: Arc<RwLock<Option<String>>>,
}

impl RegisteredServer {
    pub fn status(&self) -> ServerStatus {
        *read_lock(&self.status)
    }

    pub fn set_status(&self, status: ServerStatus) {
        *write_lock(&self.status) = status;
    }

    pub fn last_error(&self) -> Option<String> {
        read_lock(&self.last_error).clone()
    }

    pub fn set_last_error(&self, error: Option<String>) {
        *write_lock(&self.last_error) = error;
    }

    pub fn last_error_kind(&self) -> Option<String> {
        read_lock(&self.last_error_kind).clone()
    }

    pub fn auth_challenge(&self) -> Option<String> {
        read_lock(&self.auth_challenge).clone()
    }
}

/// Hub 全局聚合状态。
/// Workspace ID → Catalog Revision 的不可变快照（Session pin 整张映射）。
pub type CatalogMap = HashMap<String, Arc<CatalogRevision>>;

/// P0 默认 Workspace ID。
pub const DEFAULT_WORKSPACE: &str = "ws_default";

/// 会让某些连接的列表（tools/prompts/resources）发生变化的事件
/// （`subscriptions/listen` 的输入）。
///
/// 分两类而不是只广播 workspace_id：改绑 Workspace、访问控制启停改的都是
/// 「某几条连接能看见什么」，受影响的是连接而非 Workspace，光有
/// workspace_id 表达不了。这两类恰恰是列表整体变化最典型的场景
/// （ADR-004 通篇讲的就是前者），漏掉它们，listChanged 就替代不了 Session-Pin。
#[derive(Clone, Debug)]
pub enum CatalogChange {
    /// 指定 Workspace 的 Catalog 被重建（三类列表都可能变）。
    Rebuilt(String),
    /// 指定连接的可见范围变了：改绑 Workspace 或访问控制启停。
    /// 载荷是**受影响连接**的集合——发送方持有新旧两份映射，算 diff 的
    /// 成本在它那里最低；订阅端只需查自己在不在其中。
    ConnectionsChanged(Arc<HashSet<String>>),
}

/// 连接的工具暴露模式（存储层字符串的应用层收敛，ADR-020）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExposureMode {
    /// 平铺：tools/list 直接暴露全量真实工具。
    Flat,
    /// 网关：tools/list 只暴露元工具 + 已升格工具，目录经 discovery/instructions 直给。
    Gateway,
    /// 按客户端自适应：Claude Code（自带延迟加载）→ Flat，其余沿用 Workspace 开关。
    Auto,
}

impl ExposureMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::Gateway => "gateway",
            Self::Auto => "auto",
        }
    }

    /// 未知值按 Auto 处理并告警——枚举不加 DB CHECK 的对价（0019 迁移注释）。
    pub fn parse(value: &str) -> Self {
        match value {
            "flat" => Self::Flat,
            "gateway" => Self::Gateway,
            "auto" => Self::Auto,
            other => {
                tracing::warn!(mode = other, "未知的 tool_exposure_mode，按 auto 处理");
                Self::Auto
            }
        }
    }
}

/// 单连接可升格工具数上限：防异常客户端无限 describe 撑爆内存。
/// 正常量级是个位数到几十（一次会话实际用到的工具数），256 远超实际需要。
const MAX_PROMOTED_PER_CONNECTION: usize = 256;

/// 一个 Workspace 下被治理禁用的条目，按类别分开存。
///
/// 三类分开而不是并成一张表：同名的 tool 与 prompt 是两个不同的条目
/// （storage 的 `workspace_tools.kind` 自 0010 起就是这个语义），并进一张表
/// 会让禁用一个 prompt 顺带把同名 tool 也拦掉。
#[derive(Default)]
struct DisabledItems {
    tools: HashSet<(String, String)>,
    prompts: HashSet<(String, String)>,
    /// 键的第二段是 URI（resource 不改写名字，以 URI 作标识）。
    resources: HashSet<(String, String)>,
}

pub struct AggregationState {
    downstreams: RwLock<HashMap<String, RegisteredServer>>,
    /// 每 Workspace 一份 Revision；整体替换保证快照一致性。
    catalogs: RwLock<Arc<CatalogMap>>,
    /// 每 Workspace 一份禁用集（tool / prompt / resource 三类同构，键均为
    /// `(server_id, original_name)`；resource 的 original_name 是 URI）。
    /// **必须按 Workspace 隔离**：同一条目可在 ws A 禁用、ws B 启用，全局并集
    /// 表达不了这种差异，且并集会被后重建的 Workspace 覆盖、还会残留已删
    /// override 的陈旧项。二次校验按 Session 已 pin 的 Workspace 查本集。
    disabled_items: RwLock<HashMap<String, DisabledItems>>,
    /// 最近一次各 Workspace 的覆盖（自动恢复重建时复用）。
    last_overrides: RwLock<HashMap<String, CatalogOverrides>>,
    /// 各 Workspace 的工具动态发现开关（tools/list 只暴露 meta 工具）。
    /// 由 `rebuild_catalog_scoped` 写入——与 `last_overrides` 同一模式，
    /// 健康监控自动恢复重建时从这里取现值，不会把发现模式静默重置回全量暴露。
    discovery_workspaces: RwLock<HashMap<String, bool>>,
    /// 各 Workspace 最近一次重建的成员集（None = 全量，即 ws_default）。
    /// 同为 `rebuild_catalog_scoped` 写入：自动重建（健康恢复 / 下游
    /// list_changed）若不带上它，自建 Workspace 会被重建成全量成员——
    /// 非成员 Server 的工具泄入本不该见到它们的 Workspace。
    last_members: RwLock<HashMap<String, Option<std::collections::HashSet<String>>>>,
    /// 连接 → Workspace（来自 client_connections.default_workspace_id）。
    connection_workspaces: RwLock<HashMap<String, String>>,
    /// 每连接最近一次请求协商到的协议版本（控制面 Status 暴露，供桌面端
    /// 区分「谁在走 legacy、谁已在新协议」）。
    connection_protocols: RwLock<HashMap<String, String>>,
    /// 访问控制 deny-list：连接 → 被拒绝的 Server 集（来自 connection_server_denials）。
    /// 无 key/无成员 = 允许；请求时实时查询，对已 pin 的 Session 立即生效。
    connection_denied: RwLock<HashMap<String, HashSet<String>>>,
    /// 各 Workspace 的用户自定义使用准则（来自 workspaces.usage_guidance，
    /// 注入 instructions；空串 = 未配置）。load_downstreams 水合。
    workspace_guidance: RwLock<HashMap<String, String>>,
    /// 连接 → 工具暴露模式（来自 client_connections.tool_exposure_mode）。
    connection_exposures: RwLock<HashMap<String, ExposureMode>>,
    /// 网关模式下经 tool_describe 升格的工具（连接 → exposed_name 集）。
    /// 纯内存态：重启即清，模型重新 describe 一次即可恢复；catalog 重建后
    /// 失效的名字由 list_tools 读取时对当前 Revision 惰性过滤，不在此清理。
    promoted_tools: RwLock<HashMap<String, HashSet<String>>>,
    /// 全局出网代理（ADR-017）。挂在 state 上而非每次从库里读：健康监控循环
    /// 只持有 state、拿不到连接池，而它恰恰是重连 HTTP 下游的地方。
    /// 由 `load_downstreams` 在启动与每次 ReloadServers 时刷新。
    proxy: RwLock<ProxyConfig>,
    /// 下游 `*/list_changed` 通知驱动的 Catalog 重建触发端
    /// （由 [`Self::spawn_catalog_refresher`] 装配；未装配时通知被忽略）。
    rebuild_tx: RwLock<Option<tokio::sync::mpsc::UnboundedSender<()>>>,
    /// 工具列表变更广播，供 `subscriptions/listen` 推 `tools/list_changed`。
    /// 用 broadcast 而非 watch：每个订阅是独立的长驻请求，且丢事件比丢通知
    /// 更可接受——订阅端收到 Lagged 一律当作"变了"处理。
    catalog_changed: tokio::sync::broadcast::Sender<CatalogChange>,
    /// 重建互斥：load_downstreams（DB 事实源）与自动重建（通知/健康恢复，
    /// 用内存现值）必须串行。否则自动重建可能持着**旧** last_overrides 快照
    /// 后完成收尾写入，把用户刚做的工具禁用连同 disabled_tools/last_overrides
    /// 一起回退（禁用工具重新可调），且坏值会一直污染后续自动重建。
    rebuild_mutex: tokio::sync::Mutex<()>,
    /// 远程 MCP 的 OAuth 登录与令牌（ADR-024）。令牌目录默认在 `$MCPANE_HOME/mcp-oauth`。
    oauth: RwLock<Arc<OAuthLogins>>,
    pub logs: Option<LogRepo>,
}

fn read_lock<'a, T>(lock: &'a RwLock<T>) -> std::sync::RwLockReadGuard<'a, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write_lock<'a, T>(lock: &'a RwLock<T>) -> std::sync::RwLockWriteGuard<'a, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl AggregationState {
    pub fn new(logs: Option<LogRepo>) -> Self {
        Self {
            downstreams: RwLock::new(HashMap::new()),
            catalogs: RwLock::new(Arc::new(HashMap::from([(
                DEFAULT_WORKSPACE.to_owned(),
                Arc::new(CatalogRevision::new(
                    "rev_empty".into(),
                    DEFAULT_WORKSPACE.into(),
                    Vec::new(),
                )),
            )]))),
            disabled_items: RwLock::new(HashMap::new()),
            last_overrides: RwLock::new(HashMap::new()),
            discovery_workspaces: RwLock::new(HashMap::new()),
            last_members: RwLock::new(HashMap::new()),
            connection_workspaces: RwLock::new(HashMap::new()),
            connection_protocols: RwLock::new(HashMap::new()),
            connection_denied: RwLock::new(HashMap::new()),
            workspace_guidance: RwLock::new(HashMap::new()),
            connection_exposures: RwLock::new(HashMap::new()),
            promoted_tools: RwLock::new(HashMap::new()),
            proxy: RwLock::new(ProxyConfig::default()),
            rebuild_tx: RwLock::new(None),
            catalog_changed: tokio::sync::broadcast::channel(16).0,
            rebuild_mutex: tokio::sync::Mutex::new(()),
            oauth: RwLock::new(Arc::new(OAuthLogins::new(
                mcpane_hub_protocol::mcpane_home().join("mcp-oauth"),
            ))),
            logs,
        }
    }

    /// OAuth 登录编排与令牌目录。
    pub fn oauth(&self) -> Arc<OAuthLogins> {
        read_lock(&self.oauth).clone()
    }

    /// 改令牌目录（测试用；生产路径始终是默认目录）。
    pub fn set_oauth_dir(&self, dir: std::path::PathBuf) {
        *write_lock(&self.oauth) = Arc::new(OAuthLogins::new(dir));
    }

    /// 当前的全局出网代理配置。
    pub fn proxy(&self) -> ProxyConfig {
        read_lock(&self.proxy).clone()
    }

    /// 换上新的代理配置。只影响此后新建的下游连接——已建立的连接由
    /// `load_downstreams` 整体重连覆盖（桌面端改代理后会发 ReloadServers）。
    pub fn set_proxy(&self, config: ProxyConfig) {
        *write_lock(&self.proxy) = config;
    }

    /// 注册（或替换）一个已连上的下游实例（状态 Ready，清空 last_error）。
    pub fn register_server(
        &self,
        server_id: &str,
        slug: &str,
        spec: SpawnSpec,
        handle: DownstreamHandle,
    ) {
        self.register_runtime(
            server_id,
            slug,
            spec,
            Some(handle),
            ServerStatus::Ready,
            None,
        );
    }

    /// 登记运行时槽位：可无 handle（Starting / Failed 占位）。
    pub fn register_runtime(
        &self,
        server_id: &str,
        slug: &str,
        spec: SpawnSpec,
        handle: Option<DownstreamHandle>,
        status: ServerStatus,
        last_error: Option<String>,
    ) {
        let handle = handle.map(Arc::new);
        // 订阅该实例的 */list_changed 通知：下游工具集变化 → 触发 Catalog 重建。
        // 句柄被替换/注销时发送端 drop，changed() 报错，任务自然退出——
        // 不会给被换下的旧实例留下悬挂订阅。
        if let (Some(handle), Some(rebuild_tx)) =
            (handle.as_ref(), read_lock(&self.rebuild_tx).clone())
        {
            let mut changes = handle.subscribe_changes();
            let slug_for_log = slug.to_owned();
            tokio::spawn(async move {
                while changes.changed().await.is_ok() {
                    tracing::info!(server = %slug_for_log, "收到下游 list_changed，触发 Catalog 重建");
                    if rebuild_tx.send(()).is_err() {
                        break;
                    }
                }
            });
        }
        let old = write_lock(&self.downstreams).insert(
            server_id.to_owned(),
            RegisteredServer {
                server_id: server_id.to_owned(),
                slug: slug.to_owned(),
                spec,
                handle,
                status: Arc::new(RwLock::new(status)),
                last_error: Arc::new(RwLock::new(last_error)),
                last_error_kind: Arc::new(RwLock::new(None)),
                auth_challenge: Arc::new(RwLock::new(None)),
            },
        );
        // 不走 `Arc::try_unwrap` + close：诊断/Catalog 重建等只读路径随时握着
        // 克隆，unwrap 一失败旧连接就要活到最后一个克隆掉落才被 DropGuard 兜底
        // 回收。shutdown 经 cancellation token 发信号，无需独占所有权，替换即关。
        if let Some(old) = old
            && let Some(old_handle) = old.handle
        {
            old_handle.shutdown();
        }
    }

    /// 即将 connect：诊断立刻报 starting；保留旧 handle（若有）供探测窗口过渡。
    pub fn mark_server_starting(&self, server_id: &str, slug: &str, spec: SpawnSpec) {
        if let Some(current) = self.registered(server_id) {
            // 就地改状态，避免先拔 handle 造成工具调用空窗。
            current.set_status(ServerStatus::Starting);
            current.set_last_error(None);
            *write_lock(&current.last_error_kind) = None;
            // spec 可能已变（用户改了 command）；换掉内存里的 spec。
            if let Some(entry) = write_lock(&self.downstreams).get_mut(server_id) {
                entry.spec = spec;
                entry.slug = slug.to_owned();
            }
            return;
        }
        self.register_runtime(server_id, slug, spec, None, ServerStatus::Starting, None);
    }

    /// 连接失败：诊断报 failed + lastError。
    ///
    /// 已有注册时**就地**改状态，保留旧 handle——重连失败不等于旧连接立刻
    /// 不可用（改代理/改 command 起不来时，旧 stdio 进程往往还能接 tools/call）。
    /// 只有从未连上过才登记无 handle 的 Failed 占位。
    pub fn mark_server_failed(
        &self,
        server_id: &str,
        slug: &str,
        spec: SpawnSpec,
        error: impl ToString,
    ) {
        let error = error.to_string();
        if self.registered(server_id).is_some() {
            if let Some(entry) = write_lock(&self.downstreams).get_mut(server_id) {
                entry.spec = spec;
                entry.slug = slug.to_owned();
                entry.set_status(ServerStatus::Failed);
                entry.set_last_error(Some(error));
            }
            return;
        }
        self.register_runtime(
            server_id,
            slug,
            spec,
            None,
            ServerStatus::Failed,
            Some(error),
        );
    }

    /// 连接失败的统一入口：按错误种类落到 `needs-auth`（要登录，不是故障）或 `failed`，
    /// 并记下失败原因代号供界面显示。
    pub fn mark_server_error(
        &self,
        server_id: &str,
        slug: &str,
        spec: SpawnSpec,
        error: &DownstreamError,
    ) {
        let challenge = match error {
            DownstreamError::AuthRequired { challenge } => Some(challenge.clone()),
            _ => None,
        };
        if challenge.is_some() {
            self.mark_server_needs_auth(server_id, slug, spec);
        } else {
            self.mark_server_failed(server_id, slug, spec, error);
        }
        if let Some(entry) = self.registered(server_id) {
            *write_lock(&entry.last_error_kind) = Some(error.failure_kind());
            *write_lock(&entry.auth_challenge) = challenge.flatten();
        }
    }

    fn mark_server_needs_auth(&self, server_id: &str, slug: &str, spec: SpawnSpec) {
        let message = "需要登录授权（OAuth）".to_owned();
        if self.registered(server_id).is_some() {
            if let Some(entry) = write_lock(&self.downstreams).get_mut(server_id) {
                entry.spec = spec;
                entry.slug = slug.to_owned();
                entry.set_status(ServerStatus::NeedsAuth);
                entry.set_last_error(Some(message));
            }
            return;
        }
        self.register_runtime(
            server_id,
            slug,
            spec,
            None,
            ServerStatus::NeedsAuth,
            Some(message),
        );
    }

    /// 注销下游。
    pub fn unregister_server(&self, server_id: &str) {
        if let Some(old) = write_lock(&self.downstreams).remove(server_id)
            && let Some(handle) = old.handle
        {
            handle.shutdown();
        }
    }

    pub fn registered_servers(&self) -> Vec<RegisteredServer> {
        read_lock(&self.downstreams).values().cloned().collect()
    }

    /// 某 server_id 当前注册的运行时视图（`load_downstreams` 据此判定
    /// 「规格未变则不重连」）。
    pub fn registered(&self, server_id: &str) -> Option<RegisteredServer> {
        read_lock(&self.downstreams).get(server_id).cloned()
    }

    /// 取重建互斥（见 `rebuild_mutex` 字段注释）。load_downstreams 与
    /// 自动重建共用；`rebuild_catalog_scoped` 本身不取锁，由调用方持有。
    pub async fn lock_rebuild(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.rebuild_mutex.lock().await
    }

    /// 并行读取全部下游 tools/list（及声明了对应能力者的 prompts/resources），
    /// 重建 Catalog Revision。单个下游失败不影响其他（其条目缺席本次 Revision）。
    pub async fn rebuild_catalog(
        &self,
        workspace_id: &str,
        overrides: &CatalogOverrides,
    ) -> Result<Arc<CatalogRevision>, mcpane_hub_catalog::CatalogError> {
        // 自动恢复等无 DB 上下文的路径：发现开关与成员集都取上次重建写入的
        // 现值——丢掉成员集会把自建 Workspace 重建成全量成员（工具越权可见）。
        let discovery = self.workspace_discovery(workspace_id);
        let members = read_lock(&self.last_members)
            .get(workspace_id)
            .cloned()
            .flatten();
        self.rebuild_catalog_scoped(workspace_id, overrides, members.as_ref(), discovery)
            .await
    }

    /// Workspace 是否处于工具动态发现模式。
    pub fn workspace_discovery(&self, workspace_id: &str) -> bool {
        read_lock(&self.discovery_workspaces)
            .get(workspace_id)
            .copied()
            .unwrap_or(false)
    }

    /// 带成员集的重建：`members` 为 Some 时只纳入集合内的 Server；
    /// `tool_discovery` 为本 Workspace 的发现开关（写入 state 供查询与自动恢复）。
    pub async fn rebuild_catalog_scoped(
        &self,
        workspace_id: &str,
        overrides: &CatalogOverrides,
        members: Option<&std::collections::HashSet<String>>,
        tool_discovery: bool,
    ) -> Result<Arc<CatalogRevision>, mcpane_hub_catalog::CatalogError> {
        let servers: Vec<RegisteredServer> = self
            .registered_servers()
            .into_iter()
            .filter(|s| members.is_none_or(|set| set.contains(&s.server_id)))
            .collect();
        let mut jobs = Vec::new();
        for server in servers {
            jobs.push(tokio::spawn(async move {
                let Some(handle) = server.handle else {
                    // Starting/Failed 占位：Catalog 本轮缺席，等连上后再进。
                    return None;
                };
                let tools = match handle.list_tools().await {
                    Ok(tools) => tools,
                    Err(error) => {
                        tracing::warn!(server_id = %server.server_id, %error, "下游 tools/list 失败，本次 Catalog 缺席");
                        return None;
                    }
                };
                // prompts/resources 按下游 initialize 声明的能力门控：没声明的不发
                // 探测（免 method_not_found 噪声）；声明了但 list 失败的按空处理，
                // 不拖累 tools——与「单个下游失败不影响其他」同一口径。
                let caps = handle.capabilities().unwrap_or_default();
                let mut prompts = Vec::new();
                if caps.prompts.is_some() {
                    match handle.list_prompts().await {
                        Ok(list) => prompts = list,
                        Err(error) => {
                            tracing::warn!(server_id = %server.server_id, %error, "下游 prompts/list 失败，本次缺席")
                        }
                    }
                }
                let mut resources = Vec::new();
                let mut resource_templates = Vec::new();
                if caps.resources.is_some() {
                    match handle.list_resources().await {
                        Ok(list) => resources = list,
                        Err(error) => {
                            tracing::warn!(server_id = %server.server_id, %error, "下游 resources/list 失败，本次缺席")
                        }
                    }
                    match handle.list_resource_templates().await {
                        Ok(list) => resource_templates = list,
                        Err(error) => {
                            tracing::warn!(server_id = %server.server_id, %error, "下游 resources/templates/list 失败，本次缺席")
                        }
                    }
                }
                Some(ServerTools {
                    server_id: server.server_id,
                    server_slug: server.slug,
                    instructions: handle.instructions(),
                    tools,
                    prompts,
                    resources,
                    resource_templates,
                })
            }));
        }
        let mut server_tools: Vec<ServerTools> = Vec::new();
        for job in jobs {
            if let Ok(Some(tools)) = job.await {
                server_tools.push(tools);
            }
        }
        // 稳定排序：Catalog 顺序与注册顺序无关。
        server_tools.sort_by(|a, b| a.server_slug.cmp(&b.server_slug));

        let revision = Arc::new(build_revision(workspace_id, &server_tools, overrides)?);
        // 读-改-写必须在单个写锁临界区内完成，否则并发重建（健康监控 rebuild
        // 不持 RELOAD_LOCK，与 load_downstreams 各自克隆同一 base）会丢更新——
        // 后写者把前者刚插入的另一 Workspace Revision 覆盖回旧值。
        {
            let mut guard = write_lock(&self.catalogs);
            let mut map = (**guard).clone();
            map.insert(workspace_id.to_owned(), revision.clone());
            *guard = Arc::new(map);
        }

        // 本 Workspace 的禁用集整体替换（而非在全局并集上增量改）：既隔离了
        // 跨 Workspace 的相互覆盖，也顺带清掉了已删 override 的陈旧禁用项。
        {
            let disabled_named = |table: &mcpane_hub_catalog::ToolOverrides| {
                table
                    .iter()
                    .filter(|(_, item)| !item.enabled)
                    .map(|((server_id, original), _)| (server_id.clone(), original.clone()))
                    .collect::<HashSet<(String, String)>>()
            };
            let items = DisabledItems {
                tools: disabled_named(&overrides.tools),
                prompts: disabled_named(&overrides.prompts),
                resources: overrides
                    .resources
                    .iter()
                    .filter(|(_, enabled)| !**enabled)
                    .map(|((server_id, uri), _)| (server_id.clone(), uri.clone()))
                    .collect(),
            };
            write_lock(&self.disabled_items).insert(workspace_id.to_owned(), items);
        }
        write_lock(&self.last_overrides).insert(workspace_id.to_owned(), overrides.clone());
        // 三个「上次重建输入」map 统一在成功收尾写入：build_revision 失败时
        // 提前返回，谁都不更新——否则会留下「新成员集 × 旧覆盖」的杂交现值。
        write_lock(&self.discovery_workspaces).insert(workspace_id.to_owned(), tool_discovery);
        write_lock(&self.last_members).insert(workspace_id.to_owned(), members.cloned());

        tracing::info!(
            revision = %revision.id,
            workspace = workspace_id,
            tools = revision.tools.len(),
            prompts = revision.prompts.len(),
            resources = revision.resources.len(),
            "Catalog 已重建"
        );
        // 无订阅者时 send 返回 Err，是正常情况，不是错误。
        let _ = self
            .catalog_changed
            .send(CatalogChange::Rebuilt(workspace_id.to_owned()));
        Ok(revision)
    }

    /// 订阅工具列表变更事件。
    pub fn subscribe_catalog_changes(&self) -> tokio::sync::broadcast::Receiver<CatalogChange> {
        self.catalog_changed.subscribe()
    }

    /// 直接安装一份 Catalog Revision（测试/预热用，落入其 workspace 槽位）。
    pub fn install_catalog(&self, revision: Arc<CatalogRevision>) {
        // 读-改-写在单个写锁内完成，防并发安装丢更新（同 rebuild_catalog_scoped）。
        let mut guard = write_lock(&self.catalogs);
        let mut map = (**guard).clone();
        map.insert(revision.workspace_id.clone(), revision);
        *guard = Arc::new(map);
    }

    /// 整体替换 Catalog 映射（多 Workspace 重建用）。
    pub fn install_catalog_map(&self, map: CatalogMap) {
        *write_lock(&self.catalogs) = Arc::new(map);
    }

    /// 当前全 Workspace Catalog 快照（新 Session 在工厂中 pin 整张映射）。
    pub fn pinned_catalog_map(&self) -> Arc<CatalogMap> {
        read_lock(&self.catalogs).clone()
    }

    /// 默认 Workspace 的 Catalog（单 Workspace 场景/诊断用）。
    pub fn pinned_catalog(&self) -> Arc<CatalogRevision> {
        let map = read_lock(&self.catalogs);
        map.get(DEFAULT_WORKSPACE)
            .or_else(|| map.values().next())
            .cloned()
            .unwrap_or_else(|| {
                Arc::new(CatalogRevision::new(
                    "rev_empty".into(),
                    DEFAULT_WORKSPACE.into(),
                    Vec::new(),
                ))
            })
    }

    /// 更新连接 → Workspace 映射（注册表水合/控制面变更时调用）。
    pub fn set_connection_workspaces(&self, map: HashMap<String, String>) {
        let affected: HashSet<String> = {
            let mut guard = write_lock(&self.connection_workspaces);
            let old = std::mem::replace(&mut *guard, map);
            // 受影响 = 绑定值有变的连接（含新增与移除；移除 = 回落默认，也算变）。
            let current = &*guard;
            old.keys()
                .chain(current.keys())
                .filter(|k| old.get(*k) != current.get(*k))
                .cloned()
                .collect()
        };
        // 改绑即换可见范围：订阅者必须立刻知道，否则只能干等 ttlMs 过期。
        if !affected.is_empty() {
            let _ = self
                .catalog_changed
                .send(CatalogChange::ConnectionsChanged(Arc::new(affected)));
        }
    }

    /// 连接 → Workspace 映射的快照（控制面 Status 回显绑定用）。
    pub fn connection_workspaces_snapshot(&self) -> HashMap<String, String> {
        read_lock(&self.connection_workspaces).clone()
    }

    /// 记录连接最近协商的协议版本（每请求调用，值未变不取写锁）。
    pub fn note_connection_protocol(&self, connection_id: &str, version: &str) {
        if connection_id.is_empty() {
            return;
        }
        if read_lock(&self.connection_protocols)
            .get(connection_id)
            .is_some_and(|v| v == version)
        {
            return;
        }
        write_lock(&self.connection_protocols).insert(connection_id.to_owned(), version.to_owned());
    }

    pub fn connection_protocols_snapshot(&self) -> HashMap<String, String> {
        read_lock(&self.connection_protocols).clone()
    }

    /// 某连接生效的 Workspace（未配置回默认）。
    pub fn workspace_for(&self, client_connection_id: &str) -> String {
        read_lock(&self.connection_workspaces)
            .get(client_connection_id)
            .cloned()
            .unwrap_or_else(|| DEFAULT_WORKSPACE.to_owned())
    }

    /// 二次权限校验：该 Tool 在指定 Workspace 下是否被禁用（即便仍在已 pin 的
    /// Catalog 里）。Workspace 取自调用 Session 已 pin 的工作区（ADR-004）。
    pub fn is_tool_disabled(
        &self,
        workspace_id: &str,
        server_id: &str,
        original_name: &str,
    ) -> bool {
        self.is_item_disabled(workspace_id, server_id, original_name, |d| &d.tools)
    }

    /// 同 [`Self::is_tool_disabled`]，用于 `prompts/get`。
    pub fn is_prompt_disabled(
        &self,
        workspace_id: &str,
        server_id: &str,
        original_name: &str,
    ) -> bool {
        self.is_item_disabled(workspace_id, server_id, original_name, |d| &d.prompts)
    }

    /// 同 [`Self::is_tool_disabled`]，用于 `resources/read`（第三段传 URI）。
    pub fn is_resource_disabled(&self, workspace_id: &str, server_id: &str, uri: &str) -> bool {
        self.is_item_disabled(workspace_id, server_id, uri, |d| &d.resources)
    }

    fn is_item_disabled(
        &self,
        workspace_id: &str,
        server_id: &str,
        name: &str,
        pick: impl Fn(&DisabledItems) -> &HashSet<(String, String)>,
    ) -> bool {
        read_lock(&self.disabled_items)
            .get(workspace_id)
            .is_some_and(|items| pick(items).contains(&(server_id.to_owned(), name.to_owned())))
    }

    /// 全量替换访问控制映射（启动/ReloadServers 水合用）。
    pub fn set_connection_denied_map(&self, map: HashMap<String, HashSet<String>>) {
        let affected: HashSet<String> = {
            let mut guard = write_lock(&self.connection_denied);
            let old = std::mem::replace(&mut *guard, map);
            let current = &*guard;
            old.keys()
                .chain(current.keys())
                .filter(|k| old.get(*k) != current.get(*k))
                .cloned()
                .collect()
        };
        if !affected.is_empty() {
            let _ = self
                .catalog_changed
                .send(CatalogChange::ConnectionsChanged(Arc::new(affected)));
        }
    }

    /// 增量更新单条访问控制（控制面 SetConnectionServerAccess 用）。
    pub fn set_server_denied(&self, client_connection_id: &str, server_id: &str, denied: bool) {
        let changed = {
            let mut map = write_lock(&self.connection_denied);
            if denied {
                map.entry(client_connection_id.to_owned())
                    .or_default()
                    .insert(server_id.to_owned())
            } else if let Some(set) = map.get_mut(client_connection_id) {
                let removed = set.remove(server_id);
                if set.is_empty() {
                    map.remove(client_connection_id);
                }
                removed
            } else {
                false
            }
        };
        // 访问控制启停改变该连接的可见列表——与改绑同类，同样要即时通知。
        if changed {
            let _ = self
                .catalog_changed
                .send(CatalogChange::ConnectionsChanged(Arc::new(HashSet::from(
                    [client_connection_id.to_owned()],
                ))));
        }
    }

    /// 访问控制校验：该连接是否被拒绝访问某 Server（实时查询，不随 Session pin）。
    pub fn is_server_denied(&self, client_connection_id: &str, server_id: &str) -> bool {
        read_lock(&self.connection_denied)
            .get(client_connection_id)
            .is_some_and(|set| set.contains(server_id))
    }

    /// 删除连接时清理其访问控制内存映射（DB 行由 FK 级联删除）。
    pub fn clear_connection_denied(&self, client_connection_id: &str) {
        let removed = write_lock(&self.connection_denied)
            .remove(client_connection_id)
            .is_some();
        if removed {
            let _ = self
                .catalog_changed
                .send(CatalogChange::ConnectionsChanged(Arc::new(HashSet::from(
                    [client_connection_id.to_owned()],
                ))));
        }
    }

    /// 全量替换 Workspace → 使用准则映射（load_downstreams 水合用）。
    /// 只影响 instructions（对新会话/下次 discover 生效），不广播 listChanged：
    /// 三类列表都没变，推工具变更通知反而误导客户端重拉列表。
    pub fn set_workspace_guidance(&self, map: HashMap<String, String>) {
        *write_lock(&self.workspace_guidance) = map;
    }

    /// 某 Workspace 的使用准则（未配置为空串）。
    pub fn workspace_guidance(&self, workspace_id: &str) -> String {
        read_lock(&self.workspace_guidance)
            .get(workspace_id)
            .cloned()
            .unwrap_or_default()
    }

    /// 全量替换连接 → 暴露模式映射（启动/ReloadServers/控制面变更水合用）。
    /// 模式变化会改变该连接的 tools/list 形态，与改绑同类，即时通知订阅者。
    pub fn set_connection_exposures(&self, map: HashMap<String, ExposureMode>) {
        let affected: HashSet<String> = {
            let mut guard = write_lock(&self.connection_exposures);
            let old = std::mem::replace(&mut *guard, map);
            let current = &*guard;
            old.keys()
                .chain(current.keys())
                .filter(|k| old.get(*k) != current.get(*k))
                .cloned()
                .collect()
        };
        if !affected.is_empty() {
            let _ = self
                .catalog_changed
                .send(CatalogChange::ConnectionsChanged(Arc::new(affected)));
        }
    }

    /// 连接 → 暴露模式映射的快照（控制面 Status 回显用）。
    pub fn connection_exposures_snapshot(&self) -> HashMap<String, ExposureMode> {
        read_lock(&self.connection_exposures).clone()
    }

    /// 某连接配置的暴露模式（未配置回 Auto）。
    pub fn exposure_for(&self, client_connection_id: &str) -> ExposureMode {
        read_lock(&self.connection_exposures)
            .get(client_connection_id)
            .copied()
            .unwrap_or(ExposureMode::Auto)
    }

    /// 记录一次升格；返回是否为新条目（新条目才值得广播 listChanged）。
    /// 超过单连接上限时拒绝并告警——不静默：写日志便于定位异常客户端。
    pub fn promote_tool(&self, client_connection_id: &str, exposed_name: &str) -> bool {
        let newly_added = {
            let mut map = write_lock(&self.promoted_tools);
            let set = map.entry(client_connection_id.to_owned()).or_default();
            if set.contains(exposed_name) {
                false
            } else if set.len() >= MAX_PROMOTED_PER_CONNECTION {
                tracing::warn!(
                    connection = client_connection_id,
                    tool = exposed_name,
                    limit = MAX_PROMOTED_PER_CONNECTION,
                    "升格工具数达单连接上限，跳过（工具仍可经 tool_execute 调用）"
                );
                false
            } else {
                set.insert(exposed_name.to_owned())
            }
        };
        if newly_added {
            let _ = self
                .catalog_changed
                .send(CatalogChange::ConnectionsChanged(Arc::new(HashSet::from(
                    [client_connection_id.to_owned()],
                ))));
        }
        newly_added
    }

    /// 某连接的升格工具名快照（list_tools 网关分支追加用）。
    pub fn promoted_snapshot(&self, client_connection_id: &str) -> HashSet<String> {
        read_lock(&self.promoted_tools)
            .get(client_connection_id)
            .cloned()
            .unwrap_or_default()
    }

    /// 删除连接时清理其升格集合（与 clear_connection_denied 同拍调用）。
    pub fn clear_connection_promotions(&self, client_connection_id: &str) {
        write_lock(&self.promoted_tools).remove(client_connection_id);
    }

    /// 删除连接时清理全部连接级运行时映射（DB 行由 FK 级联删除）。
    /// workspace/exposure 另由 `hydrate_connection_mappings` 从 DB 全量刷新；
    /// protocol 仅存内存，必须在此显式 remove。
    pub fn clear_connection_runtime_maps(&self, client_connection_id: &str) {
        self.clear_connection_denied(client_connection_id);
        self.clear_connection_promotions(client_connection_id);
        write_lock(&self.connection_workspaces).remove(client_connection_id);
        write_lock(&self.connection_exposures).remove(client_connection_id);
        write_lock(&self.connection_protocols).remove(client_connection_id);
    }

    pub fn downstream(&self, server_id: &str) -> Option<Arc<DownstreamHandle>> {
        read_lock(&self.downstreams)
            .get(server_id)
            .and_then(|s| s.handle.clone())
    }

    /// 按最近一次重建时的覆盖/成员集/发现开关现值，重建全部已知 Workspace
    /// 的 Catalog（健康监控自动恢复与下游 list_changed 通知两条路径共用）。
    /// 全程持重建互斥：与 load_downstreams 串行，输入快照不会被并发覆盖回退。
    pub async fn rebuild_all_workspaces(&self) {
        let _guard = self.lock_rebuild().await;
        let per_workspace = read_lock(&self.last_overrides).clone();
        let workspaces: Vec<String> = if per_workspace.is_empty() {
            vec![DEFAULT_WORKSPACE.to_owned()]
        } else {
            per_workspace.keys().cloned().collect()
        };
        for workspace_id in workspaces {
            let overrides = per_workspace
                .get(&workspace_id)
                .cloned()
                .unwrap_or_default();
            if let Err(error) = self.rebuild_catalog(&workspace_id, &overrides).await {
                tracing::error!(%error, workspace_id, "Catalog 自动重建失败");
            }
        }
    }

    /// 只保留给定集合内的 Workspace 状态（catalog / 覆盖 / 成员集 / 发现开关 /
    /// 禁用集）。load_downstreams 每轮按 DB 现存 Workspace 调用：已删 Workspace
    /// 的条目若不清理，自动重建会永远带着幽灵 Workspace 逐个扇出下游 list 请求，
    /// 开销随历史删除量单调增长，幽灵 Catalog 也会一直出现在诊断里。
    pub fn retain_workspaces(&self, keep: &HashSet<String>) {
        {
            let mut guard = write_lock(&self.catalogs);
            let mut map = (**guard).clone();
            map.retain(|id, _| keep.contains(id));
            *guard = Arc::new(map);
        }
        write_lock(&self.last_overrides).retain(|id, _| keep.contains(id));
        write_lock(&self.discovery_workspaces).retain(|id, _| keep.contains(id));
        write_lock(&self.last_members).retain(|id, _| keep.contains(id));
        write_lock(&self.disabled_items).retain(|id, _| keep.contains(id));
        write_lock(&self.workspace_guidance).retain(|id, _| keep.contains(id));
    }

    /// 装配下游通知驱动的 Catalog 自动重建：下游发 `*/list_changed` 后
    /// debounce 聚合（一批连续通知只重建一次），随后按 [`Self::rebuild_all_workspaces`]
    /// 刷新。Session-Pinned 语义不变：活跃会话仍用其 pin 的 Revision，
    /// 新会话立即看到新工具集。必须在装载下游之前调用（register 时才有订阅端）。
    pub fn spawn_catalog_refresher(self: &Arc<Self>) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        *write_lock(&self.rebuild_tx) = Some(tx);
        let state = Arc::downgrade(self);
        tokio::spawn(async move {
            while rx.recv().await.is_some() {
                // debounce：下游批量变更常连发多条通知，聚 500ms 只重建一次。
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                while rx.try_recv().is_ok() {}
                let Some(state) = state.upgrade() else { break };
                state.rebuild_all_workspaces().await;
                tracing::info!("下游 list_changed：Catalog 已自动重建");
            }
        });
    }

    /// 启动健康监控循环：周期探测（MCP ping），连续失败自动重连。
    /// - 探测失败 → Degraded；连续 2 次失败 → 尝试按 spec 重连；
    /// - 重连成功 → 替换实例并 Ready + 重建 Catalog；失败 → Failed（下轮再试）。
    pub fn spawn_health_monitor(self: &Arc<Self>, interval: std::time::Duration) {
        let state = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut fail_counts: HashMap<String, u32> = HashMap::new();
            loop {
                tokio::time::sleep(interval).await;
                let Some(state) = state.upgrade() else { break };
                let mut need_catalog_rebuild = false;

                let servers = state.registered_servers();
                // 清掉已注销下游的计数。不清的话，用户禁用再启用同一个 Server，
                // 它会带着上一轮的失败数回来——首次探测失败即达阈值直接重连，
                // 跳过了 Degraded 这一级缓冲；计数表也会随历史删除量单调增长。
                let live: HashSet<&str> = servers.iter().map(|s| s.server_id.as_str()).collect();
                fail_counts.retain(|id, _| live.contains(id.as_str()));

                // 阶段一：并行探测。此前串行 await——一个僵死下游会拖住整轮。
                // ping 超时 5s（不是 list_tools 的 60s）；探测只读不改状态，
                // 无需重建互斥；重连（阶段二）仍逐个持锁。
                // 无 handle 的占位（首连失败）跳过 ping，直接进入重连。
                let mut probe_tasks = tokio::task::JoinSet::new();
                for (index, server) in servers.iter().enumerate() {
                    // 需要登录的下游没有可探测也无需重连的东西：登录、退出登录或配置变化
                    // 会触发重载，在那之前每轮重连只会白白再撞一次 401。
                    if server.status() == ServerStatus::NeedsAuth {
                        continue;
                    }
                    let server = server.clone();
                    probe_tasks.spawn(async move {
                        let outcome = match server.handle.as_ref() {
                            Some(handle) => handle.probe().await.map_err(|error| error.to_string()),
                            None => Err(server
                                .last_error()
                                .unwrap_or_else(|| "not connected".into())),
                        };
                        (index, outcome, server.handle.is_none())
                    });
                }
                let mut probes: Vec<_> = (0..servers.len()).map(|_| None).collect();
                while let Some(joined) = probe_tasks.join_next().await {
                    // join 失败（探测任务 panic）按「本轮无结论」处理，下轮再探。
                    if let Ok((index, result, no_handle)) = joined {
                        probes[index] = Some((result, no_handle));
                    }
                }

                for (server, probe) in servers.into_iter().zip(probes) {
                    let Some((probe, no_handle)) = probe else {
                        continue;
                    };
                    match probe {
                        Ok(()) => {
                            fail_counts.remove(&server.server_id);
                            server.set_status(ServerStatus::Ready);
                            server.set_last_error(None);
                        }
                        Err(error) => {
                            let fails = fail_counts.entry(server.server_id.clone()).or_insert(0);
                            // 无 handle：没有可探测的实例，不必再缓冲一轮 Degraded。
                            *fails = if no_handle { 2 } else { *fails + 1 };
                            tracing::warn!(server = %server.slug, %error, fails = *fails, "健康探测失败");
                            if *fails < 2 {
                                server.set_status(ServerStatus::Degraded);
                                continue;
                            }
                            // 自动恢复：按 spec 重连。与 load_downstreams 共享同一把
                            // 重建互斥——否则两者可能并发地对同一个 server_id 重连：
                            // 用户在健康监控判定失败的同时改了配置并触发 ReloadServers，
                            // 若 load_downstreams 先用新 spec 装好、健康监控随后才用
                            // 探测开始时刻捕获的旧 spec 重连完，"后写者赢"会把刚生效的
                            // 新配置连接悄悄顶替回旧的。
                            let _guard = state.lock_rebuild().await;
                            // 拿到锁之后必须重新核对：等锁期间 load_downstreams 可能
                            // 已经用别的 spec 重连过这个 server_id——此时不该用我们
                            // 探测开始时刻捕获的旧 spec 覆盖回去，那份新连接才是权威的。
                            let still_stale = state
                                .registered(&server.server_id)
                                .is_some_and(|current| current.spec == server.spec);
                            if !still_stale {
                                tracing::info!(
                                    server = %server.slug,
                                    "等待重建锁期间下游已被其他路径更新，跳过健康监控的重连"
                                );
                                fail_counts.remove(&server.server_id);
                                continue;
                            }
                            server.set_status(ServerStatus::Starting);
                            match DownstreamHandle::connect(
                                &server.spec,
                                std::time::Duration::from_secs(10),
                                &state.proxy(),
                            )
                            .await
                            {
                                Ok(handle) => {
                                    state.register_server(
                                        &server.server_id,
                                        &server.slug,
                                        server.spec.clone(),
                                        handle,
                                    );
                                    fail_counts.remove(&server.server_id);
                                    need_catalog_rebuild = true;
                                    tracing::info!(server = %server.slug, "下游自动恢复成功");
                                }
                                Err(error) => {
                                    state.mark_server_error(
                                        &server.server_id,
                                        &server.slug,
                                        server.spec.clone(),
                                        &error,
                                    );
                                    if error.needs_auth() {
                                        tracing::warn!(server = %server.slug, "下游需要登录授权，暂停自动恢复");
                                    } else {
                                        tracing::error!(server = %server.slug, %error, "下游自动恢复失败，下轮重试");
                                    }
                                }
                            }
                        }
                    }
                }

                if need_catalog_rebuild {
                    state.rebuild_all_workspaces().await;
                }
            }
        });
    }

    /// 诊断摘要（控制面 diagnostics 用）。
    pub fn diagnostics(&self) -> serde_json::Value {
        let servers: BTreeMap<String, serde_json::Value> = read_lock(&self.downstreams)
            .values()
            .map(|s| {
                (
                    s.server_id.clone(),
                    serde_json::json!({
                        "slug": s.slug,
                        "status": s.status(),
                        "crashCount": s.handle.as_ref().map(|h| h.crash_count()).unwrap_or(0),
                        "lastError": s.last_error(),
                        "lastErrorKind": s.last_error_kind(),
                        // 仅远程 HTTP 服务器有「是否已登录」可言。
                        "signedIn": match &s.spec {
                            SpawnSpec::Http { oauth: Some(spec), .. } => {
                                serde_json::json!(spec.signed_in())
                            }
                            _ => serde_json::Value::Null,
                        },
                    }),
                )
            })
            .collect();
        // pinned_catalog 内部也会 read 同一把 catalogs 锁：必须在拿本地读锁
        // **之前**调用。std RwLock 的递归 read 在有写者排队时可能死锁
        // （诊断命令与 Catalog 重建并发是真实会发生的组合）。
        let default = self.pinned_catalog();
        let catalogs = read_lock(&self.catalogs);
        let workspaces: BTreeMap<String, serde_json::Value> = catalogs
            .iter()
            .map(|(ws, rev)| {
                (
                    ws.clone(),
                    serde_json::json!({ "revision": rev.id, "tools": rev.tools.len() }),
                )
            })
            .collect();
        serde_json::json!({
            "downstreams": servers,
            "catalogRevision": default.id,
            "catalogTools": default.tools.len(),
            "workspaces": workspaces,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn clear_connection_runtime_maps_removes_all_connection_state() {
        let state = AggregationState::new(None);
        let id = "conn_test";
        state.set_connection_workspaces(std::collections::HashMap::from([(
            id.to_owned(),
            "ws_default".to_owned(),
        )]));
        state.set_connection_exposures(std::collections::HashMap::from([(
            id.to_owned(),
            ExposureMode::Flat,
        )]));
        state.note_connection_protocol(id, "2026-07-28");
        state.clear_connection_runtime_maps(id);
        assert!(state.connection_workspaces_snapshot().is_empty());
        assert!(state.connection_exposures_snapshot().is_empty());
        assert!(state.connection_protocols_snapshot().is_empty());
    }
}
