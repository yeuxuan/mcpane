//! 下游 MCP 客户端（TASK-402 stdio / TASK-403 Streamable HTTP / SSE legacy）。
//!
//! - stdio：不经 shell 直接 spawn，stderr 独立捕获入 tracing，
//!   进程随句柄关闭清理（kill-on-drop 语义由 rmcp 子进程传输保证）；
//! - HTTP：Bearer Token 从环境变量读取（明文不入配置），支持静态 Header；
//! - SSE：HTTP+SSE legacy 传输（见 [`sse`] 模块），认证/代理语义与 HTTP 一致；
//! - 统一：initialize / list_tools / call_tool + 超时 + 崩溃计数（仅诊断）。
//!
//! 故障恢复策略（ADR 无强制熔断要求）：本层**不做 call 级熔断**。参考同类
//! 聚合网关 mcp-router 的取舍——网关替上游熔断会吞掉本可成功的调用，且下游
//! 恢复由 Hub 的健康监控负责（MCP `ping` 连续失败即按 spec 重连、句柄
//! 换新）。`crash_counter` 仅作诊断遥测（连续失败计数，成功即清零），不阻断调用。

pub mod oauth;
pub mod sse;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use mcpane_net::ProxyConfig;
use rmcp::ServiceExt;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientRequest, GetPromptRequestParams, GetPromptResult,
    PingRequest, Prompt, ReadResourceRequestParams, ReadResourceResult, Resource, ResourceTemplate,
    ServerCapabilities, Tool,
};
use rmcp::service::{ClientServiceExt, RoleClient, RunningService, ServiceError};
use rmcp::transport::TokioChildProcess;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransport;

/// 向下游发 cancelled 通知的限时：通知 .await 等的是真实写入，下游卡死时
/// 不能反过来拖住取消/超时/Drop 路径。
const CANCEL_NOTIFY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum DownstreamError {
    #[error("下游启动失败（{context}）：{message}")]
    Spawn { context: String, message: String },
    #[error("下游调用失败：{0}")]
    Call(String),
    #[error("下游调用超时（{0:?}）")]
    Timeout(Duration),
    #[error("配置错误：{0}")]
    Config(String),
    /// 远程服务器要求 OAuth 授权（HTTP 401）。`challenge` 是 `WWW-Authenticate` 原文，
    /// 登录时据此发现授权服务器。**不是故障**：等用户登录，健康监控不对它重连。
    #[error("需要登录授权（OAuth）")]
    AuthRequired { challenge: Option<String> },
    /// OAuth 过程本身失败（发现、注册、换令牌、刷新被拒…）。
    #[error("OAuth 失败：{0}")]
    Auth(String),
}

impl DownstreamError {
    pub fn needs_auth(&self) -> bool {
        matches!(self, Self::AuthRequired { .. })
    }

    /// 失败原因的稳定代号（诊断返回给界面，界面据此显示具体原因而不是笼统的「失败」）：
    /// `needs_auth` / `command_not_found` / `exited` / `no_response` /
    /// `connection_refused` / `http_<状态码>` / `config` / `other`。
    pub fn failure_kind(&self) -> String {
        match self {
            Self::AuthRequired { .. } => "needs_auth".into(),
            Self::Timeout(_) => "no_response".into(),
            Self::Config(_) => "config".into(),
            Self::Auth(_) => "other".into(),
            Self::Call(message) | Self::Spawn { message, .. } => classify_failure(message),
        }
    }
}

/// 按错误文案归类。文案来自操作系统与 reqwest / rmcp，措辞随平台而异，
/// 所以只认最稳的几个特征，认不出归 `other`（界面照常显示原文）。
fn classify_failure(message: &str) -> String {
    let lower = message.to_lowercase();
    let has = |needle: &str| lower.contains(needle);
    if has("no such file or directory")
        || has("program not found")
        || has("os error 2")
        || has("系统找不到指定的文件")
        || has("command not found")
    {
        return "command_not_found".into();
    }
    if has("connection refused") || has("actively refused") || has("积极拒绝") {
        return "connection_refused".into();
    }
    if let Some(status) = http_status(&lower) {
        return format!("http_{status}");
    }
    if has("connection closed")
        || has("broken pipe")
        || has("unexpected eof")
        || has("exited")
        || has("process exited")
    {
        return "exited".into();
    }
    if has("timed out") || has("timeout") || has("deadline") {
        return "no_response".into();
    }
    "other".into()
}

/// reqwest 的「HTTP status client error (404 Not Found) for url」之类里取状态码。
fn http_status(lower: &str) -> Option<u16> {
    let marker = lower
        .find("http status")
        .or_else(|| lower.find("status code"))?;
    lower[marker..]
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| part.len() == 3)
        .find_map(|part| part.parse::<u16>().ok())
        .filter(|code| (400..600).contains(code))
}

/// 初始化失败是不是服务器回了 401：是则返回 `WWW-Authenticate` 原文（可能为空）。
/// 先按类型下转型取精确值，取不到再认文案，免得 rmcp 换包装形式就漏判。
fn auth_challenge(error: &rmcp::service::ClientInitializeError) -> Option<String> {
    use rmcp::service::ClientInitializeError;
    use rmcp::transport::auth::AuthError;
    use rmcp::transport::streamable_http_client::StreamableHttpError;
    if let ClientInitializeError::TransportError { error, .. } = error {
        match error
            .error
            .downcast_ref::<StreamableHttpError<reqwest::Error>>()
        {
            Some(StreamableHttpError::AuthRequired(required)) => {
                return Some(required.www_authenticate_header.clone());
            }
            // 令牌过期且刷新被授权服务器明确拒绝：只能重新登录。
            // 刷新的瞬时失败（`TokenRefreshFailed`）不算，交给健康监控重试。
            Some(StreamableHttpError::Auth(
                AuthError::AuthorizationRequired
                | AuthError::TokenExpired
                | AuthError::TokenRefreshRejected(_),
            )) => return Some(String::new()),
            _ => {}
        }
    }
    error
        .to_string()
        .to_lowercase()
        .contains("authorization required")
        .then(String::new)
}

/// 下游连接规格（与存储层解耦；hub 二进制负责从 McpServerDef 映射）。
///
/// `PartialEq` 是 Hub 判定「这个下游要不要重连」的依据：ReloadServers 被工具
/// 开关、Workspace 变更、导入等十余处调用点共用，规格没变就不该动已建连接。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnSpec {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: Option<String>,
    },
    Http {
        url: String,
        /// Bearer Token 直填值（优先于 bearer_env）。
        bearer: Option<String>,
        /// Bearer Token 的环境变量名（先查 env map，再查进程环境）。
        bearer_env: Option<String>,
        /// Server 自带的环境变量（供 bearer_env 就地取值，不依赖 Hub 进程环境）。
        env: BTreeMap<String, String>,
        headers: BTreeMap<String, String>,
        /// OAuth 令牌位置（ADR-024）。已登录且没有手配的 Bearer / Authorization 头时启用。
        oauth: Option<oauth::OAuthSpec>,
    },
    /// HTTP+SSE（legacy 传输）；认证/代理语义与 Http 完全一致。
    Sse {
        url: String,
        bearer: Option<String>,
        bearer_env: Option<String>,
        env: BTreeMap<String, String>,
        headers: BTreeMap<String, String>,
    },
}

/// 旧式 elicitation/create 的桥接转发器：入参为下游的确认请求，出参为上游
/// 客户端的应答。
pub type ElicitForward = Arc<
    dyn Fn(
            rmcp::model::ElicitRequestParams,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<rmcp::model::ElicitResult, rmcp::ErrorData>>
                    + Send,
            >,
        > + Send
        + Sync,
>;

/// 下游 progress 通知的转发器（登记方负责把 token 换回上游的再发出）。
pub type ProgressForward = Arc<dyn Fn(rmcp::model::ProgressNotificationParam) + Send + Sync>;

/// 「唯一在飞」桥接登记表（每下游连接一份，elicitation 与 progress 共用）。
///
/// 旧协议的 server→client 消息（elicitation/create、notifications/progress）
/// 在线上与触发它的 tools/call 没有可靠的关联字段（progressToken 还会被
/// rmcp 在发送时覆写为它自己生成的值），只能按「唯一在飞」归属：Hub 代理
/// 某上游调用期间登记转发器，此间下游发来的消息即归属该上游；同一下游
/// 并发多路在飞时归属不可判定，放弃转发（宁可丢弃，不可发给错的人）。
pub struct FlightRelay<F> {
    inner: Arc<std::sync::Mutex<FlightInner<F>>>,
}

// 手写 Clone/Default：derive 会给 F 加不必要的约束（F 在 Arc 里）。
impl<F> Clone for FlightRelay<F> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<F> Default for FlightRelay<F> {
    fn default() -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(FlightInner {
                next_id: 0,
                slots: Vec::new(),
            })),
        }
    }
}

struct FlightInner<F> {
    next_id: u64,
    slots: Vec<(u64, F)>,
}

impl<F: Clone> FlightRelay<F> {
    /// 登记一路在飞调用的转发器；guard 落地即注销。
    pub fn register(&self, forward: F) -> FlightRelayGuard<F> {
        let mut inner = self.lock();
        inner.next_id += 1;
        let id = inner.next_id;
        inner.slots.push((id, forward));
        FlightRelayGuard {
            relay: self.clone(),
            id,
        }
    }

    /// 唯一在飞时返回其转发器；空/歧义时返回在飞路数（供日志）。
    fn claim(&self) -> Result<F, usize> {
        let inner = self.lock();
        match inner.slots.as_slice() {
            [(_, forward)] => Ok(forward.clone()),
            slots => Err(slots.len()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FlightInner<F>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// [`FlightRelay::register`] 的注销句柄。
pub struct FlightRelayGuard<F> {
    relay: FlightRelay<F>,
    id: u64,
}

impl<F> Drop for FlightRelayGuard<F> {
    fn drop(&mut self) {
        self.relay
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .slots
            .retain(|(id, _)| *id != self.id);
    }
}

pub type ElicitationRelay = FlightRelay<ElicitForward>;
pub type ElicitationRelayGuard = FlightRelayGuard<ElicitForward>;
pub type ProgressRelay = FlightRelay<ProgressForward>;
pub type ProgressRelayGuard = FlightRelayGuard<ProgressForward>;

/// 下游客户端 handler：接收 `*/list_changed` 类通知并累加变更计数，
/// 供上层（Hub）订阅后触发 Catalog 自动重建；另承接旧式 elicitation/create
/// 的桥接（见 [`ElicitationRelay`]）与 progress 转发（见 [`ProgressRelay`]）。
#[derive(Clone)]
struct DownstreamClient {
    changes: tokio::sync::watch::Sender<u64>,
    elicitations: ElicitationRelay,
    progresses: ProgressRelay,
}

impl DownstreamClient {
    fn bump(&self) {
        self.changes.send_modify(|n| *n += 1);
    }
}

impl rmcp::ClientHandler for DownstreamClient {
    /// 声明 elicitation 能力：规范要求服务器只对声明了的客户端发确认请求，
    /// 不声明则循规的下游永远不发，桥接永无用武之地。空对象即 2025-06-18
    /// 的 form 形态声明。
    fn get_info(&self) -> rmcp::model::ClientInfo {
        let mut info = rmcp::model::ClientInfo::default();
        info.client_info.name = "mcpane-hub".to_owned();
        info.client_info.version = env!("CARGO_PKG_VERSION").to_owned();
        info.capabilities.elicitation = Some(rmcp::model::ElicitationCapability::default());
        info
    }

    async fn create_elicitation(
        &self,
        request: rmcp::model::ElicitRequestParams,
        _context: rmcp::service::RequestContext<RoleClient>,
    ) -> Result<rmcp::model::ElicitResult, rmcp::ErrorData> {
        match self.elicitations.claim() {
            Ok(forward) => forward(request).await,
            Err(in_flight) => {
                // 0 路（上游走新协议或未声明能力，无人可接）与 ≥2 路（并发
                // 歧义）成因完全不同，日志分开，排查时别把前者误读成并发冲突。
                if in_flight == 0 {
                    tracing::warn!("下游 elicitation/create 无上游登记转发，按拒绝处理");
                } else {
                    tracing::warn!(
                        in_flight,
                        "下游 elicitation/create 多路在飞归属不可判定，按拒绝处理"
                    );
                }
                Ok(rmcp::model::ElicitResult::new(
                    rmcp::model::ElicitationAction::Decline,
                ))
            }
        }
    }

    async fn on_progress(
        &self,
        params: rmcp::model::ProgressNotificationParam,
        _context: rmcp::service::NotificationContext<RoleClient>,
    ) {
        // 空登记（无上游要进度）是常态，静默；歧义才值得留痕。
        match self.progresses.claim() {
            Ok(forward) => forward(params),
            Err(in_flight) if in_flight > 1 => {
                tracing::debug!(in_flight, "下游 progress 无法归属唯一上游调用，丢弃");
            }
            Err(_) => {}
        }
    }

    async fn on_tool_list_changed(&self, _context: rmcp::service::NotificationContext<RoleClient>) {
        self.bump();
    }
    async fn on_prompt_list_changed(
        &self,
        _context: rmcp::service::NotificationContext<RoleClient>,
    ) {
        self.bump();
    }
    async fn on_resource_list_changed(
        &self,
        _context: rmcp::service::NotificationContext<RoleClient>,
    ) {
        self.bump();
    }
}

/// 单个下游实例句柄。
pub struct DownstreamHandle {
    service: RunningService<RoleClient, DownstreamClient>,
    /// 下游 `*/list_changed` 通知的变更计数（watch 订阅端见 [`Self::subscribe_changes`]）。
    changes: tokio::sync::watch::Receiver<u64>,
    /// 执行类调用超时（call_tool / read_resource / get_prompt）。默认放宽到
    /// 5 分钟，可经 `MCPANE_DOWNSTREAM_CALL_TIMEOUT_SECS` 调整——长任务工具
    /// （大型构建、深度检索）不应被短超时误杀。
    pub call_timeout: Duration,
    /// Catalog 重建用的 list_* 超时。固定 60s：不能跟着 call_timeout 一起
    /// 放宽，否则重建一个僵死下游要等 5 分钟。健康探测走独立的
    /// [`Self::health_timeout`]，不共用本值。
    pub probe_timeout: Duration,
    /// 健康探测（MCP `ping`）超时。固定 5s：只需确认对端还活着，不必拉全量
    /// 工具表；僵死下游最多卡住一轮 5s，而不是 list_tools 的 60s。
    pub health_timeout: Duration,
    crash_counter: Arc<AtomicU32>,
    elicitations: ElicitationRelay,
    progresses: ProgressRelay,
}

/// 执行类调用的默认超时（秒），可经环境变量覆盖。
fn default_call_timeout() -> Duration {
    let secs = std::env::var("MCPANE_DOWNSTREAM_CALL_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(300);
    Duration::from_secs(secs)
}

// 探测次序与客户端安装探测共用一份（见该常量的文档注释）：两边各存一份会漂移。
use mcpane_hub_protocol::cli_path::WINDOWS_EXECUTABLE_EXTS;

/// 在 Windows 上把命令名补成真实存在的那个文件。
///
/// 起因：Rust 的 `Command` 在 PATH 里搜索时**只补 `.exe`**（std `resolve_exe`：
/// 命令名不含 `.` 时 `set_extension("exe")`，不认 PATHEXT）。而 npm 全局装出来的
/// bin 全是 `.cmd` 垫片——`npx` 首当其冲，于是 Windows 上所有 npx 类 MCP Server
/// 直接报 `program not found`。
///
/// **为什么按 PATH 实探而不是列一张「这些要补 .cmd」的表**：那张表既补不全也会
/// 补错。补不全——`tsx`、各家 `mcp-server-*` 等任何 npm 全局 bin 都是 `.cmd`；
/// 补错——同一个命令的分发方式并不唯一（pnpm 走 standalone 装出的是 `pnpm.exe`，
/// 走 npm/corepack 才是 `pnpm.cmd`；Volta 的垫片把 `npx` 也做成 `.exe`），写死
/// `.cmd` 会把这些**本来正常**的用户改坏。实探则两边都不会错。
///
/// **只修名字解析，不自己拼 `cmd /c`**：扩展名写明后 std 会认出批处理，自动用
/// cmd.exe 包裹并按 `make_bat_command_line` 做安全引号（CVE-2024-24576
/// "BatBadBut" 的修复）。自己拼 `cmd /c` 等于把 std 已经解决的参数注入问题
/// 重新引回来。
///
/// 找不到时原样返回，让 std 去报它自己的错——这里不负责诊断命令是否存在。
fn resolve_stdio_command(
    command: &str,
    windows: bool,
    path_var: Option<&std::ffi::OsStr>,
) -> String {
    if !windows {
        return command.to_owned();
    }
    resolve_windows_command(command, path_var, |path| {
        // 存在且非空：WindowsApps 下的 Store 别名垫片是 0 字节，is_file() 却认它，
        // 选中后执行弹的是 Store 安装页（uvx/python 类 Server 首当其冲）。
        path.metadata().is_ok_and(|m| m.is_file() && m.len() > 0)
    })
}

/// [`resolve_stdio_command`] 的可测内核：PATH 与「文件是否存在」都由外部注入，
/// 好让这段分派逻辑在任意宿主上单测（同 envsync 的 HostOs 做法）。
fn resolve_windows_command(
    command: &str,
    path_var: Option<&std::ffi::OsStr>,
    exists: impl Fn(&std::path::Path) -> bool,
) -> String {
    // 已经写明扩展名的不动：用户明确指定了什么就用什么。
    if std::path::Path::new(command).extension().is_some() {
        return command.to_owned();
    }

    // 带路径分隔符的不查 PATH，就地按扩展名试。
    if command.contains('/') || command.contains('\\') {
        return match found_ext(command, &exists) {
            Some(ext) => format!("{command}.{ext}"),
            None => command.to_owned(),
        };
    }

    let Some(path_var) = path_var else {
        return command.to_owned();
    };
    for dir in std::env::split_paths(path_var) {
        let Some(ext) = found_ext(&dir.join(command).to_string_lossy(), &exists) else {
            continue;
        };
        // `.exe` 原样返回：std 本来就会补 `.exe`，写不写结果一样，
        // 保持原样能少一处行为差异。只有批处理才必须把扩展名写明。
        return if ext == "exe" {
            command.to_owned()
        } else {
            format!("{command}.{ext}")
        };
    }
    command.to_owned()
}

/// 按 [`WINDOWS_EXECUTABLE_EXTS`] 的次序找出第一个存在的文件，返回其扩展名。
fn found_ext(stem: &str, exists: &impl Fn(&std::path::Path) -> bool) -> Option<&'static str> {
    WINDOWS_EXECUTABLE_EXTS
        .iter()
        .find(|ext| exists(std::path::Path::new(&format!("{stem}.{ext}"))))
        .copied()
}

impl DownstreamHandle {
    /// 连接并完成 initialize 握手。
    ///
    /// `proxy` 是**全局**出网代理（ADR-017），故走参数而非塞进 per-server 的
    /// `SpawnSpec`——放进 spec 等于宣称每个 server 可以有自己的代理，是建模撒谎。
    /// 本机与内网的下游（`http://127.0.0.1:*` 之类）会被自动豁免，不受配置影响。
    pub async fn connect(
        spec: &SpawnSpec,
        startup_timeout: Duration,
        proxy: &ProxyConfig,
    ) -> Result<Self, DownstreamError> {
        let (changes_tx, changes_rx) = tokio::sync::watch::channel(0u64);
        let elicitations = ElicitationRelay::default();
        let progresses = ProgressRelay::default();
        let handler_elicitations = elicitations.clone();
        let handler_progresses = progresses.clone();
        let handler = move || DownstreamClient {
            changes: changes_tx.clone(),
            elicitations: handler_elicitations.clone(),
            progresses: handler_progresses.clone(),
        };
        // Auto：先用 server/discover 探 2026-07-28，只有当对端自证是 legacy 时
        // 才回退 initialize 握手。一套代码吃两代上游（ADR-019 / TASK-804）。
        // 仅用于 stdio 与 Streamable HTTP——SSE 是 2025-03-26 起就弃用的传输，
        // 新协议根本不定义它，对它探测只会白费一个来回。
        let modern_lifecycle = || rmcp::service::ClientLifecycleMode::Auto {
            preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
            legacy_version: Some(rmcp::model::ProtocolVersion::V_2025_11_25),
        };
        let connect_future = async {
            // 按引用捕获：回退重试要求 handler 工厂可被多次调用（&F: Fn 且 Copy）。
            let handler = &handler;
            match spec {
                SpawnSpec::Stdio {
                    command,
                    args,
                    env,
                    cwd,
                } => {
                    // 整个 spawn+握手包成可重试单元：回退 legacy 时进程已随
                    // 失败退出，必须整体重来。
                    let spawn_and_serve = |lifecycle: rmcp::service::ClientLifecycleMode| async move {
                        // GUI/sidecar 薄 PATH：解析与子进程都用补齐后的 PATH，
                        // 否则 Dock 启动时 `npx`/`uvx` 直接 program not found。
                        let path_for_child =
                            mcpane_hub_protocol::cli_path::augmented_path_from_env();
                        // 不经 shell：直接构造命令。
                        let mut cmd = tokio::process::Command::new(resolve_stdio_command(
                            command,
                            cfg!(windows),
                            Some(path_for_child.as_os_str()),
                        ));
                        cmd.args(args);
                        // pnpm 会向下游进程注入 npm 不认识的配置键，触发 npm warn；
                        // 这里移除它，不影响 npx 实际行为。
                        const NPM_NOISE_VAR: &str = "npm_config_manage_package_manager_versions";
                        cmd.env_remove(NPM_NOISE_VAR);
                        // 先写补齐 PATH，再用 per-server env 覆盖（用户显式 PATH 优先）。
                        cmd.env("PATH", &path_for_child);
                        for (key, value) in env {
                            if key == NPM_NOISE_VAR {
                                continue;
                            }
                            cmd.env(key, value);
                        }
                        if let Some(cwd) = cwd {
                            cmd.current_dir(cwd);
                        }

                        // npx/uvx 经 cmd.exe 执行，真正干活的 node.exe 是孙进程，
                        // `Child::kill()` 只杀得掉 cmd.exe 那层壳，孙进程会变孤儿占着端口；
                        // 纳入 Job Object 后 rmcp 现有的 kill 会连带杀掉整棵树。
                        // KillOnDrop 才会给 Job 打上 KILL_ON_JOB_CLOSE：Hub 被
                        // TerminateProcess / process::exit 时不跑 Drop，全靠内核关
                        // Job 句柄带走子树。CREATE_NO_WINDOW 消掉无控制台 sidecar
                        // 拉起子进程时的黑框闪烁。
                        // 顺序有要求：CreationFlags 必须先 wrap，否则 JobObject 读不到标志。
                        #[cfg(windows)]
                        let cmd = {
                            use process_wrap::tokio::{
                                CommandWrap, CreationFlags, JobObject, KillOnDrop,
                            };
                            use windows::Win32::System::Threading::CREATE_NO_WINDOW;

                            let mut wrapped = CommandWrap::from(cmd);
                            wrapped.wrap(CreationFlags(CREATE_NO_WINDOW));
                            wrapped.wrap(JobObject);
                            wrapped.wrap(KillOnDrop);
                            wrapped
                        };

                        let (transport, stderr) = TokioChildProcess::builder(cmd)
                            .stderr(std::process::Stdio::piped())
                            .spawn()
                            .map_err(|e| DownstreamError::Spawn {
                                context: format!("spawn {command}"),
                                message: e.to_string(),
                            })?;
                        // stderr 独立捕获：逐行入 tracing。边读边截断——
                        // next_line 会先把整行缓冲进内存，异常进程输出无换行的
                        // 超长内容时会被动膨胀，这里保证单行缓冲不超过上限。
                        if let Some(stderr) = stderr {
                            let command = command.clone();
                            tokio::spawn(async move {
                                use tokio::io::AsyncReadExt;
                                const MAX_LINE: usize = 2000;
                                let mut stderr = stderr;
                                let mut buf = [0u8; 4096];
                                let mut line: Vec<u8> = Vec::new();
                                loop {
                                    let n = match stderr.read(&mut buf).await {
                                        Ok(0) | Err(_) => break,
                                        Ok(n) => n,
                                    };
                                    for &byte in &buf[..n] {
                                        if byte == b'\n' {
                                            if !line.is_empty() {
                                                let text = String::from_utf8_lossy(&line);
                                                tracing::warn!(downstream = %command, "stderr: {text}");
                                            }
                                            line.clear();
                                        } else if line.len() < MAX_LINE {
                                            line.push(byte);
                                        }
                                        // 超过 MAX_LINE 的行尾部分直接丢弃。
                                    }
                                }
                                if !line.is_empty() {
                                    let text = String::from_utf8_lossy(&line);
                                    tracing::warn!(downstream = %command, "stderr: {text}");
                                }
                            });
                        }
                        Ok(handler().serve_with_lifecycle(transport, lifecycle).await)
                    };
                    match spawn_and_serve(modern_lifecycle()).await? {
                        Ok(service) => Ok(service),
                        // 新协议探测失败一律回退 legacy 重试：现实中的拒绝方式
                        // 无法穷举（-32600、直接断流、404……），按错误类别设
                        // 允许清单只会漏——真连不上的服务器，重试一次也只是
                        // 快速失败，代价可忽略。两轮都败以 legacy 错误为主报告
                        // （存量服务器语境下它才是可诊断的那个），附探测错误。
                        Err(probe_error) => {
                            tracing::warn!(downstream = %command, %probe_error,
                                "2026-07-28 探测失败，回退 legacy initialize 重试");
                            spawn_and_serve(rmcp::service::ClientLifecycleMode::Initialize)
                                .await?
                                .map_err(|e| DownstreamError::Spawn {
                                    context: "initialize (stdio)".into(),
                                    message: format!("{e}（新协议探测亦失败：{probe_error}）"),
                                })
                        }
                    }
                }
                SpawnSpec::Http {
                    url,
                    bearer,
                    bearer_env,
                    env,
                    headers,
                    oauth,
                } => {
                    let client = build_http_client(
                        url,
                        bearer.as_deref(),
                        bearer_env.as_deref(),
                        env,
                        headers,
                        proxy,
                    )?;
                    // 用户自己配了 Bearer / Authorization 头就以它为准，不再走 OAuth。
                    let static_auth = bearer.is_some()
                        || bearer_env.is_some()
                        || headers
                            .keys()
                            .any(|name| name.eq_ignore_ascii_case("authorization"));
                    let authorized = match oauth {
                        Some(spec) if !static_auth => {
                            oauth::authorized_client(spec, url, client.clone()).await?
                        }
                        _ => None,
                    };
                    let served = match authorized {
                        Some(auth_client) => {
                            serve_streamable_http(auth_client, url, handler, &modern_lifecycle)
                                .await
                        }
                        None => {
                            serve_streamable_http(client, url, handler, &modern_lifecycle).await
                        }
                    };
                    match served {
                        // 手配了认证信息仍被 401：是那份凭据不对，登录帮不上忙。
                        Err(DownstreamError::AuthRequired { .. }) if static_auth => {
                            Err(DownstreamError::Spawn {
                                context: "initialize (http)".into(),
                                message: "服务器拒绝了已配置的认证信息（HTTP 401）".into(),
                            })
                        }
                        other => other,
                    }
                }
                SpawnSpec::Sse {
                    url,
                    bearer,
                    bearer_env,
                    env,
                    headers,
                } => {
                    let client = build_http_client(
                        url,
                        bearer.as_deref(),
                        bearer_env.as_deref(),
                        env,
                        headers,
                        proxy,
                    )?;
                    let transport = sse::SseClientTransport::start_with_client(
                        client,
                        sse::SseClientConfig {
                            sse_endpoint: url.clone().into(),
                            ..Default::default()
                        },
                    )
                    .await
                    .map_err(|e| DownstreamError::Spawn {
                        context: "connect (sse)".into(),
                        message: e.to_string(),
                    })?;
                    handler()
                        .serve(transport)
                        .await
                        .map_err(|e| DownstreamError::Spawn {
                            context: "initialize (sse)".into(),
                            message: e.to_string(),
                        })
                }
            }
        };

        let service = tokio::time::timeout(startup_timeout, connect_future)
            .await
            .map_err(|_| DownstreamError::Timeout(startup_timeout))??;

        Ok(Self {
            service,
            changes: changes_rx,
            call_timeout: default_call_timeout(),
            probe_timeout: Duration::from_secs(60),
            health_timeout: Duration::from_secs(5),
            crash_counter: Arc::new(AtomicU32::new(0)),
            elicitations,
            progresses,
        })
    }

    /// 旧式 elicitation 桥接登记表（Hub 在代理调用期间登记上游转发器）。
    pub fn elicitation_relay(&self) -> ElicitationRelay {
        self.elicitations.clone()
    }

    /// progress 转发登记表（Hub 为带进度诉求的调用分配下游 token）。
    pub fn progress_relay(&self) -> ProgressRelay {
        self.progresses.clone()
    }

    /// 订阅下游 `*/list_changed` 通知的变更计数。句柄被替换/关闭后
    /// 发送端随之 drop，`changed()` 返回 Err，订阅任务可据此自然退出。
    pub fn subscribe_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changes.clone()
    }

    pub async fn list_tools(&self) -> Result<Vec<Tool>, DownstreamError> {
        tokio::time::timeout(self.probe_timeout, self.service.list_all_tools())
            .await
            .map_err(|_| DownstreamError::Timeout(self.probe_timeout))?
            .map_err(|e| DownstreamError::Call(e.to_string()))
    }

    /// 健康探测：发 MCP `ping`，不拉 tools/list。
    ///
    /// 对端回了 JSON-RPC（含 `ping` 的 `-32601`）即视为活着——`2026-07-28`
    /// 把 ping 划进 legacy lifecycle，rmcp 现代服务端会 method_not_found，
    /// 但那仍然是一次成功的往返。超时 / 传输断开才算挂。
    pub async fn probe(&self) -> Result<(), DownstreamError> {
        let result = tokio::time::timeout(
            self.health_timeout,
            self.service
                .send_request(ClientRequest::PingRequest(PingRequest::default())),
        )
        .await
        .map_err(|_| DownstreamError::Timeout(self.health_timeout))?;
        ping_roundtrip_alive(result)
    }

    /// 下游 initialize 时声明的 capabilities（连接后即有；None 仅出现在握手前）。
    ///
    /// 聚合层用它做能力门控：不支持 resources/prompts 的下游不发对应探测，
    /// 免得每轮重建都攒一堆 method_not_found 噪声。
    pub fn capabilities(&self) -> Option<ServerCapabilities> {
        self.service
            .peer_info()
            .map(|info| info.capabilities.clone())
    }

    /// 下游 initialize 返回的自述（instructions）。聚合层把它拼进 Hub 对
    /// 上游的 instructions——客户端通常将这段话注入模型上下文。
    pub fn instructions(&self) -> Option<String> {
        self.service
            .peer_info()
            .and_then(|info| info.instructions.clone())
    }

    pub async fn list_resources(&self) -> Result<Vec<Resource>, DownstreamError> {
        tokio::time::timeout(self.probe_timeout, self.service.list_all_resources())
            .await
            .map_err(|_| DownstreamError::Timeout(self.probe_timeout))?
            .map_err(|e| DownstreamError::Call(e.to_string()))
    }

    pub async fn list_resource_templates(&self) -> Result<Vec<ResourceTemplate>, DownstreamError> {
        tokio::time::timeout(
            self.probe_timeout,
            self.service.list_all_resource_templates(),
        )
        .await
        .map_err(|_| DownstreamError::Timeout(self.probe_timeout))?
        .map_err(|e| DownstreamError::Call(e.to_string()))
    }

    pub async fn read_resource(&self, uri: &str) -> Result<ReadResourceResult, DownstreamError> {
        let params = ReadResourceRequestParams::new(uri.to_owned());
        tokio::time::timeout(self.call_timeout, self.service.read_resource(params))
            .await
            .map_err(|_| DownstreamError::Timeout(self.call_timeout))?
            .map_err(|e| DownstreamError::Call(e.to_string()))
    }

    pub async fn list_prompts(&self) -> Result<Vec<Prompt>, DownstreamError> {
        tokio::time::timeout(self.probe_timeout, self.service.list_all_prompts())
            .await
            .map_err(|_| DownstreamError::Timeout(self.probe_timeout))?
            .map_err(|e| DownstreamError::Call(e.to_string()))
    }

    pub async fn get_prompt(
        &self,
        original_name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<GetPromptResult, DownstreamError> {
        let mut params = GetPromptRequestParams::new(original_name.to_owned());
        if let Some(arguments) = arguments {
            params = params.with_arguments(arguments);
        }
        tokio::time::timeout(self.call_timeout, self.service.get_prompt(params))
            .await
            .map_err(|_| DownstreamError::Timeout(self.call_timeout))?
            .map_err(|e| DownstreamError::Call(e.to_string()))
    }

    /// 调用下游 Tool（带超时；超时/传输错误计入崩溃计数，仅供诊断，不阻断调用）。
    pub async fn call_tool(
        &self,
        original_name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<CallToolResult, DownstreamError> {
        let mut params = CallToolRequestParams::new(original_name.to_owned());
        if let Some(arguments) = arguments {
            params = params.with_arguments(arguments);
        }
        let result = tokio::time::timeout(self.call_timeout, self.service.call_tool(params)).await;
        match result {
            Err(_) => {
                self.crash_counter.fetch_add(1, Ordering::SeqCst);
                Err(DownstreamError::Timeout(self.call_timeout))
            }
            Ok(Err(error)) => {
                // 传输层断开视为崩溃；协议级错误（Tool 返回 error）不计。
                // 按 ServiceError 变体判定而非匹配错误文案：rmcp 改措辞不该
                // 让崩溃计数悄悄失灵。
                if matches!(
                    error,
                    ServiceError::TransportSend(_) | ServiceError::TransportClosed
                ) {
                    self.crash_counter.fetch_add(1, Ordering::SeqCst);
                }
                Err(DownstreamError::Call(error.to_string()))
            }
            Ok(Ok(result)) => {
                self.crash_counter.store(0, Ordering::SeqCst);
                Ok(result)
            }
        }
    }

    /// 单轮 `tools/call`：不替调用方驱动 MRTR 往返，原样返回下游的响应。
    ///
    /// Hub 是聚合器，不是终端客户端——下游要求补充输入时，该由**真正的上游
    /// 客户端**回答，Hub 只负责转交（TASK-805）。用 `call_tool` 会让 rmcp 拿
    /// `DownstreamClient` 去应付这些请求，等于 Hub 替用户瞎答。
    /// 执行单轮 tools/call。`cancel` 触发或超时到期时，先向下游发
    /// `notifications/cancelled` 再返回——不再静默 drop future，给下游停止
    /// 白跑的机会（取消是协作式的，不保证真的停）。
    pub async fn call_tool_once(
        &self,
        original_name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
        input_responses: Option<rmcp::model::InputResponses>,
        request_state: Option<String>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<rmcp::model::CallToolResponse, DownstreamError> {
        let mut params = CallToolRequestParams::new(original_name.to_owned());
        if let Some(arguments) = arguments {
            params = params.with_arguments(arguments);
        }
        params.input_responses = input_responses;
        params.request_state = request_state;
        // 走 send_cancellable_request 拿到请求 ID——高层 call_tool_once 不暴露它，
        // 而取消通知必须携带这个 ID 才能对上号。
        let handle = self
            .service
            .send_cancellable_request(
                rmcp::model::ClientRequest::CallToolRequest(rmcp::model::CallToolRequest::new(
                    params,
                )),
                rmcp::service::PeerRequestOptions::no_options(),
            )
            .await
            .map_err(|error| {
                if matches!(
                    error,
                    ServiceError::TransportSend(_) | ServiceError::TransportClosed
                ) {
                    self.crash_counter.fetch_add(1, Ordering::SeqCst);
                }
                DownstreamError::Call(error.to_string())
            })?;
        let request_id = handle.id.clone();
        let notify_cancelled = |reason: &'static str| {
            let request_id = request_id.clone();
            async move {
                let _ = tokio::time::timeout(
                    CANCEL_NOTIFY_TIMEOUT,
                    self.service
                        .notify_cancelled(rmcp::model::CancelledNotificationParam::new(
                            Some(request_id),
                            Some(reason.to_owned()),
                        )),
                )
                .await;
            }
        };
        // 兜底：本 future 被直接 drop（2026-07-28 上游断流即取消时 rmcp 会
        // 终止 Hub 的处理任务，select 的取消分支没机会跑）也要通知下游。
        // 响应出口直接解除武装；取消/超时出口先通知完再解除——中途被 drop
        // 也只是补发一次无害的重复取消，比漏发强。
        struct CancelOnDrop {
            peer: rmcp::service::Peer<RoleClient>,
            request_id: Option<rmcp::model::RequestId>,
        }
        impl Drop for CancelOnDrop {
            fn drop(&mut self) {
                let Some(request_id) = self.request_id.take() else {
                    return;
                };
                let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                    return;
                };
                let peer = self.peer.clone();
                runtime.spawn(async move {
                    // 与正常出口同款限时：下游卡死时别让每个被 drop 的调用
                    // 都留一个永不结束的任务。
                    let _ = tokio::time::timeout(
                        CANCEL_NOTIFY_TIMEOUT,
                        peer.notify_cancelled(rmcp::model::CancelledNotificationParam::new(
                            Some(request_id),
                            Some("request dropped".to_owned()),
                        )),
                    )
                    .await;
                });
            }
        }
        let mut drop_guard = CancelOnDrop {
            peer: self.service.peer().clone(),
            request_id: Some(request_id.clone()),
        };
        tokio::select! {
            result = handle.await_response() => {
                drop_guard.request_id = None;
                match result {
                Err(error) => {
                    if matches!(
                        error,
                        ServiceError::TransportSend(_) | ServiceError::TransportClosed
                    ) {
                        self.crash_counter.fetch_add(1, Ordering::SeqCst);
                    }
                    Err(DownstreamError::Call(error.to_string()))
                }
                // 响应形态判定与 rmcp `Peer::call_tool_once` 保持一致（升级时核对）。
                Ok(rmcp::model::ServerResult::CallToolResult(result)) => {
                    self.crash_counter.store(0, Ordering::SeqCst);
                    Ok(rmcp::model::CallToolResponse::Complete(result))
                }
                Ok(rmcp::model::ServerResult::InputRequiredResult(result)) => {
                    self.crash_counter.store(0, Ordering::SeqCst);
                    Ok(rmcp::model::CallToolResponse::InputRequired(result))
                }
                Ok(rmcp::model::ServerResult::CreateTaskResult(result)) => {
                    self.crash_counter.store(0, Ordering::SeqCst);
                    Ok(rmcp::model::CallToolResponse::Task(result))
                }
                Ok(other) => Err(DownstreamError::Call(format!(
                    "unexpected response: {other:?}"
                ))),
            }},
            () = cancel.cancelled() => {
                notify_cancelled("client cancelled").await;
                drop_guard.request_id = None;
                Err(DownstreamError::Call("cancelled by client".to_owned()))
            }
            () = tokio::time::sleep(self.call_timeout) => {
                notify_cancelled("hub call timeout").await;
                drop_guard.request_id = None;
                self.crash_counter.fetch_add(1, Ordering::SeqCst);
                Err(DownstreamError::Timeout(self.call_timeout))
            }
        }
    }

    /// completion/complete 转发。补全是交互路径，用探测级超时（60s 已远超
    /// 可接受的补全延迟），不跟 call_timeout 一起放宽。
    pub async fn complete(
        &self,
        params: rmcp::model::CompleteRequestParams,
    ) -> Result<rmcp::model::CompleteResult, DownstreamError> {
        tokio::time::timeout(self.probe_timeout, self.service.complete(params))
            .await
            .map_err(|_| DownstreamError::Timeout(self.probe_timeout))?
            .map_err(|e| DownstreamError::Call(e.to_string()))
    }

    pub fn crash_count(&self) -> u32 {
        self.crash_counter.load(Ordering::SeqCst)
    }

    /// 优雅关闭（stdio 进程随之回收）。
    pub async fn close(self) {
        let _ = self.service.cancel().await;
    }

    /// 不要求独占所有权的关停：向 rmcp serve loop 发取消信号，传输层随之
    /// 关闭（stdio 子进程回收）。效果与 [`Self::close`] 一致但不等待收尾，
    /// 供替换/注销时仍有其他 `Arc` 克隆在途（诊断、Catalog 重建）的场景使用。
    pub fn shutdown(&self) {
        self.service.cancellation_token().cancel();
    }
}

/// Streamable HTTP 的握手：先探 2026-07-28，探不通回退 legacy initialize。
/// 服务器回 401 时直接上抛 [`DownstreamError::AuthRequired`]，不回退——换协议版本
/// 解决不了没登录，白白再撞一次 401。`C` 既可以是裸 reqwest 客户端，也可以是挂了令牌的 `AuthClient`。
async fn serve_streamable_http<C>(
    client: C,
    url: &str,
    handler: &impl Fn() -> DownstreamClient,
    modern: &impl Fn() -> rmcp::service::ClientLifecycleMode,
) -> Result<RunningService<RoleClient, DownstreamClient>, DownstreamError>
where
    C: rmcp::transport::streamable_http_client::StreamableHttpClient
        + Clone
        + Send
        + Sync
        + 'static,
{
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
    let serve = |lifecycle: rmcp::service::ClientLifecycleMode| {
        let client = client.clone();
        async move {
            let transport = StreamableHttpClientTransport::with_client(
                client,
                StreamableHttpClientTransportConfig::with_uri(url.to_owned()),
            );
            handler().serve_with_lifecycle(transport, lifecycle).await
        }
    };
    let challenge = |error: &rmcp::service::ClientInitializeError| {
        auth_challenge(error).map(|header| DownstreamError::AuthRequired {
            challenge: Some(header).filter(|h| !h.is_empty()),
        })
    };
    match serve(modern()).await {
        Ok(service) => Ok(service),
        // 一律回退（理由同 stdio 分支）：微软 learn.microsoft.com
        // 对 discover 直接断流（ConnectionClosed），官方 TS SDK
        // 回 -32600——都不是 rmcp Auto 认得的 method_not_found。
        Err(probe_error) => {
            if let Some(required) = challenge(&probe_error) {
                return Err(required);
            }
            tracing::warn!(url, %probe_error,
                "2026-07-28 探测失败，回退 legacy initialize 重试");
            serve(rmcp::service::ClientLifecycleMode::Initialize)
                .await
                .map_err(|e| {
                    challenge(&e).unwrap_or_else(|| DownstreamError::Spawn {
                        context: "initialize (http)".into(),
                        message: format!("{e}（新协议探测亦失败：{probe_error}）"),
                    })
                })
        }
    }
}

/// 构建带默认 Header（静态 headers + Bearer）与全局代理的 reqwest 客户端
/// （Streamable HTTP 与 SSE 两路共用）。
fn build_http_client(
    url: &str,
    bearer: Option<&str>,
    bearer_env: Option<&str>,
    env: &BTreeMap<String, String>,
    headers: &BTreeMap<String, String>,
    proxy: &ProxyConfig,
) -> Result<reqwest::Client, DownstreamError> {
    let mut default_headers = reqwest::header::HeaderMap::new();
    for (name, value) in headers {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| DownstreamError::Config(e.to_string()))?;
        let value = reqwest::header::HeaderValue::from_str(value)
            .map_err(|e| DownstreamError::Config(e.to_string()))?;
        default_headers.insert(name, value);
    }
    if let Some(token) = resolve_bearer(bearer, bearer_env, env)? {
        let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| DownstreamError::Config(e.to_string()))?;
        value.set_sensitive(true);
        default_headers.insert(reqwest::header::AUTHORIZATION, value);
    }
    proxy
        .apply(
            reqwest::Client::builder().default_headers(default_headers),
            url,
        )
        .map_err(|e| DownstreamError::Config(e.to_string()))?
        .build()
        .map_err(|e| DownstreamError::Config(e.to_string()))
}

/// ping 往返：对端回了 JSON-RPC（含 method_not_found）就算活着。
fn ping_roundtrip_alive(
    result: Result<rmcp::model::ServerResult, ServiceError>,
) -> Result<(), DownstreamError> {
    match result {
        Ok(_) | Err(ServiceError::McpError(_)) => Ok(()),
        Err(error) => Err(DownstreamError::Call(error.to_string())),
    }
}

// 注：曾有一个 `backoff_delay` 指数退避函数，生产路径从未接线（健康监控是
// 固定间隔探测，重连持 `lock_rebuild`，锁内 sleep 会堵死 ReloadServers），
// 已删除。若将来要引入退避，正确形态是「跳过 N 轮探测」而不是「在锁内
// sleep N 秒」。

/// Bearer Token 解析：直填值 → bearer_env 查 Server env → 进程环境。
/// 声明了 bearer_env 但两处都取不到 → 报错；两者皆未声明 → Ok(None)（无认证）。
fn resolve_bearer(
    bearer: Option<&str>,
    bearer_env: Option<&str>,
    env: &BTreeMap<String, String>,
) -> Result<Option<String>, DownstreamError> {
    if let Some(token) = bearer.filter(|t| !t.is_empty()) {
        return Ok(Some(token.to_owned()));
    }
    let Some(env_name) = bearer_env.filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    if let Some(token) = env.get(env_name) {
        return Ok(Some(token.clone()));
    }
    std::env::var(env_name).map(Some).map_err(|_| {
        DownstreamError::Config(format!(
            "环境变量 {env_name} 未设置（Server env 与进程环境均无）"
        ))
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn spawn(message: &str) -> DownstreamError {
        DownstreamError::Spawn {
            context: "spawn uvx".into(),
            message: message.into(),
        }
    }

    /// 界面靠这些代号显示「uvx 未找到 / 进程已退出 / 连接被拒绝 / HTTP 404…」，
    /// 而不是一律笼统的「失败」。代号是下游与前端之间的契约。
    #[test]
    fn failures_are_classified_by_their_most_stable_traits() {
        let kind = |e: DownstreamError| e.failure_kind();
        assert_eq!(
            kind(spawn("No such file or directory (os error 2)")),
            "command_not_found"
        );
        assert_eq!(kind(spawn("program not found")), "command_not_found");
        assert_eq!(
            kind(spawn(
                "error sending request: tcp connect error: Connection refused (os error 61)"
            )),
            "connection_refused"
        );
        assert_eq!(
            kind(spawn(
                "HTTP status client error (404 Not Found) for url (https://x.example/mcp)"
            )),
            "http_404"
        );
        assert_eq!(
            kind(spawn(
                "HTTP status server error (503 Service Unavailable) for url (https://x/mcp)"
            )),
            "http_503"
        );
        assert_eq!(
            kind(spawn("connection closed: initialize response")),
            "exited"
        );
        assert_eq!(
            kind(DownstreamError::Timeout(Duration::from_secs(10))),
            "no_response"
        );
        assert_eq!(kind(DownstreamError::Config("bad header".into())), "config");
        assert_eq!(
            kind(DownstreamError::AuthRequired { challenge: None }),
            "needs_auth"
        );
        assert_eq!(kind(spawn("something nobody has seen before")), "other");
        // 三位数但不是 4xx/5xx 的不当状态码。
        assert_eq!(kind(spawn("HTTP status 200 weird")), "other");
    }

    #[test]
    fn bearer_resolution_precedence() {
        let mut env = BTreeMap::new();
        env.insert("TOK".to_owned(), "from-env-map".to_owned());

        // 直填优先于 bearer_env。
        let got = resolve_bearer(Some("direct"), Some("TOK"), &env).unwrap();
        assert_eq!(got.as_deref(), Some("direct"));

        // 空直填视为未填，回落 env map。
        let got = resolve_bearer(Some(""), Some("TOK"), &env).unwrap();
        assert_eq!(got.as_deref(), Some("from-env-map"));

        // 两者皆无 → 无认证。
        assert_eq!(resolve_bearer(None, None, &env).unwrap(), None);

        // 声明了 bearer_env 但取不到 → 报错。
        let missing = resolve_bearer(None, Some("MCPANE_TEST_NO_SUCH_VAR"), &BTreeMap::new());
        assert!(matches!(missing, Err(DownstreamError::Config(_))));
    }

    #[test]
    fn modern_ping_method_not_found_still_alive() {
        let error = ServiceError::McpError(rmcp::model::ErrorData::method_not_found::<
            rmcp::model::PingRequestMethod,
        >());
        assert!(ping_roundtrip_alive(Err(error)).is_ok());
    }

    #[test]
    fn transport_closed_means_dead() {
        assert!(ping_roundtrip_alive(Err(ServiceError::TransportClosed)).is_err());
    }

    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    /// 用 `join_paths` / `Path::join` 构造，故分隔符跟随宿主——这套测试验的是
    /// **探测逻辑**（先 exe 后 cmd、按 PATH 次序、.exe 不改名），不是 Windows 的
    /// 路径语法。生产里这段只在 Windows 跑，那里宿主行为本就是对的。
    fn path_var(dirs: &[&str]) -> OsString {
        std::env::join_paths(dirs).expect("join_paths")
    }

    /// 只有列出的这些文件算存在。
    fn only(files: Vec<PathBuf>) -> impl Fn(&Path) -> bool {
        move |probed| files.iter().any(|f| f == probed)
    }

    /// 绝大多数 MCP Server 都是 `npx -y @scope/pkg` 起的。npm 装出来的 npx 是
    /// `npx.cmd`，而 Rust 的 Command 在 PATH 里只补 `.exe`——不写明扩展名，
    /// Windows 用户一个 npx 类 Server 都装不起来。
    #[test]
    fn npm_shim_resolves_to_cmd() {
        let dir = "/node";
        let exists = only(vec![Path::new(dir).join("npx.cmd")]);
        assert_eq!(
            resolve_windows_command("npx", Some(&path_var(&[dir])), exists),
            "npx.cmd"
        );
    }

    /// 同一个命令的分发方式并不唯一：Volta 的垫片把 npx 装成真 `.exe`。
    /// 写死 `.cmd` 会把这些**本来正常**的用户改坏——这正是不用 allowlist 的理由。
    #[test]
    fn real_exe_shim_is_left_alone() {
        let dir = "/volta";
        let exists = only(vec![Path::new(dir).join("npx.exe")]);
        assert_eq!(
            resolve_windows_command("npx", Some(&path_var(&[dir])), exists),
            "npx"
        );
    }

    /// pnpm 走 standalone 装出的是 `pnpm.exe`，走 npm/corepack 才是 `pnpm.cmd`。
    /// 两种都得对——写死任一种都会坏掉另一半用户。
    #[test]
    fn pnpm_resolves_by_what_is_actually_installed() {
        let dir = "/p";
        let standalone = only(vec![Path::new(dir).join("pnpm.exe")]);
        assert_eq!(
            resolve_windows_command("pnpm", Some(&path_var(&[dir])), standalone),
            "pnpm",
            "standalone 装的是真 exe，不该改名"
        );

        let corepack = only(vec![Path::new(dir).join("pnpm.cmd")]);
        assert_eq!(
            resolve_windows_command("pnpm", Some(&path_var(&[dir])), corepack),
            "pnpm.cmd",
            "corepack 装的是 .cmd，必须写明"
        );
    }

    /// allowlist 补不全的那一类：npm 全局装的任意 bin 都是 `.cmd` 垫片。实探则天然覆盖。
    #[test]
    fn arbitrary_npm_global_bin_also_resolves() {
        let dir = "/node";
        let exists = only(vec![Path::new(dir).join("tsx.cmd")]);
        assert_eq!(
            resolve_windows_command("tsx", Some(&path_var(&[dir])), exists),
            "tsx.cmd"
        );
    }

    /// 同名同在时 `.exe` 优先，与 PATHEXT 的默认次序一致。
    #[test]
    fn exe_wins_over_cmd_in_the_same_dir() {
        let dir = "/both";
        let exists = only(vec![
            Path::new(dir).join("foo.exe"),
            Path::new(dir).join("foo.cmd"),
        ]);
        assert_eq!(
            resolve_windows_command("foo", Some(&path_var(&[dir])), exists),
            "foo"
        );
    }

    /// 跨目录时按 PATH 的先后次序取第一个命中的，不因 .exe 优先而跳到后面的目录。
    #[test]
    fn earlier_path_entry_wins() {
        let exists = only(vec![
            Path::new("/first").join("foo.cmd"),
            Path::new("/second").join("foo.exe"),
        ]);
        assert_eq!(
            resolve_windows_command("foo", Some(&path_var(&["/first", "/second"])), exists),
            "foo.cmd"
        );
    }

    /// 非 Windows 上一个字都不能改：Unix 没有 .cmd 这回事。
    #[test]
    fn nothing_is_rewritten_off_windows() {
        for command in ["npx", "npm", "uvx", "node", "/usr/local/bin/my-server"] {
            assert_eq!(resolve_stdio_command(command, false, None), command);
        }
    }

    /// 用户自己写全了扩展名时不重复追加。
    #[test]
    fn explicit_extension_is_untouched() {
        let exists = only(vec![]);
        assert_eq!(
            resolve_windows_command("npx.cmd", Some(&path_var(&["/node"])), exists),
            "npx.cmd"
        );
    }

    /// 带路径的命令不查 PATH，就地按扩展名试——std 对这类同样只补 `.exe`
    /// （resolve_exe 的非 file-name 分支也 append_suffix），不管的话
    /// 写绝对路径的 npx 照样起不来。
    #[test]
    fn command_with_path_is_probed_in_place() {
        let stem = Path::new("/tools").join("npx");
        let exists = only(vec![Path::new("/tools").join("npx.cmd")]);
        assert_eq!(
            resolve_windows_command(&stem.to_string_lossy(), None, exists),
            format!("{}.cmd", stem.to_string_lossy())
        );
    }

    /// 找不到就原样交回，让 std 报它自己的「program not found」，不在这里替它诊断。
    #[test]
    fn unknown_command_is_passed_through() {
        assert_eq!(
            resolve_windows_command("nope", Some(&path_var(&["/node"])), only(vec![])),
            "nope"
        );
        assert_eq!(
            resolve_windows_command("nope", None, |_: &Path| true),
            "nope",
            "PATH 读不到时也不该乱改"
        );
    }
}

/// Job Object + KillOnDrop：kill 之后子进程必须从 tasklist 消失。
/// Hub 被 TerminateProcess 时不跑 Drop，全靠 KILL_ON_JOB_CLOSE 带走树。
#[cfg(all(test, windows))]
mod windows_job_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::process::Stdio;
    use std::time::{Duration, Instant};

    use process_wrap::tokio::{CommandWrap, JobObject, KillOnDrop};
    use tokio::process::Command;

    fn pid_listed(pid: u32) -> bool {
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .ok();
        let Some(output) = output else {
            return false;
        };
        // 进程不在时 tasklist 打印 INFO 行（含本地化），不是 `"name","pid",...` CSV。
        String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
    }

    #[tokio::test]
    async fn kill_on_drop_job_removes_child_from_tasklist() {
        let mut cmd = Command::new("ping");
        cmd.args(["-n", "30", "127.0.0.1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut wrapped = CommandWrap::from(cmd);
        wrapped.wrap(JobObject);
        wrapped.wrap(KillOnDrop);
        let mut child = wrapped.spawn().expect("spawn ping");
        let pid = child.id().expect("child pid");
        assert!(pid_listed(pid), "刚拉起的 ping 必须出现在 tasklist");

        child.start_kill().expect("start_kill");
        let _ = child.wait().await;

        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if !pid_listed(pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("KillOnDrop + JobObject 之后 pid={pid} 仍在 tasklist");
    }
}
