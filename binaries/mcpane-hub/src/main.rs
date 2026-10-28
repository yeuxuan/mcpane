//! mcpane-hub 入口：数据面（127.0.0.1:24860/mcp）+ 控制面（UDS）。
//!
//! 认证真源为 SQLite `client_connections`（TASK-401）：
//! - 启动时把 dev 环境变量 Token（如有）以固定 ID upsert 入库（只存哈希）；
//! - 内存 TokenRegistry 从库水合，作为数据面同步快速路径；
//! - 控制面 create/rotate/revoke 先写库再重新水合。
//!
//! SQLite 不可用时降级为纯内存 dev 模式（明确告警）。

use std::sync::Arc;

use mcpane_hub::auth::{ClientIdentity, ClientType, TokenRegistry, generate_token};
use mcpane_hub::{DEFAULT_BIND_ADDR, app};
use mcpane_storage::ConnectionRepo;
use tracing_subscriber::EnvFilter;

/// 健康探测默认间隔。提成常量是为了让 `--help` 直接引用它，不靠人肉同步。
const DEFAULT_HEALTH_INTERVAL_SECS: u64 = 30;

/// 命令行意图。
///
/// hub 没有任何配置用的 flag——监听地址、数据目录、会话行为**全部**走环境变量
/// （见 `help()`），故这里只需分辨「跑」「打印点什么就退」「参数不认识」。
/// 为此引入一个 arg parser 不划算。
#[derive(Debug, PartialEq, Eq)]
enum Cli {
    Run,
    Help,
    Version,
    /// stdio ↔ Streamable HTTP 桥（供只支持 stdio 的客户端接入 Hub）。
    Bridge,
    /// 参数不认识。**必须报错而不是忽略**：hub 的配置全在环境变量里，命令行
    /// 传什么都不会生效——静默忽略的话，`--bind 0.0.0.0:9999` 会一声不吭地
    /// 起在默认端口上，而用户以为自己改了。
    Unknown(String),
}

/// 只看第一个参数：认识就照做，不认识就报错——两种情况都不会再去看后面的，
/// 所以没有「遍历参数」这回事。
fn parse_args(args: impl IntoIterator<Item = String>) -> Cli {
    match args.into_iter().next() {
        None => Cli::Run,
        Some(arg) => match arg.as_str() {
            "-h" | "--help" => Cli::Help,
            "-V" | "--version" => Cli::Version,
            "bridge" => Cli::Bridge,
            other => Cli::Unknown(other.to_owned()),
        },
    }
}

/// 默认值一律取自常量而非重打一遍：帮助里的数字与实际行为不符，比没有帮助更糟。
fn help() -> String {
    let session = app::HubOptions::default()
        .session_keep_alive
        .map_or_else(|| "无".to_owned(), |d| format!("{}s", d.as_secs()));
    format!(
        "mcpane-hub {version} — MCPane 的 MCP 聚合端点

用法：
  mcpane-hub                以数据面 + 控制面常驻运行
  mcpane-hub bridge         stdio ↔ Streamable HTTP 桥：把本进程当 stdio
                            MCP Server 拉起，消息原样转发到 Hub 数据面。
                            供只支持 stdio 的客户端（Claude Desktop 等）接入。
                            读 MCPANE_TOKEN（必填）与 MCPANE_HUB_URL
                            （默认 {mcp_url}）
  mcpane-hub --help         显示本帮助
  mcpane-hub --version      显示版本

通常由 MCPane 桌面应用作为 sidecar 拉起，也可独立运行用于调试。
没有配置用的命令行参数——下列环境变量是唯一的配置入口：

  MCPANE_HOME                   数据目录（默认按平台：macOS 为
                                ~/Library/Application Support/MCPane）。
                                mcpane.db 与控制面 socket 都在其下。
  MCPANE_HUB_BIND               数据面监听地址（默认 {bind}）
  MCPANE_HUB_STATEFUL           设为 0 关闭 MCP-Session-Id，回退无状态基线
                                （默认开启）
  MCPANE_SESSION_TIMEOUT_SECS   会话空闲超时（默认 {session}）
  MCPANE_HEALTH_INTERVAL_SECS   下游健康探测间隔（默认 {health}s）
  MCPANE_SUPERVISED             置任意值 = 由桌面应用托管：父进程退出时
                                随之退出（读 stdin 至 EOF；Windows 另有
                                MCPANE_SUPERVISOR_PID/
                                MCPANE_SUPERVISOR_START_FILETIME 兜底轮询）
  MCPANE_CLAUDE_TOKEN           dev 用 Token，启动时以固定 ID 入库（只存哈希）
  MCPANE_CODEX_TOKEN            同上，Codex 侧
  RUST_LOG                      日志级别（默认 info）

数据面：POST {bind}/mcp，需 `Authorization: Bearer <token>`。
Token 由桌面应用「客户端」页签签发；库里只存哈希，明文只在签发时出现一次。
",
        version = env!("CARGO_PKG_VERSION"),
        bind = DEFAULT_BIND_ADDR,
        mcp_url = mcpane_hub_protocol::HUB_MCP_URL,
        session = session,
        health = DEFAULT_HEALTH_INTERVAL_SECS,
    )
}

/// 数据面绑定地址护栏：SECURITY.md 把「永不监听非 loopback」定为不变式。
/// CLI 层已显式拒绝 `--bind`，但 `MCPANE_HUB_BIND` 环境变量才是真正的配置
/// 通道，必须在这里同样设防——一旦绑到非 loopback，带 Bearer 的数据面就会
/// 暴露到 LAN，而 guard 校验的是 `Host` 请求头（可被攻击者伪造），挡不住
/// 网络层暴露。参考 mcp-router：GUI 形态写死 loopback，只有 CLI 放开且要求
/// 非 loopback 必须带 token。MCPane 恒需 token，故这里直接拒绝启动、更严。
fn ensure_loopback_bind(addr: &str) -> anyhow::Result<()> {
    use std::net::ToSocketAddrs;
    let resolved: Vec<_> = addr
        .to_socket_addrs()
        .map_err(|e| anyhow::anyhow!("MCPANE_HUB_BIND=`{addr}` 无法解析为监听地址：{e}"))?
        .collect();
    if resolved.is_empty() {
        anyhow::bail!("MCPANE_HUB_BIND=`{addr}` 解析不到任何地址");
    }
    if let Some(bad) = resolved.iter().find(|a| !a.ip().is_loopback()) {
        anyhow::bail!(
            "MCPANE_HUB_BIND=`{addr}` 解析到非 loopback 地址 {bad}，已拒绝启动：\
             Hub 数据面只允许绑定 127.0.0.1 / ::1（见 SECURITY.md）。"
        );
    }
    Ok(())
}

/// `MCPANE_SUPERVISOR_PID` 兜底轮询用：查询给定 PID 是否仍是**同一个**活着的进程。
///
/// 只查 `STILL_ACTIVE` 不够：PID 是操作系统复用的资源，父进程死后几秒内
/// 就可能被完全无关的新进程捡走（真机复现：这台机器开发工具起停频繁，
/// 复现率不低）——那样会误判"父进程还在"，兜底机制形同虚设。
/// `expected_start_filetime` 是父进程 spawn 时记录的自身创建时间
/// （见桌面端 `windows_own_start_filetime`），跟当前持有该 PID 的进程的
/// 创建时间核对一致才算真的还是原进程；传 `None`（旧版桌面端/取值失败）
/// 时退化为只看 PID 存活，不阻断兜底本身。
#[cfg(windows)]
#[allow(unsafe_code)]
fn windows_process_alive(pid: u32, expected_start_filetime: Option<u64>) -> bool {
    use windows::Win32::Foundation::{CloseHandle, FILETIME, STILL_ACTIVE};
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut code = 0u32;
        let still_active =
            GetExitCodeProcess(handle, &mut code).is_ok() && code == STILL_ACTIVE.0 as u32;
        let same_process = match expected_start_filetime {
            None => true,
            Some(expected) => {
                let mut creation = FILETIME::default();
                let mut exit = FILETIME::default();
                let mut kernel = FILETIME::default();
                let mut user = FILETIME::default();
                GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user).is_ok()
                    && (((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64)
                        == expected
            }
        };
        let _ = CloseHandle(handle);
        still_active && same_process
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 先于一切：--help / --version 必须零副作用，不装 panic hook、不初始化日志、
    // 更不能碰数据目录。参数不认识时以 2 退出（usage error 的惯例）。
    match parse_args(std::env::args().skip(1)) {
        Cli::Run => {}
        Cli::Help => {
            print!("{}", help());
            return Ok(());
        }
        Cli::Version => {
            println!("mcpane-hub {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Cli::Unknown(arg) => {
            eprintln!("mcpane-hub: 无法识别的参数 `{arg}`");
            eprintln!("hub 的配置只走环境变量。用 `mcpane-hub --help` 查看。");
            std::process::exit(2);
        }
        Cli::Bridge => {
            // 桥模式：stdout 是协议通道，日志必须走 stderr；不装 crash.log
            // panic hook、不碰数据目录——桥是宿主客户端拉起的纯网络进程。
            tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
                )
                .with_writer(std::io::stderr)
                .init();
            let Ok(token) = std::env::var(mcpane_hub::bridge::TOKEN_ENV) else {
                eprintln!(
                    "mcpane-hub bridge: 缺少环境变量 {}（在 MCPane「客户端」页签签发）",
                    mcpane_hub::bridge::TOKEN_ENV
                );
                std::process::exit(2);
            };
            let url = std::env::var(mcpane_hub::bridge::URL_ENV)
                .unwrap_or_else(|_| mcpane_hub_protocol::HUB_MCP_URL.to_owned());
            return mcpane_hub::bridge::run(url, token).await;
        }
    }

    // hub 崩溃时 stderr 无人接收（被 desktop 转发后同样丢弃），
    // crash.log 是发布后唯一线索。与桌面端共用同一份文件，靠组件名区分。
    mcpane_hub_protocol::panic_hook::install("hub", env!("CARGO_PKG_VERSION"));

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // 被桌面 App 托管时（MCPANE_SUPERVISED=1，stdin 为管道）：父进程无论
    // 以何种方式退出（Ctrl+C / 崩溃 / SIGKILL），内核都会关闭管道产生 EOF，
    // hub 借此自行退出——杜绝 dev 模式 Ctrl+C 留下孤儿进程。
    // 独立运行（终端/cargo run）不设该变量，不受影响。
    // 看守必须用独立 OS 线程而非 tokio 任务：tokio 的 stdin 读底层是
    // spawn_blocking，Runtime drop 会等它完成——而这个读只有父进程退出关闭
    // 管道才返回。若挂在 tokio 上，SIGTERM 优雅退出会在 Runtime drop 处
    // 与父进程互等（父进程等子退出、子等父关管道），直到被 3s 超时强杀。
    if std::env::var("MCPANE_SUPERVISED").is_ok() {
        std::thread::spawn(|| {
            use std::io::Read;
            let mut stdin = std::io::stdin();
            let mut sink = [0u8; 64];
            loop {
                match stdin.read(&mut sink) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            // 真机复现（2026-08-12）：这里退出前若先 tracing::info! 一行，
            // 触发的进程会在真正强杀测试里卡住到不了 exit。父进程一死，
            // stdout/stderr 这两条管道就再没有读端——桌面端转发日志的
            // 任务连着进程一起没了，这次写入会在内核管道缓冲区上无限期
            // 阻塞（Windows 匿名管道读端全关时写端理应报错，但连着一串
            // Windows Job Object/CreateProcess 句柄继承的坑，实测就是会
            // 卡住，没有再深挖到底哪一步）。清理路径的可靠性优先于最后
            // 一行日志，索性不写，直接退出。
            std::process::exit(0);
        });

        // Windows 兜底：上面这条 stdin EOF 看守线程在“App 被强杀”场景下
        // 真机实测未必可靠触发（同一份卡死顾虑），另开一路不依赖管道的
        // 轮询：直接查父进程（桌面应用）是否还存活，双保险。同样地，
        // 循环体里不能有任何可能阻塞的操作（尤其是写 stdout/stderr）。
        #[cfg(windows)]
        if let Ok(supervisor_pid) = std::env::var("MCPANE_SUPERVISOR_PID")
            && let Ok(supervisor_pid) = supervisor_pid.parse::<u32>()
        {
            let expected_start_filetime = std::env::var("MCPANE_SUPERVISOR_START_FILETIME")
                .ok()
                .and_then(|v| v.parse::<u64>().ok());
            std::thread::spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    if !windows_process_alive(supervisor_pid, expected_start_filetime) {
                        std::process::exit(0);
                    }
                }
            });
        }
    }

    let bind_addr = std::env::var("MCPANE_HUB_BIND").unwrap_or_else(|_| DEFAULT_BIND_ADDR.into());
    ensure_loopback_bind(&bind_addr)?;
    let endpoint = format!("http://{bind_addr}/mcp");

    // SQLite（mcpane.db，WAL）。
    let db_path = mcpane_hub::control::mcpane_home().join("mcpane.db");
    let pool = match mcpane_storage::open_database(&db_path).await {
        Ok(pool) => {
            tracing::info!(db = %db_path.display(), "SQLite 已就绪（WAL）");
            Some(pool)
        }
        Err(error) => {
            tracing::warn!(%error, "SQLite 打开失败，降级为纯内存 dev 模式");
            None
        }
    };
    let connections = pool.clone().map(|p| Arc::new(ConnectionRepo::new(p)));
    let logs = pool.clone().map(mcpane_storage::LogRepo::new);

    let registry = Arc::new(TokenRegistry::new());
    bootstrap_connections(&registry, connections.as_deref()).await;

    let state = Arc::new(mcpane_hub::state::AggregationState::new(logs));

    // 先绑数据面端口再启动控制面：端口即单实例锁——第二个实例在这里
    // 直接失败退出，不会碰到活实例的控制面 socket。
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;

    // 控制面必须先于下游装载启动：装载可能耗时数秒（逐个连下游），
    // desktop 的探活/状态展示不该等它——status 只依赖 registry/state。
    let started_at = std::time::Instant::now();
    let control_shutdown = Arc::new(tokio::sync::Notify::new());
    {
        let registry = registry.clone();
        let state = state.clone();
        let pool = pool.clone();
        let endpoint = endpoint.clone();
        let control_shutdown = control_shutdown.clone();
        tokio::spawn(async move {
            if let Err(error) = mcpane_hub::control::serve_control_status(
                registry,
                state,
                pool,
                endpoint,
                started_at,
                control_shutdown,
            )
            .await
            {
                tracing::error!(%error, "控制面状态端点退出");
            }
        });
    }

    // TASK-404：从 SQLite 装载已启用的下游 Server 并注册、重建 Catalog。
    // 下游 */list_changed 通知 → Catalog 自动重建。必须先于装载下游装配：
    // register 时才会给句柄挂上通知订阅。
    state.spawn_catalog_refresher();
    if let Some(pool) = &pool {
        mcpane_hub::control::load_downstreams(&state, pool).await;
    }

    // TASK-404：健康监控（周期探测 + 自动恢复），间隔可配。
    let health_interval = std::env::var("MCPANE_HEALTH_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_HEALTH_INTERVAL_SECS);
    state.spawn_health_monitor(std::time::Duration::from_secs(health_interval));

    // TASK-104：默认有状态（MCP-Session-Id）；MCPANE_HUB_STATEFUL=0 回退无状态基线。
    let stateful = std::env::var("MCPANE_HUB_STATEFUL").map_or(true, |v| v != "0");
    let session_keep_alive = std::env::var("MCPANE_SESSION_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(std::time::Duration::from_secs);
    let options = app::HubOptions {
        stateful,
        session_keep_alive: session_keep_alive.or(app::HubOptions::default().session_keep_alive),
    };
    tracing::info!(stateful, keep_alive_secs = ?options.session_keep_alive.map(|d| d.as_secs()), "Session 模式");
    let router = app::build_router(registry, state, options);
    tracing::info!(endpoint = %endpoint, protocol = mcpane_hub::MCP_PROTOCOL_VERSION, "MCP Hub 数据面已启动");

    // 优雅关停设 1s 上限：MCP 客户端常持有 SSE 长流（响应永不结束），
    // 无界等待在途连接会让退出永远卡住，最终被 desktop 的 3s 兜底强杀。
    let shutdown_started = Arc::new(tokio::sync::Notify::new());
    let graceful = {
        let notify = shutdown_started.clone();
        async move {
            shutdown_signal(control_shutdown).await;
            notify.notify_one();
        }
    };
    let server = std::future::IntoFuture::into_future(
        axum::serve(listener, router).with_graceful_shutdown(graceful),
    );
    tokio::pin!(server);
    let deadline = async {
        shutdown_started.notified().await;
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    };
    tokio::select! {
        result = &mut server => result?,
        () = deadline => tracing::warn!("在途连接 1s 内未结束，放弃等待直接退出"),
    }

    // 退出前删除控制面 socket，避免残留文件误导 desktop 的探活判定。
    #[cfg(unix)]
    let _ = std::fs::remove_file(mcpane_hub::control::control_socket_path());
    Ok(())
}

/// 退出信号：Ctrl-C、Unix SIGTERM、控制面 `shutdown`（Windows 没有 SIGTERM）。
async fn shutdown_signal(control_requested: Arc<tokio::sync::Notify>) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => tokio::select! {
                _ = tokio::signal::ctrl_c() => tracing::info!("收到 Ctrl-C，MCP Hub 正在退出"),
                _ = term.recv() => tracing::info!("收到 SIGTERM，MCP Hub 正在退出"),
                () = control_requested.notified() => {
                    tracing::info!("收到控制面 shutdown，MCP Hub 正在退出")
                }
            },
            Err(error) => {
                tracing::warn!(%error, "注册 SIGTERM 处理器失败，仅监听 Ctrl-C 与控制面");
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => tracing::info!("收到 Ctrl-C，MCP Hub 正在退出"),
                    () = control_requested.notified() => {
                        tracing::info!("收到控制面 shutdown，MCP Hub 正在退出")
                    }
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => tracing::info!("收到 Ctrl-C，MCP Hub 正在退出"),
            () = control_requested.notified() => {
                tracing::info!("收到控制面 shutdown，MCP Hub 正在退出")
            }
        }
    }
}

/// 启动引导：dev 环境变量 Token 入库（哈希）→ 注册表从库水合。
/// 无 SQLite 时纯内存注册。
async fn bootstrap_connections(registry: &TokenRegistry, repo: Option<&ConnectionRepo>) {
    let dev_tokens = [
        (
            "MCPANE_CLAUDE_TOKEN",
            "conn_dev_claude",
            "Claude Code (dev)",
            ClientType::Claude,
            "claude",
        ),
        (
            "MCPANE_CODEX_TOKEN",
            "conn_dev_codex",
            "Codex (dev)",
            ClientType::Codex,
            "codex",
        ),
    ];

    match repo {
        Some(repo) => {
            for (env_var, id, name, _client_type, type_str) in dev_tokens {
                match std::env::var(env_var) {
                    Ok(token) if !token.trim().is_empty() => {
                        if let Err(error) = repo
                            .upsert_with_token(id, name, type_str, token.trim())
                            .await
                        {
                            tracing::error!(%error, env_var, "dev Token 入库失败");
                        } else {
                            tracing::info!(id, env_var, "dev Token 已入库（仅哈希）");
                        }
                    }
                    _ => {
                        // 库里没有任何连接且环境变量缺失时，引导生成一个（打印一次）。
                        let existing = repo.list().await.unwrap_or_default();
                        if !existing.iter().any(|c| c.id == id) {
                            let token = generate_token();
                            if repo
                                .upsert_with_token(id, name, type_str, &token)
                                .await
                                .is_ok()
                            {
                                eprintln!(
                                    "[mcpane-hub] {env_var} 未设置，已生成 Token（仅展示一次）:"
                                );
                                eprintln!("[mcpane-hub]   export {env_var}={token}");
                            }
                        }
                    }
                }
            }
            mcpane_hub::control::hydrate_registry(registry, repo).await;
        }
        None => {
            for (env_var, id, _name, _client_type, type_str) in dev_tokens {
                let identity = ClientIdentity::from_raw(id.to_owned(), type_str);
                match std::env::var(env_var) {
                    Ok(token) if !token.trim().is_empty() => {
                        registry.register(token.trim(), identity);
                    }
                    _ => {
                        let token = generate_token();
                        registry.register(&token, identity);
                        eprintln!("[mcpane-hub] {env_var} 未设置，已生成临时 Token（仅展示一次）:");
                        eprintln!("[mcpane-hub]   export {env_var}={token}");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // cfg(windows) 的进程存活用例要 spawn 真进程，expect 比层层 unwrap_or_else 清楚。
    #![allow(clippy::expect_used)]
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        parse_args(args.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn no_args_runs_the_server() {
        assert_eq!(parse(&[]), Cli::Run);
    }

    #[test]
    fn help_and_version_flags_are_recognized() {
        assert_eq!(parse(&["--help"]), Cli::Help);
        assert_eq!(parse(&["-h"]), Cli::Help);
        assert_eq!(parse(&["--version"]), Cli::Version);
        assert_eq!(parse(&["-V"]), Cli::Version);
    }

    #[test]
    fn bridge_subcommand_is_recognized() {
        assert_eq!(parse(&["bridge"]), Cli::Bridge);
    }

    /// 这条是本次修复的重点。此前 argv 完全不解析：`--help` 不打帮助，
    /// 而是一声不吭地把服务起起来占住 24860——排查时根本想不到是自己起的。
    /// 拼错的配置参数同理，必须报错而不是忽略。
    #[test]
    fn unknown_args_are_rejected_not_ignored() {
        assert_eq!(
            parse(&["--bind", "0.0.0.0:9999"]),
            Cli::Unknown("--bind".into())
        );
        assert_eq!(parse(&["--helpp"]), Cli::Unknown("--helpp".into()));
        assert_eq!(parse(&["serve"]), Cli::Unknown("serve".into()));
    }

    #[test]
    fn help_reports_the_real_defaults() {
        let help = help();
        assert!(
            help.contains(DEFAULT_BIND_ADDR),
            "帮助必须给出真实的默认监听地址"
        );
        assert!(help.contains(&DEFAULT_HEALTH_INTERVAL_SECS.to_string()));
        // 会话超时取自 HubOptions::default()，与数据面实际行为同源。
        assert!(
            help.contains("300s"),
            "帮助里的会话超时应与 HubOptions::default() 一致"
        );
        assert!(help.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn loopback_binds_are_accepted() {
        assert!(ensure_loopback_bind("127.0.0.1:24860").is_ok());
        assert!(ensure_loopback_bind("[::1]:24860").is_ok());
        assert!(ensure_loopback_bind(DEFAULT_BIND_ADDR).is_ok());
    }

    /// SECURITY.md 的「永不监听非 loopback」不变式：环境变量通道必须在启动期拦住。
    #[test]
    fn non_loopback_binds_are_rejected() {
        assert!(ensure_loopback_bind("0.0.0.0:24860").is_err());
        assert!(ensure_loopback_bind("[::]:24860").is_err());
        // 具体网卡地址（RFC 5737 文档示例段）同样拒绝。
        assert!(ensure_loopback_bind("192.0.2.1:24860").is_err());
    }

    /// 真机复现（2026-08-12）：PID 会被系统复用，只查 `STILL_ACTIVE` 会把
    /// 已死父进程误判成还活着（详见 `windows_process_alive` 文档）。
    #[cfg(windows)]
    #[test]
    fn windows_process_alive_reports_self_as_alive() {
        assert!(windows_process_alive(std::process::id(), None));
    }

    #[cfg(windows)]
    #[test]
    fn windows_process_alive_reports_exited_process_as_dead() {
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "exit", "0"])
            .spawn()
            .expect("spawn cmd");
        let pid = child.id();
        child.wait().expect("wait for exit");
        assert!(!windows_process_alive(pid, None));
    }

    /// 创建时间对不上：即使 PID 现在指向一个活的进程，也不能算作原父进程
    /// ——这正是 PID 复用误判的核心防线。
    #[cfg(windows)]
    #[test]
    fn windows_process_alive_rejects_mismatched_start_filetime() {
        assert!(!windows_process_alive(std::process::id(), Some(1)));
    }

    /// 帮助漏掉一个环境变量，等于把唯一的配置入口藏起来——而 hub 没有别的入口。
    /// 做法是扫本文件里以字面量取环境变量的那些调用，新增时忘了写进帮助，这里会红。
    ///
    /// 扫的是源码文本，所以本模块里别再以字面量形式写出 `MCPANE_` 开头的取值调用
    /// ——包括注释。写了会被当成真调用，误报成「帮助漏了它」（初版就这么自摆乌龙）。
    #[test]
    fn help_documents_every_env_var_read_by_main() {
        const PAT: &str = "env::var(\"";
        let src = include_str!("main.rs");
        let help = help();
        let mut missing: Vec<&str> = Vec::new();
        for (index, _) in src.match_indices(PAT) {
            let rest = &src[index + PAT.len()..];
            let Some(end) = rest.find('"') else { continue };
            let name = &rest[..end];
            if name.starts_with("MCPANE_") && !help.contains(name) {
                missing.push(name);
            }
        }
        missing.sort_unstable();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "这些环境变量 main.rs 会读，但 --help 只字未提: {missing:?}"
        );
    }

    /// 上面那条扫不到它们：dev Token 的变量名是数组元素，到 `env::var` 时已是变量。
    #[test]
    fn help_documents_dev_token_and_data_dir_vars() {
        let help = help();
        for name in [
            "MCPANE_CLAUDE_TOKEN",
            "MCPANE_CODEX_TOKEN",
            "MCPANE_HOME",
            "RUST_LOG",
        ] {
            assert!(help.contains(name), "帮助未提及 {name}");
        }
    }
}
