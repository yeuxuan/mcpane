//! 控制面（ADR-007）：仅本机用户可见的类型化命令端点，一行 JSON 命令 →
//! 一行 JSON 响应。
//!
//! - macOS/Unix：Unix Domain Socket（权限 0600）；
//! - Windows：Named Pipe（DACL 显式收紧到创建者与 SYSTEM，等价 0600——默认
//!   安全描述符会额外给 Everyone 与匿名账户读权限，不能依赖）；
//! - 两端 accept/连接层各异，但命令派发共用 `handle_control_connection`（传输无关）；
//! - 完整命令集（shutdown / reload / rotate-token / diagnostics / set-workspace 等）见 ControlCommand。
//!
//! **Windows 说明**：Named Pipe 实现已落地，除按 `x86_64-pc-windows-gnu` 目标
//! 通过编译核验外，也已在真实 Windows 机器上运行时验证（见下方
//! `windows_response_survives_a_slow_client_read`、`owner_only_sddl_is_accepted_by_win32`
//! 两条 Windows-only 回归，2026-08-04/2026-08-12 均实测通过）。

use std::sync::Arc;

use serde::Serialize;

use crate::auth::TokenRegistry;
use crate::{MCP_PROTOCOL_VERSION, SERVER_VERSION};
/// 命令 wire 类型与 socket 路径的唯一事实源（双端共享）。
pub use mcpane_hub_protocol::ControlCommand;
#[cfg(unix)]
pub use mcpane_hub_protocol::control_socket_path;
pub use mcpane_hub_protocol::mcpane_home;

/// 唯一已实现的 Runtime Policy。DB 的 CHECK 约束还放行
/// `per-client-connection` / `per-upstream-session`，但 Hub 侧一律按共享单实例
/// 运行——那两种要按连接/会话各起一份下游并各自管生命周期与清理，是独立工作量。
const RUNTIME_POLICY_SHARED: &str = "shared";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubStatus {
    pub name: &'static str,
    pub server_version: String,
    /// 主协议版本。Hub 是双栈的，这一个值说不全实情——历史版本见
    /// [`Self::legacy_protocol_version`]（ADR-019）。
    pub protocol_version: String,
    /// 兼容层版本：两个锚定客户端目前实际走的那一档。
    pub legacy_protocol_version: String,
    pub endpoint: String,
    pub uptime_secs: u64,
    pub connections: Vec<ConnectionSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionSummary {
    pub client_connection_id: String,
    /// 库里存的原样字符串（自定义客户端就是用户填的标识）。
    pub client_type: String,
    /// 仅前缀，永不包含完整 Token。
    pub token_prefix: String,
    pub enabled: bool,
    /// 绑定的默认 Workspace；None 表示跟随默认工作区。
    pub default_workspace_id: Option<String>,
    /// 该连接最近一次请求协商到的协议版本；None 表示尚未见到请求。
    pub protocol_version: Option<String>,
    /// 工具暴露模式：flat / gateway / auto（ADR-020）。
    pub tool_exposure_mode: String,
}

/// 从 SQLite 装载启用的下游定义：连接、注册、按 Workspace 重建 Catalog、
/// 水合连接→Workspace 映射。单个下游失败只告警，不影响其他。
pub async fn load_downstreams(
    state: &crate::state::AggregationState,
    pool: &mcpane_storage::SqlitePool,
) {
    // 串行化装载：控制面先于启动装载就绪后，reload-servers 可能与之并发，
    // 同一下游会被重复连接（旧 handle 虽被覆盖丢弃，stdio 子进程会短暂翻倍）。
    // 用 state 上的重建互斥而非本地 static：自动重建（通知/健康恢复）与本函数
    // 竞态时，会用旧 last_overrides 快照把用户刚做的禁用收尾覆盖回去。
    let _guard = state.lock_rebuild().await;

    // 先刷代理再连下游：本轮新建的连接必须用上新配置。
    //
    // 桌面端改完代理写库后发的就是 ReloadServers ——它的语义本就是「重读配置、
    // 重连下游」，正是改代理需要的动作，故不另加控制面命令。
    // 代理不在 SpawnSpec 里（它是全局配置，见 DownstreamHandle::connect 注释），
    // 故必须在这里显式比对：它一变，所有 HTTP/SSE 下游的出网路径就变了，
    // 「规格未变不重连」的快捷判定对它不成立。
    let previous_proxy = state.proxy();
    match mcpane_storage::SettingsRepo::new(pool.clone())
        .get_proxy()
        .await
    {
        Ok(proxy) => {
            tracing::info!(proxy = %mcpane_net::describe(&proxy), "出网代理已装载");
            // 同时喂给本进程的全局出网槽：Hub 目前只经 SpawnSpec 显式传参用代理，
            // 但 mcpane-net 的 client_for 走的是全局槽。不初始化它，将来谁在 Hub
            // 里加一处 client_for，会静默拿到默认的「跟随系统」，用户配的
            // Direct/Manual 无声失效。一行的事，消灭整类问题。
            if let Err(error) = mcpane_net::set_proxy(&proxy) {
                tracing::warn!(%error, "全局出网槽应用代理失败，该进程内 client_for 将按跟随系统出网");
            }
            state.set_proxy(proxy);
        }
        // 沿用上一次的配置，不退回默认：用户已明确配过代理时，「按默认出网」
        // 会绕开他指定的通道（可能直接连不上，也可能把流量送错地方），
        // 比保持原样更糟。
        Err(error) => tracing::error!(%error, "读取代理配置失败，沿用上一次的配置"),
    }

    let servers = mcpane_storage::ServerRepo::new(pool.clone());
    let defs = match servers.list().await {
        Ok(defs) => defs,
        Err(error) => {
            tracing::error!(%error, "读取下游 Server 定义失败");
            return;
        }
    };
    // 回收孤儿 OAuth 令牌：服务器被删除后，它的令牌文件不该留在盘上。
    // 只按「库里还有没有这个 Server」判定（禁用的 Server 保留令牌，重新启用不必重登）。
    {
        let oauth_dir = state.oauth().spec("").dir;
        let live: Vec<String> = defs.iter().map(|d| d.id.clone()).collect();
        mcpane_hub_downstream::oauth::prune(&oauth_dir, &live);
    }
    // 注销不再启用的旧实例。
    let enabled_ids: std::collections::HashSet<String> = defs
        .iter()
        .filter(|d| d.enabled)
        .map(|d| d.id.clone())
        .collect();
    for registered in state.registered_servers() {
        if !enabled_ids.contains(&registered.server_id) {
            state.unregister_server(&registered.server_id);
        }
    }

    // 取一次即可：本轮所有下游共用同一份代理配置。
    let proxy = state.proxy();
    let proxy_changed = proxy != previous_proxy;
    for def in defs.into_iter().filter(|d| d.enabled) {
        // 未实现的 policy 如实告警，不静默按 shared 跑：用户选了隔离模式却拿到
        // 共享实例，从外部完全看不出来（工具照常可用，只是没隔离）。
        if def.runtime_policy != RUNTIME_POLICY_SHARED {
            tracing::warn!(
                server = %def.slug,
                policy = %def.runtime_policy,
                "runtime_policy 尚未实现，本下游按 shared 单实例运行"
            );
        }
        let spec = match def.transport {
            mcpane_storage::ServerTransport::Stdio => {
                let Some(command) = def.command.clone() else {
                    tracing::warn!(server = %def.slug, "stdio Server 缺少 command，跳过");
                    continue;
                };
                mcpane_hub_downstream::SpawnSpec::Stdio {
                    command,
                    args: def.args.clone(),
                    env: def.env.clone(),
                    cwd: def.cwd.clone(),
                }
            }
            mcpane_storage::ServerTransport::Http => {
                let Some(url) = def.url.clone() else {
                    tracing::warn!(server = %def.slug, "HTTP Server 缺少 url，跳过");
                    continue;
                };
                mcpane_hub_downstream::SpawnSpec::Http {
                    url,
                    bearer: def.bearer.clone(),
                    bearer_env: def.bearer_env.clone(),
                    env: def.env.clone(),
                    headers: def.headers.clone(),
                    oauth: Some(state.oauth().spec(&def.id)),
                }
            }
            mcpane_storage::ServerTransport::Sse => {
                let Some(url) = def.url.clone() else {
                    tracing::warn!(server = %def.slug, "SSE Server 缺少 url，跳过");
                    continue;
                };
                mcpane_hub_downstream::SpawnSpec::Sse {
                    url,
                    bearer: def.bearer.clone(),
                    bearer_env: def.bearer_env.clone(),
                    env: def.env.clone(),
                    headers: def.headers.clone(),
                }
            }
        };
        // 增量装载：规格没变、句柄还在、状态也没判失败的下游一律留着不动。
        //
        // ReloadServers 的调用点有十余处——逐工具开关、Prompt/Resource 开关、
        // 发现模式开关、Workspace 增删与成员变更、导入、云同步全都发它。若每次
        // 都无条件重连，改一个跟 Server A 毫不相干的设置，也会 kill 掉 A 的 stdio
        // 子进程再重起（打断在途 tools/call，丢掉下游进程内状态如 memory Server
        // 的图谱），耗时还随 Server 数线性增长（每个连接超时 10s，且本循环是串行）。
        // 编辑弹窗里的「全部禁用」更是 N 个工具 = N 轮全量重连。
        //
        // 只有 Ready 才留：Degraded/Failed 说明探测已经在报错，Starting 说明
        // 健康监控正在重连——这几种状态下用户手动触发的 reload 理应重试，
        // 而不是把一个连不上的下游原样留着。
        if !proxy_changed
            && let Some(current) = state.registered(&def.id)
            && current.spec == spec
            && current.status() == crate::state::ServerStatus::Ready
            && current.handle.is_some()
        {
            tracing::debug!(server = %def.slug, "下游规格未变，保留现有连接");
            continue;
        }
        // 先占 Starting 槽：诊断立刻报真·启动中，失败再落到 failed——
        // 不再「本轮跳过」导致前端永远合成启动中。
        state.mark_server_starting(&def.id, &def.slug, spec.clone());
        match mcpane_hub_downstream::DownstreamHandle::connect(
            &spec,
            std::time::Duration::from_secs(10),
            &proxy,
        )
        .await
        {
            Ok(handle) => {
                state.register_server(&def.id, &def.slug, spec, handle);
                tracing::info!(server = %def.slug, "下游已连接");
            }
            Err(error) => {
                if error.needs_auth() {
                    tracing::info!(server = %def.slug, "下游需要登录授权，等待用户登录");
                } else {
                    tracing::warn!(server = %def.slug, %error, "下游连接失败");
                }
                state.mark_server_error(&def.id, &def.slug, spec, &error);
            }
        }
    }

    // 按 Workspace 重建 Catalog：
    // - ws_default：包含全部启用 Server（保持单 Workspace 语义）；
    // - 其他 Workspace：只含 workspace_servers 中启用的 Server；
    // - 各自应用 workspace_tools 覆盖。
    let workspace_repo = mcpane_storage::WorkspaceRepo::new(pool.clone());
    // 注解声明是全局的，每个工作区重建时合并同一份（见 load_overrides）。
    let server_repo = mcpane_storage::ServerRepo::new(pool.clone());
    // (workspace_id, tool_discovery)。ws_default 在库里无真实行，
    // 其发现开关走 tool_discovery() 单查（设置幽灵行，见 0008 迁移）。
    let default_discovery = workspace_repo
        .tool_discovery(crate::state::DEFAULT_WORKSPACE)
        .await
        .unwrap_or(false);
    let mut workspaces = vec![(
        crate::state::DEFAULT_WORKSPACE.to_owned(),
        default_discovery,
    )];
    // Workspace → 使用准则（instructions 注入用，ADR-020 主动性轴）。
    // 读取失败保留旧映射同 hydrate_connection_mappings 策略，
    // 故这里逐步累积、末尾一次性覆盖。
    let mut guidance_map = std::collections::HashMap::new();
    if let Ok(guidance) = workspace_repo
        .usage_guidance(crate::state::DEFAULT_WORKSPACE)
        .await
        && !guidance.is_empty()
    {
        guidance_map.insert(crate::state::DEFAULT_WORKSPACE.to_owned(), guidance);
    }
    // 列表读取失败必须跳过下方的幽灵清理：此时「DB 现存集合」不可知，若按
    // 默认工作区单集去 retain，会把全部非默认 Workspace 的内存状态（Catalog、
    // 覆盖、成员）误清——一次 DB 抖动就击穿 Workspace 隔离。
    let mut workspace_list_ok = true;
    match workspace_repo.list().await {
        Ok(list) => {
            for ws in list {
                if ws.id != crate::state::DEFAULT_WORKSPACE {
                    if !ws.usage_guidance.is_empty() {
                        guidance_map.insert(ws.id.clone(), ws.usage_guidance.clone());
                    }
                    workspaces.push((ws.id, ws.tool_discovery));
                }
            }
        }
        Err(error) => {
            workspace_list_ok = false;
            tracing::error!(%error, "读取 Workspace 列表失败，本轮保留旧 Workspace 状态");
        }
    }
    // 列表读取失败时不覆盖（否则只剩默认工作区那条，自建工作区的准则被误清）。
    if workspace_list_ok {
        state.set_workspace_guidance(guidance_map);
    }
    for (workspace_id, tool_discovery) in &workspaces {
        let overrides = match load_overrides(&workspace_repo, &server_repo, workspace_id).await {
            Ok(overrides) => overrides,
            // 覆盖读不到就整体跳过本 Workspace 的重建，内存里的旧覆盖与禁用集
            // 原样保留（见 load_overrides 的说明）。
            Err(error) => {
                tracing::error!(
                    %error,
                    workspace_id,
                    "读取 Workspace 覆盖失败，跳过本轮重建，保留旧覆盖"
                );
                continue;
            }
        };
        let member_servers: Option<std::collections::HashSet<String>> =
            if workspace_id == crate::state::DEFAULT_WORKSPACE {
                None // 全量
            } else {
                // 查询失败取空集（fail-closed）：None 表示「全量成员」，
                // 一次 DB 抖动就把非成员 Server 的工具泄进本 Workspace。
                Some(
                    workspace_repo
                        .enabled_servers(workspace_id)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .collect(),
                )
            };
        if let Err(error) = state
            .rebuild_catalog_scoped(
                workspace_id,
                &overrides,
                member_servers.as_ref(),
                *tool_discovery,
            )
            .await
        {
            tracing::error!(%error, workspace_id, "Catalog 重建失败");
        }
    }

    // 清理已删 Workspace 的幽灵状态（本轮 DB 现存集合为准）。
    if workspace_list_ok {
        let keep: std::collections::HashSet<String> =
            workspaces.iter().map(|(id, _)| id.clone()).collect();
        state.retain_workspaces(&keep);
    }

    // 连接 → Workspace / 暴露模式映射。
    let connections = mcpane_storage::ConnectionRepo::new(pool.clone());
    hydrate_connection_mappings(state, &connections).await;

    // 连接 → 被拒 Server 映射（访问控制，全量覆盖与增量更新收敛到同一 DB 事实源）。
    match connections.denied_servers_map().await {
        Ok(map) => state.set_connection_denied_map(map),
        Err(error) => tracing::warn!(%error, "读取访问控制 deny-list 失败，本轮保留旧映射"),
    }
}

/// 刷新连接 → Workspace / 暴露模式两张映射（DB 为事实源，全量覆盖内存，
/// 一次 list() 同时喂两张表）。单独抽出：绑定/模式变更只需刷新映射，不必像
/// load_downstreams 那样重连全部下游。
async fn hydrate_connection_mappings(
    state: &crate::state::AggregationState,
    connections: &mcpane_storage::ConnectionRepo,
) {
    match connections.list().await {
        Ok(list) => {
            let workspaces = list
                .iter()
                .filter_map(|c| c.default_workspace_id.clone().map(|ws| (c.id.clone(), ws)))
                .collect();
            let exposures = list
                .iter()
                .map(|c| {
                    (
                        c.id.clone(),
                        crate::state::ExposureMode::parse(&c.tool_exposure_mode),
                    )
                })
                .collect();
            state.set_connection_workspaces(workspaces);
            state.set_connection_exposures(exposures);
        }
        // 与 deny-list 同一策略：读取失败保留旧映射并留痕，不静默吞掉。
        Err(error) => {
            tracing::warn!(%error, "读取连接映射失败，本轮保留旧映射");
        }
    }
}

/// 读取某 Workspace 的 Tool 覆盖（禁用/alias）。
///
/// 读取失败必须上抛而不是取空集：空覆盖等价于「本 Workspace 没有任何禁用项」，
/// 而 `rebuild_catalog_scoped` 拿它**整体替换** `disabled_tools`，一次 DB 抖动
/// 就会让用户明确关掉的 Tool 重新进 `tools/list` 并通过调用时的二次校验，
/// alias 也一并丢失导致暴露名变化。方向与下方 `enabled_servers` 的 fail-closed
/// 一致，只是这里的保守值是「保持现状」而非「空集」——调用方据此跳过本轮重建。
async fn load_overrides(
    repo: &mcpane_storage::WorkspaceRepo,
    servers: &mcpane_storage::ServerRepo,
    workspace_id: &str,
) -> Result<mcpane_hub_catalog::CatalogOverrides, mcpane_storage::StorageError> {
    let mut overrides = mcpane_hub_catalog::CatalogOverrides::default();
    for (kind, target) in [
        (mcpane_storage::ItemKind::Tool, &mut overrides.tools),
        (mcpane_storage::ItemKind::Prompt, &mut overrides.prompts),
    ] {
        for item in repo.item_overrides(workspace_id, kind).await? {
            target.insert(
                (item.server_id, item.original_name),
                mcpane_hub_catalog::ItemOverride {
                    enabled: item.enabled,
                    alias: item.alias,
                    description_suffix: item.description_suffix,
                    ..Default::default()
                },
            );
        }
    }
    for item in repo
        .item_overrides(workspace_id, mcpane_storage::ItemKind::Resource)
        .await?
    {
        overrides
            .resources
            .insert((item.server_id, item.original_name), item.enabled);
    }
    // 注解是全局的（键不含 workspace，见迁移 0025），每个工作区重建时都合并
    // 同一份。必须 entry().or_default() 而非 insert：一个工具可能既被本工作区
    // 禁用、又有全局声明，insert 会把整条覆盖项换成默认值（enabled=true），
    // 把用户明确关掉的工具悄悄翻回启用——本仓最忌讳的那类回归。
    for ((server_id, original_name), declared) in servers.tool_annotations().await? {
        overrides
            .tools
            .entry((server_id, original_name))
            .or_default()
            .declared_annotations = Some(declared);
    }
    Ok(overrides)
}

/// 从 SQLite 水合内存注册表（明文永不出库，只搬运哈希）。
pub async fn hydrate_registry(registry: &TokenRegistry, repo: &mcpane_storage::ConnectionRepo) {
    match repo.list_with_hashes().await {
        Ok(rows) => {
            registry.clear();
            for (connection, hash) in rows {
                registry.register_hash(
                    &hash,
                    &connection.token_prefix,
                    crate::auth::ClientIdentity::from_raw(
                        connection.id.clone(),
                        &connection.client_type,
                    ),
                    connection.enabled,
                );
            }
            tracing::info!("Token 注册表已从 SQLite 水合");
        }
        Err(error) => tracing::error!(%error, "注册表水合失败"),
    }
}

pub fn hub_status(
    registry: &TokenRegistry,
    endpoint: &str,
    started_at: std::time::Instant,
    connection_workspaces: &std::collections::HashMap<String, String>,
    connection_protocols: &std::collections::HashMap<String, String>,
    connection_exposures: &std::collections::HashMap<String, crate::state::ExposureMode>,
) -> HubStatus {
    HubStatus {
        name: "mcpane-hub",
        server_version: SERVER_VERSION.to_owned(),
        protocol_version: MCP_PROTOCOL_VERSION.to_owned(),
        legacy_protocol_version: crate::MCP_LEGACY_PROTOCOL_VERSION.to_owned(),
        endpoint: endpoint.to_owned(),
        uptime_secs: started_at.elapsed().as_secs(),
        connections: registry
            .connection_summaries()
            .into_iter()
            .map(
                |(client_connection_id, client_type, token_prefix, enabled)| ConnectionSummary {
                    default_workspace_id: connection_workspaces.get(&client_connection_id).cloned(),
                    protocol_version: connection_protocols.get(&client_connection_id).cloned(),
                    tool_exposure_mode: connection_exposures
                        .get(&client_connection_id)
                        .copied()
                        .unwrap_or(crate::state::ExposureMode::Auto)
                        .as_str()
                        .to_owned(),
                    client_connection_id,
                    client_type,
                    token_prefix,
                    enabled,
                },
            )
            .collect(),
    }
}

/// 启动控制面状态端点（Unix：UDS，0600）。
#[cfg(unix)]
pub async fn serve_control_status(
    registry: Arc<TokenRegistry>,
    state: Arc<crate::state::AggregationState>,
    pool: Option<mcpane_storage::SqlitePool>,
    endpoint: String,
    started_at: std::time::Instant,
    shutdown: Arc<tokio::sync::Notify>,
) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let connections = pool
        .clone()
        .map(|p| Arc::new(mcpane_storage::ConnectionRepo::new(p)));

    let socket_path = control_socket_path();
    if let Some(parent) = socket_path.parent() {
        let created = !parent.exists();
        std::fs::create_dir_all(parent)?;
        if created {
            // 我们自己新建的目录一律收紧为 0700（含 SUN_LEN 回退目录）。
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    // 上一次未清理的 socket 文件会导致 bind 失败；但若对端能连通说明
    // 已有活实例在服务，绝不能抢占它的控制面。
    if socket_path.exists() {
        let alive = tokio::time::timeout(
            std::time::Duration::from_millis(300),
            tokio::net::UnixStream::connect(&socket_path),
        )
        .await
        .is_ok_and(|r| r.is_ok());
        if alive {
            anyhow::bail!(
                "另一个 mcpane-hub 实例已在运行（控制面 {} 可连通）",
                socket_path.display()
            );
        }
        let _ = std::fs::remove_file(&socket_path);
    }

    let listener = tokio::net::UnixListener::bind(&socket_path)?;
    std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;
    tracing::info!(socket = %socket_path.display(), "控制面状态端点已启动 (UDS, 0600)");

    loop {
        // accept 失败（ECONNABORTED / EMFILE 等瞬时错误）不能放弃整个控制面：
        // 任务一旦退出，hub 主进程仍在服务数据面，桌面探活却永远 refused，
        // 会被误判为「外部程序占端口」进入死局。
        let (stream, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(error) => {
                tracing::warn!(%error, "控制面 accept 失败，稍后继续");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };
        // 每个控制连接独立 spawn：慢命令（ReloadServers 逐个连下游、每个 10s
        // 超时）不得堵住 accept 循环，否则并发的 Status 探活连不进来、桌面会把
        // 活着的 hub 误判为已死。共享句柄皆 Arc/Clone，逐连接克隆后 move 进任务；
        // load_downstreams 自带 RELOAD_LOCK，并发 reload 仍串行、不会重复连下游。
        tokio::spawn(handle_control_connection(
            stream,
            ControlDeps {
                registry: registry.clone(),
                state: state.clone(),
                pool: pool.clone(),
                connections: connections.clone(),
                endpoint: endpoint.clone(),
                started_at,
                read_timeout: CONTROL_READ_TIMEOUT,
                shutdown: Some(shutdown.clone()),
            },
        ));
    }
}

/// 单行命令读取的超时上限。
///
/// 本地 IPC（UDS/Named Pipe）正常传输是微秒级的，但系统繁忙（CI 机器抖动、
/// 大量 env/headers 的 `TestServer` 命令被内核分片投递）时，一整行可能分
/// 好几次 `read` 才凑齐。**这个值只决定"裸连接不发送任何东西时多快判定为
/// 探测"，不能定得太短**——定得太短、又把超时和"空行/EOF"混为一谈（历史
/// 版本的 bug），会把一条正在慢速抵达的合法命令误判成探测请求，参见下方
/// `handle_control_connection` 的分支拆分。2s 对手动 `nc` 探测而言完全无感，
/// 对分片写入而言留了充分余量；且每个连接独立 spawn（见 `serve_control_status`
/// 注释），调大它不会拖慢 accept 循环处理其他连接。
const CONTROL_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(2_000);

/// 处理一条控制连接所需的运行时依赖。
///
/// 打包成结构体而不是逐个传参：Unix accept 循环、Windows Named Pipe 循环与
/// 测试共四处调用点，逐个传会把同一串 `clone()` 抄四遍，日后加字段极易漏改
/// 其中一处（且漏改只在那条平台路径上暴露）。`handle_control_connection`
/// 入口处解构回同名局部变量，函数体照旧按名字用。
#[derive(Clone)]
struct ControlDeps {
    registry: Arc<TokenRegistry>,
    state: Arc<crate::state::AggregationState>,
    pool: Option<mcpane_storage::SqlitePool>,
    connections: Option<Arc<mcpane_storage::ConnectionRepo>>,
    endpoint: String,
    started_at: std::time::Instant,
    /// 显式注入而非直接读常量：测试要用极短超时验证"慢速分片抵达的合法命令
    /// 不被误判成探测"这条回归。
    read_timeout: std::time::Duration,
    /// 控制面 `shutdown` 用来触发进程退出；测试里可以是 None（命令仍回 ok）。
    shutdown: Option<Arc<tokio::sync::Notify>>,
}

/// Token 吊销/恢复：**DB 先写、内存后改**。
///
/// 此前顺序相反且 DB 写失败被 `let _ =` 吞掉——UI 显示撤销成功，库里没落，
/// hub 一重启 `hydrate_registry` 就把 Token 复活（安全相关的假成功）。现在
/// DB 写失败时内存一并不动、响应带错因；DB 成功但内存 registry 未命中
/// （尚未水合该连接）时整表重水合收敛，保证活跃 hub 的判定与 DB 一致。
/// 无 DB（降级运行）时退化为纯内存开关，语义同旧版。
async fn set_token_enabled(
    registry: &TokenRegistry,
    connections: Option<&mcpane_storage::ConnectionRepo>,
    client_connection_id: &str,
    enabled: bool,
) -> serde_json::Value {
    if let Some(repo) = connections {
        match repo.set_enabled(client_connection_id, enabled).await {
            Ok(true) => {}
            Ok(false) => {
                return serde_json::json!({
                    "ok": false,
                    "clientConnectionId": client_connection_id,
                    "error": format!("连接不存在：{client_connection_id}"),
                });
            }
            Err(error) => {
                return serde_json::json!({
                    "ok": false,
                    "clientConnectionId": client_connection_id,
                    "error": format!("数据库写入失败，Token 状态未变更：{error}"),
                });
            }
        }
        if !registry.set_enabled(client_connection_id, enabled) {
            hydrate_registry(registry, repo).await;
        }
        return serde_json::json!({
            "ok": true,
            "clientConnectionId": client_connection_id,
            "enabled": enabled,
        });
    }
    let hit = registry.set_enabled(client_connection_id, enabled);
    serde_json::json!({
        "ok": hit,
        "clientConnectionId": client_connection_id,
        "enabled": enabled,
    })
}

/// 单个控制连接的处理：读一行 JSON 命令 → 派发 → 写一行 JSON 响应 → 关闭。
///
/// 传输无关（UDS 与 Named Pipe 都实现 AsyncRead+AsyncWrite），Unix 与 Windows
/// 的 accept 循环共用这一份派发逻辑，杜绝双端命令处理漂移。
async fn handle_control_connection<S>(stream: S, deps: ControlDeps)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let ControlDeps {
        registry,
        state,
        pool,
        connections,
        endpoint,
        started_at,
        read_timeout,
        shutdown,
    } = deps;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    let mut stream = BufReader::new(stream);

    // 读一行命令。**超时必须与"空行/EOF"分开处理**——此前两者共用
    // `_ => Some(ControlCommand::Status)` 兜底：一条格式完全合法、只是被分片
    // 慢速写入（或系统繁忙）拖过超时窗口的命令（例如 revoke-token）会被当成
    // 无害的 Status 探测执行掉、连接随即关闭，调用方拿到的是 Status 响应、
    // 会误以为命令已生效，而真正的命令从未被派发——这是比明确报错更危险的
    // 假成功。空行/EOF（真探测：对端确实什么都没发）才归入 Status 兜底。
    // `read_timeout` 由调用方传入（生产走 `CONTROL_READ_TIMEOUT`），测试用
    // 更短的值即可确定性地触发超时分支，不必真等 2s。
    // 单行封顶：合法命令（含带大 env 表的 TestServer）远小于此；异常/恶意
    // 客户端不带换行地灌数据时，read_line 会在超时窗口内无限累积到内存里，
    // take 让累积在上限处停下并立即回明确错误。
    const MAX_COMMAND_LINE_BYTES: u64 = 256 * 1024;
    let mut line = String::new();
    let mut limited = (&mut stream).take(MAX_COMMAND_LINE_BYTES + 1);
    let read_result = tokio::time::timeout(read_timeout, limited.read_line(&mut line)).await;
    let timed_out = read_result.is_err();
    let oversized = line.len() as u64 > MAX_COMMAND_LINE_BYTES;
    let command = match read_result {
        _ if oversized => None,
        Ok(Ok(n)) if n > 0 && !line.trim().is_empty() => {
            serde_json::from_str::<ControlCommand>(line.trim()).ok()
        }
        // EOF / 空行 / 读时 IO 错误：真探测或连接本就有问题，按 Status 处理
        // （向后兼容 `nc` 裸探测；IO 错误时响应大概率也写不回去，无实际影响）。
        Ok(_) => Some(ControlCommand::Status),
        // 超时：不得假装是 Status，见上方函数注释。
        Err(_) => None,
    };

    let response = if oversized {
        serde_json::json!({
            "ok": false,
            "error": format!("控制命令过长（超过 {MAX_COMMAND_LINE_BYTES} 字节），已拒绝")
        })
    } else if timed_out {
        serde_json::json!({
            "ok": false,
            "error": format!("读取控制命令超时（{read_timeout:?} 内未收到完整一行），请重试")
        })
    } else {
        match command {
            Some(ControlCommand::Status) => serde_json::to_value(hub_status(
                &registry,
                &endpoint,
                started_at,
                &state.connection_workspaces_snapshot(),
                &state.connection_protocols_snapshot(),
                &state.connection_exposures_snapshot(),
            ))
            .unwrap_or_else(|error| serde_json::json!({ "ok": false, "error": error.to_string() })),
            Some(ControlCommand::Shutdown) => {
                if let Some(notify) = &shutdown {
                    notify.notify_one();
                }
                tracing::info!("收到控制面 shutdown");
                serde_json::json!({ "ok": true })
            }
            Some(ControlCommand::RevokeToken {
                client_connection_id,
            }) => {
                set_token_enabled(
                    &registry,
                    connections.as_deref(),
                    &client_connection_id,
                    false,
                )
                .await
            }
            Some(ControlCommand::EnableToken {
                client_connection_id,
            }) => {
                set_token_enabled(
                    &registry,
                    connections.as_deref(),
                    &client_connection_id,
                    true,
                )
                .await
            }
            Some(ControlCommand::CreateConnection { name, client_type }) => {
                match &connections {
                    Some(repo) => match repo
                        .create(mcpane_storage::NewConnection {
                            name,
                            client_type,
                            default_workspace_id: None,
                        })
                        .await
                    {
                        Ok((record, token)) => {
                            hydrate_registry(&registry, repo).await;
                            // 完整 Token 只在此响应中出现一次。
                            serde_json::json!({ "ok": true, "connection": record, "token": token })
                        }
                        Err(error) => {
                            serde_json::json!({ "ok": false, "error": error.to_string() })
                        }
                    },
                    None => serde_json::json!({ "ok": false, "error": "SQLite 不可用" }),
                }
            }
            Some(ControlCommand::RotateToken {
                client_connection_id,
            }) => match &connections {
                Some(repo) => match repo.rotate_token(&client_connection_id).await {
                    Ok(token) => {
                        hydrate_registry(&registry, repo).await;
                        serde_json::json!({ "ok": true, "clientConnectionId": client_connection_id, "token": token })
                    }
                    Err(error) => {
                        serde_json::json!({ "ok": false, "error": error.to_string() })
                    }
                },
                None => serde_json::json!({ "ok": false, "error": "SQLite 不可用" }),
            },
            Some(ControlCommand::DeleteConnection {
                client_connection_id,
            }) => match &connections {
                Some(repo) => match repo.delete(&client_connection_id).await {
                    Ok(()) => {
                        hydrate_registry(&registry, repo).await;
                        state.clear_connection_runtime_maps(&client_connection_id);
                        hydrate_connection_mappings(&state, repo).await;
                        serde_json::json!({ "ok": true, "clientConnectionId": client_connection_id })
                    }
                    Err(error) => {
                        serde_json::json!({ "ok": false, "error": error.to_string() })
                    }
                },
                None => serde_json::json!({ "ok": false, "error": "SQLite 不可用" }),
            },
            Some(ControlCommand::Diagnostics) => {
                let mut value = state.diagnostics();
                value["serverVersion"] = serde_json::json!(SERVER_VERSION);
                value["protocolVersion"] = serde_json::json!(MCP_PROTOCOL_VERSION);
                value["legacyProtocolVersion"] =
                    serde_json::json!(crate::MCP_LEGACY_PROTOCOL_VERSION);
                value["uptimeSecs"] = serde_json::json!(started_at.elapsed().as_secs());
                value
            }
            Some(ControlCommand::ReloadServers) => match &pool {
                Some(pool) => {
                    load_downstreams(&state, pool).await;
                    serde_json::json!({ "ok": true })
                }
                None => serde_json::json!({ "ok": false, "error": "SQLite 不可用" }),
            },
            Some(ControlCommand::OAuthBegin {
                server_id,
                redirect_uri,
            }) => oauth_begin(&state, &server_id, &redirect_uri).await,
            Some(ControlCommand::OAuthComplete {
                server_id,
                code,
                state: oauth_state,
            }) => match &pool {
                Some(pool) => oauth_complete(&state, pool, &server_id, &code, &oauth_state).await,
                None => serde_json::json!({ "ok": false, "error": "SQLite 不可用" }),
            },
            Some(ControlCommand::OAuthCancel { server_id }) => {
                state.oauth().cancel(&server_id);
                serde_json::json!({ "ok": true })
            }
            Some(ControlCommand::OAuthLogout { server_id }) => match &pool {
                Some(pool) => oauth_logout(&state, pool, &server_id).await,
                None => serde_json::json!({ "ok": false, "error": "SQLite 不可用" }),
            },
            Some(ControlCommand::SetConnectionWorkspace {
                client_connection_id,
                workspace_id,
            }) => match &connections {
                Some(repo) => {
                    let result = repo
                        .set_default_workspace(&client_connection_id, workspace_id.as_deref())
                        .await;
                    match result {
                        Ok(()) => {
                            // 只刷新连接→Workspace 映射，**不**重连下游：绑定变更不改
                            // 任何下游 spec，也不改各 Workspace 的 Catalog（Catalog 是
                            // 谁绑它无关的）。此前走 load_downstreams 会 kill/重拉全部
                            // 下游，打断其它连接在途的 tools/call。新 Session 按新映射
                            // pin，已建 Session 保持原 pin（ADR-004）。
                            hydrate_connection_mappings(&state, repo).await;
                            serde_json::json!({ "ok": true })
                        }
                        Err(error) => {
                            serde_json::json!({ "ok": false, "error": error.to_string() })
                        }
                    }
                }
                None => serde_json::json!({ "ok": false, "error": "SQLite 不可用" }),
            },
            Some(ControlCommand::SetConnectionToolExposure {
                client_connection_id,
                mode,
            }) => match &connections {
                Some(repo) => {
                    // 合法性在应用层收口（DB 无 CHECK，0019 迁移注释）。
                    if !matches!(mode.as_str(), "flat" | "gateway" | "auto") {
                        serde_json::json!({ "ok": false, "error": format!("未知的暴露模式: {mode}") })
                    } else {
                        match repo.set_tool_exposure(&client_connection_id, &mode).await {
                            Ok(()) => {
                                // 与改绑同类的轻量路径：只刷新映射，不重连下游。
                                hydrate_connection_mappings(&state, repo).await;
                                serde_json::json!({ "ok": true })
                            }
                            Err(error) => {
                                serde_json::json!({ "ok": false, "error": error.to_string() })
                            }
                        }
                    }
                }
                None => serde_json::json!({ "ok": false, "error": "SQLite 不可用" }),
            },
            Some(ControlCommand::SetConnectionServerAccess {
                client_connection_id,
                server_id,
                allowed,
            }) => match &connections {
                // 轻量路径：写库 + 增量更新内存映射，不重连下游、不重建 Catalog；
                // 数据面在请求时实时查询，活跃 Session 立即生效。
                Some(repo) => match repo
                    .set_server_access(&client_connection_id, &server_id, allowed)
                    .await
                {
                    Ok(()) => {
                        state.set_server_denied(&client_connection_id, &server_id, !allowed);
                        serde_json::json!({ "ok": true })
                    }
                    Err(error) => {
                        serde_json::json!({ "ok": false, "error": error.to_string() })
                    }
                },
                None => serde_json::json!({ "ok": false, "error": "SQLite 不可用" }),
            },
            Some(ControlCommand::ListCatalog { workspace_id }) => {
                let map = state.pinned_catalog_map();
                let workspace =
                    workspace_id.unwrap_or_else(|| crate::state::DEFAULT_WORKSPACE.to_owned());
                match map.get(&workspace) {
                    Some(revision) => serde_json::to_value(&**revision).unwrap_or_else(
                        |e| serde_json::json!({ "ok": false, "error": e.to_string() }),
                    ),
                    None => serde_json::json!({ "ok": false, "error": "Workspace 无 Catalog" }),
                }
            }
            Some(ControlCommand::TestServer {
                transport,
                command,
                args,
                env,
                cwd,
                url,
                bearer,
                bearer_env,
                headers,
            }) => {
                // 用与真实连接同一份代理配置：测试若走另一条出网路径，
                // 「测得通、连不上」会让用户彻底失去判断依据。
                test_server(
                    &transport,
                    command,
                    args,
                    env,
                    cwd,
                    url,
                    bearer,
                    bearer_env,
                    headers,
                    &state.proxy(),
                )
                .await
            }
            None => serde_json::json!({ "ok": false, "error": "无法解析控制命令" }),
        }
    };

    let mut payload = match serde_json::to_vec(&response) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::warn!(%error, "控制面响应序列化失败");
            return;
        }
    };
    // 换行是帧结束标志：客户端据此判完，不必靠对端关闭——这也是下面等待对端
    // 关闭不会双向死等的前提。
    payload.push(b'\n');
    let inner = stream.get_mut();
    if let Err(error) = inner.write_all(&payload).await {
        tracing::warn!(%error, "控制面响应写入失败");
        return;
    }
    if let Err(error) = inner.flush().await {
        tracing::warn!(%error, "控制面响应 flush 失败");
        return;
    }
    #[cfg(windows)]
    wait_for_peer_close(inner).await;
    let _ = inner.shutdown().await;
}

/// Windows 专属：写完响应后等客户端读走再释放管道句柄。
///
/// 命名管道关闭实例会**丢弃缓冲区里未被读走的数据**（Win32 `DisconnectNamedPipe`
/// 文档），而 tokio 的 `NamedPipeServer::poll_shutdown` 是空实现、Drop 只
/// `CloseHandle`——写完就返回会让响应蒸发，客户端读到 0 字节报「EOF while
/// parsing a value at line 1 column 0」，命令其实早已执行完（假失败）。
///
/// 官方给重叠 I/O 服务端的收尾就是等对端关闭（读到 EOF 或管道错误），而不是
/// `FlushFileBuffers`（会阻塞线程）。客户端按行读、读完即关，正常路径立即返回。
#[cfg(windows)]
async fn wait_for_peer_close<S>(stream: &mut S)
where
    S: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    /// 防呆上限：客户端要么读完就关，要么自己超时退出（读立刻拿到 BROKEN_PIPE）。
    const PEER_CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
    let mut scratch = [0u8; 64];
    let _ = tokio::time::timeout(PEER_CLOSE_TIMEOUT, async {
        while let Ok(n) = stream.read(&mut scratch).await {
            if n == 0 {
                break;
            }
        }
    })
    .await;
}

/// OAuth 第一段：发现授权服务器、动态注册，返回要在浏览器里打开的授权链接。
pub async fn oauth_begin(
    state: &crate::state::AggregationState,
    server_id: &str,
    redirect_uri: &str,
) -> serde_json::Value {
    let Some(server) = state.registered(server_id) else {
        return serde_json::json!({ "ok": false, "error": "找不到该服务器（未启用或不存在）" });
    };
    let mcpane_hub_downstream::SpawnSpec::Http { url, .. } = &server.spec else {
        return serde_json::json!({ "ok": false, "error": "只有远程 HTTP 服务器支持 OAuth 登录" });
    };
    match state
        .oauth()
        .begin(
            server_id,
            url,
            server.auth_challenge(),
            redirect_uri,
            &state.proxy(),
        )
        .await
    {
        Ok(authorization_url) => {
            serde_json::json!({ "ok": true, "authorizationUrl": authorization_url })
        }
        Err(error) => serde_json::json!({ "ok": false, "error": error.to_string() }),
    }
}

/// OAuth 第二段：凭回调的 code 换令牌，成功后重连该服务器（它此刻是 needs-auth，
/// 不在「Ready 才保留」之列，重载会重新连接）。
pub async fn oauth_complete(
    state: &crate::state::AggregationState,
    pool: &mcpane_storage::SqlitePool,
    server_id: &str,
    code: &str,
    oauth_state: &str,
) -> serde_json::Value {
    match state.oauth().complete(server_id, code, oauth_state).await {
        Ok(()) => {
            load_downstreams(state, pool).await;
            let status = state.registered(server_id).map(|s| s.status());
            serde_json::json!({ "ok": true, "status": status })
        }
        Err(error) => serde_json::json!({ "ok": false, "error": error.to_string() }),
    }
}

/// 退出登录：删令牌、丢进行中的登录，并让该服务器回到「需要登录」。
pub async fn oauth_logout(
    state: &crate::state::AggregationState,
    pool: &mcpane_storage::SqlitePool,
    server_id: &str,
) -> serde_json::Value {
    state.oauth().logout(server_id);
    // 先注销再重载：重载对「规格没变且 Ready」的下游会原样保留旧连接，
    // 那条连接手里还拿着刚被删掉的令牌。
    state.unregister_server(server_id);
    load_downstreams(state, pool).await;
    serde_json::json!({ "ok": true })
}

/// 测试一份 Server 配置：connect + tools/list，成功返回工具名列表，测完即关。
/// 不落库、不注册进聚合状态；短超时避免拖住控制面连接。
#[allow(clippy::too_many_arguments)]
pub async fn test_server(
    transport: &str,
    command: Option<String>,
    args: Vec<String>,
    env: std::collections::BTreeMap<String, String>,
    cwd: Option<String>,
    url: Option<String>,
    bearer: Option<String>,
    bearer_env: Option<String>,
    headers: std::collections::BTreeMap<String, String>,
    proxy: &mcpane_net::ProxyConfig,
) -> serde_json::Value {
    let spec = match transport {
        "stdio" => {
            let Some(command) = command.filter(|c| !c.is_empty()) else {
                return serde_json::json!({ "ok": false, "error": "stdio Server 缺少 command" });
            };
            mcpane_hub_downstream::SpawnSpec::Stdio {
                command,
                args,
                env,
                cwd,
            }
        }
        "http" => {
            let Some(url) = url.filter(|u| !u.is_empty()) else {
                return serde_json::json!({ "ok": false, "error": "HTTP Server 缺少 url" });
            };
            mcpane_hub_downstream::SpawnSpec::Http {
                url,
                bearer,
                bearer_env,
                env,
                headers,
                // 测试的是尚未保存的配置，没有 Server id，也就没有令牌。
                oauth: None,
            }
        }
        "sse" => {
            let Some(url) = url.filter(|u| !u.is_empty()) else {
                return serde_json::json!({ "ok": false, "error": "SSE Server 缺少 url" });
            };
            mcpane_hub_downstream::SpawnSpec::Sse {
                url,
                bearer,
                bearer_env,
                env,
                headers,
            }
        }
        other => {
            return serde_json::json!({ "ok": false, "error": format!("未知 transport: {other}") });
        }
    };
    let started = std::time::Instant::now();
    let handle = match mcpane_hub_downstream::DownstreamHandle::connect(
        &spec,
        std::time::Duration::from_secs(15),
        proxy,
    )
    .await
    {
        Ok(handle) => handle,
        Err(error) => {
            return serde_json::json!({ "ok": false, "error": error.to_string() });
        }
    };
    let result = match handle.list_tools().await {
        Ok(tools) => serde_json::json!({
            "ok": true,
            "toolCount": tools.len(),
            "tools": tools.iter().map(|t| t.name.to_string()).collect::<Vec<_>>(),
            "elapsedMs": started.elapsed().as_millis() as u64,
        }),
        Err(error) => serde_json::json!({ "ok": false, "error": error.to_string() }),
    };
    handle.close().await;
    result
}

/// 仅创建者（OW）与 SYSTEM（SY）可访问的安全描述符，用于控制面管道。
///
/// 不能用默认安全描述符：`CreateNamedPipeW` 官方文档明写，`lpSecurityAttributes`
/// 为 NULL 时默认 ACL 除给 LocalSystem/Administrators/创建者完全控制外，**还给
/// Everyone 组和匿名账户读权限**；且"若 server 服务在跑，所有命名管道都可被远程
/// 访问"。控制面暴露的是 rotate-token / revoke-token / diagnostics，同机其他账户能
/// 连上就足以反复占住空闲实例把桌面端挤掉（本地 DoS）。
///
/// SDDL 与 stacklok/toolhive（同为本机 MCP 管理工具）已合并的实现一致：
/// `P` 阻断继承放宽，`GA` = GENERIC_ALL。刻意不含 Administrators——Unix 侧的
/// UDS 是 0600，也没有"管理员组直通"这一说，两端语义才对得上。
/// 安全描述符不跨 await 持有——它内部是裸指针、不是 `Send`，而本函数的调用方
/// 是被 `tokio::spawn` 的 async fn。每次建实例现解析一次 SDDL，只发生在有新连接
/// 到来时，开销可忽略。
#[cfg(windows)]
fn create_owner_only_pipe(
    options: &mut tokio::net::windows::named_pipe::ServerOptions,
    name: &str,
) -> anyhow::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    let sd =
        WindowsSecurityDescriptor::from_sddl(windows::core::w!("D:P(A;;GA;;;OW)(A;;GA;;;SY)"))?;
    Ok(sd.create_pipe(options, name)?)
}

/// SDDL 解析出的安全描述符，Drop 时 `LocalFree`。
#[cfg(windows)]
struct WindowsSecurityDescriptor(windows::Win32::Security::PSECURITY_DESCRIPTOR);

#[cfg(windows)]
impl WindowsSecurityDescriptor {
    // 豁免理由：设置管道 DACL 只有 Win32 FFI 一条路——tokio 至今没提供安全封装
    // （tokio-rs/tokio#6128 仍未解决）。unsafe 收敛在本 impl 的三处调用。
    #![allow(unsafe_code)]

    fn from_sddl(sddl: windows::core::PCWSTR) -> anyhow::Result<Self> {
        use windows::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        let mut psd = windows::Win32::Security::PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl,
                SDDL_REVISION_1,
                &mut psd,
                None,
            )?;
        }
        Ok(Self(psd))
    }

    /// 带本安全描述符创建管道实例。
    ///
    /// `SECURITY_ATTRIBUTES` 只需在调用期间存活：`CreateNamedPipeW` 会把描述符
    /// 拷进内核对象，返回后指针无需保持有效（Win32 标准约定）。
    fn create_pipe(
        &self,
        options: &mut tokio::net::windows::named_pipe::ServerOptions,
        name: &str,
    ) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
        let mut attrs = windows::Win32::Security::SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<windows::Win32::Security::SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0.0,
            bInheritHandle: false.into(),
        };
        unsafe {
            options.create_with_security_attributes_raw(
                name,
                std::ptr::from_mut(&mut attrs).cast::<std::ffi::c_void>(),
            )
        }
    }
}

#[cfg(windows)]
impl Drop for WindowsSecurityDescriptor {
    fn drop(&mut self) {
        #![allow(unsafe_code)]
        use windows::Win32::Foundation::{HLOCAL, LocalFree};
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0.0)));
        }
    }
}

/// Windows：Named Pipe 控制面（对应 Unix 的 UDS）。
///
/// 与 Unix 端语义对齐：单实例锁靠数据面 TCP 端口（main.rs 先 bind），故这里
/// 直接创建管道 server；每来一个连接就先备好下一个 server 实例再 spawn 处理，
/// 保证并发连接都能连上、且慢命令不堵后续（与 UDS 端 spawn-per-conn 一致）。
/// 访问权经显式安全描述符收紧到创建者与 SYSTEM（见 [`owner_only_sd`]）。
#[cfg(windows)]
pub async fn serve_control_status(
    registry: Arc<TokenRegistry>,
    state: Arc<crate::state::AggregationState>,
    pool: Option<mcpane_storage::SqlitePool>,
    endpoint: String,
    started_at: std::time::Instant,
    shutdown: Arc<tokio::sync::Notify>,
) -> anyhow::Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let connections = pool
        .clone()
        .map(|p| Arc::new(mcpane_storage::ConnectionRepo::new(p)));
    let pipe_name = mcpane_hub_protocol::control_pipe_name();

    // 首个实例带 first_pipe_instance：若已有同名管道在服务（另一活实例），
    // 这里会失败——与 UDS 端「对端可连通即 bail」同义的单实例保护。
    let mut server =
        create_owner_only_pipe(ServerOptions::new().first_pipe_instance(true), &pipe_name)?;
    tracing::info!(pipe = %pipe_name, "控制面状态端点已启动 (Named Pipe)");

    loop {
        // 等待客户端连上当前实例。
        if let Err(error) = server.connect().await {
            tracing::warn!(%error, "控制面管道 connect 失败，稍后继续");
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            continue;
        }
        // 已连上的实例交给处理任务；立刻另建一个实例以接纳下一个客户端。
        let connected = server;
        server = match create_owner_only_pipe(&mut ServerOptions::new(), &pipe_name) {
            Ok(next) => next,
            Err(error) => {
                tracing::error!(%error, "控制面无法新建管道实例，端点退出");
                return Err(error);
            }
        };
        tokio::spawn(handle_control_connection(
            connected,
            ControlDeps {
                registry: registry.clone(),
                state: state.clone(),
                pool: pool.clone(),
                connections: connections.clone(),
                endpoint: endpoint.clone(),
                started_at,
                read_timeout: CONTROL_READ_TIMEOUT,
                shutdown: Some(shutdown.clone()),
            },
        ));
    }
}

/// SDDL 语法错了，`ConvertStringSecurityDescriptorToSecurityDescriptorW` 会失败，
/// 控制面整个起不来——而那只在 Windows 运行时才暴露。这条让 Windows CI 拦住它。
#[cfg(all(test, windows))]
#[test]
fn owner_only_sddl_is_accepted_by_win32() {
    let result =
        WindowsSecurityDescriptor::from_sddl(windows::core::w!("D:P(A;;GA;;;OW)(A;;GA;;;SY)"))
            .map(|_| ());
    assert!(
        result.is_ok(),
        "控制面 SDDL 必须能被 Win32 解析：{:?}",
        result.err()
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::auth::ClientIdentity;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    /// 按行读回响应，与真实客户端同一套读法（`read_to_end` 会和服务端的
    /// `wait_for_peer_close` 互相等到超时）。
    async fn read_response_line<S>(stream: S) -> serde_json::Value
    where
        S: tokio::io::AsyncRead + Unpin,
    {
        let mut reader = tokio::io::BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(
            line.ends_with('\n'),
            "控制面响应必须以换行收尾（客户端据此判帧）：{line:?}"
        );
        serde_json::from_str(line.trim()).unwrap()
    }

    /// 回归（rust-gate 实证）：一条格式完全合法、只是被系统繁忙/内核分片
    /// 拖成多次小写入的命令，必须被完整读到并真正执行——不得因为拼够一整行
    /// 的耗时较长就被误判成超时、更不能被误判成 Status 探测悄悄放过。
    ///
    /// 用比历史 200ms 判定阈值更长的总耗时（3 段 × 80ms ≈ 240ms）验证：只要
    /// 仍在本次调用传入的 `read_timeout`（500ms）窗口内，读取必须完整，
    /// revoke-token 必须真正生效（token 立即失效），响应也必须是
    /// revoke-token 自己的形状，而非 Status 响应（后者带 `connections` 字段）。
    #[tokio::test]
    async fn chunked_slow_command_is_read_in_full_and_actually_executed() {
        let registry = Arc::new(TokenRegistry::new());
        let secret = "secret-token-for-chunk-test";
        registry.register(
            secret,
            ClientIdentity::from_raw("client_chunk_test".into(), "claude"),
        );
        let state = Arc::new(crate::state::AggregationState::new(None));

        let (mut client, server) = tokio::io::duplex(4096);
        let handle = tokio::spawn(handle_control_connection(
            server,
            ControlDeps {
                registry: registry.clone(),
                state,
                pool: None,
                connections: None,
                endpoint: "http://127.0.0.1:24860/mcp".into(),
                started_at: std::time::Instant::now(),
                read_timeout: std::time::Duration::from_millis(500),
                shutdown: None,
            },
        ));

        let full = r#"{"cmd":"revoke-token","client_connection_id":"client_chunk_test"}"#
            .to_owned()
            + "\n";
        let (p1, rest) = full.split_at(10);
        let (p2, rest) = rest.split_at(rest.len() / 3);
        let (p3, p4) = rest.split_at(rest.len() / 2);
        // 4 段 × 3 个 80ms 间隔 ≈ 240ms：明确超过历史版本写死的 200ms 判定阈值，
        // 而不是恰好卡在临界值附近——确保这条测试真的在验证"曾经会踩坑的时长"。
        for chunk in [p1, p2, p3, p4] {
            client.write_all(chunk.as_bytes()).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        }

        let response = read_response_line(&mut client).await;
        handle.await.unwrap();

        assert_eq!(response["ok"], serde_json::json!(true), "{response}");
        assert_eq!(response["clientConnectionId"], "client_chunk_test");
        assert!(
            response.get("connections").is_none(),
            "分片慢发的合法命令被误判成了 Status 响应：{response}"
        );
        assert!(
            registry.verify(secret).is_none(),
            "revoke-token 必须真正执行：token 应立即失效，而不是被当作 Status 探测放行"
        );
    }

    /// 回归：真正的读超时必须返回明确错误，绝不能伪装成 Status——此前
    /// 超时与"空行/EOF"共用同一个兜底分支，调用方会拿到看似正常的 Status
    /// 响应，误以为自己发的命令已经生效。
    #[tokio::test]
    async fn genuine_read_timeout_yields_explicit_error_not_status() {
        let registry = Arc::new(TokenRegistry::new());
        let state = Arc::new(crate::state::AggregationState::new(None));

        let (mut client, server) = tokio::io::duplex(256);
        let handle = tokio::spawn(handle_control_connection(
            server,
            ControlDeps {
                registry,
                state,
                pool: None,
                connections: None,
                endpoint: "http://127.0.0.1:24860/mcp".into(),
                started_at: std::time::Instant::now(),
                read_timeout: std::time::Duration::from_millis(50),
                shutdown: None,
            },
        ));

        // 只发一半、故意不发换行符：连接没断，但一整行永远拼不齐。
        client.write_all(b"{\"cmd\":\"revoke-tok").await.unwrap();

        let response = read_response_line(&mut client).await;
        handle.await.unwrap();

        assert_eq!(response["ok"], serde_json::json!(false), "{response}");
        let message = response["error"].as_str().unwrap_or_default();
        assert!(
            message.contains("超时"),
            "超时必须给出明确的超时错误，而不是猜：{response}"
        );
        assert!(
            response.get("connections").is_none(),
            "超时不得被伪装成 Status 响应：{response}"
        );
    }

    /// 回归（Windows 实证 2026-08-04）：服务端写完不得立刻释放管道句柄，否则
    /// 未读数据被丢弃，客户端读到 0 字节报 EOF——而命令早已执行完（发放 Token
    /// 报错、Token 却真发出去了）。客户端故意晚 300ms 读：修复前必丢。
    #[cfg(windows)]
    #[tokio::test]
    async fn windows_response_survives_a_slow_client_read() {
        use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};

        let pipe_name = r"\\.\pipe\mcpane-control-slow-client-read-test";
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(pipe_name)
            .unwrap();
        let mut client = ClientOptions::new().open(pipe_name).unwrap();
        server.connect().await.unwrap();

        let handle = tokio::spawn(handle_control_connection(
            server,
            ControlDeps {
                registry: Arc::new(TokenRegistry::new()),
                state: Arc::new(crate::state::AggregationState::new(None)),
                pool: None,
                connections: None,
                endpoint: "http://127.0.0.1:24860/mcp".into(),
                started_at: std::time::Instant::now(),
                read_timeout: CONTROL_READ_TIMEOUT,
                shutdown: None,
            },
        ));

        client.write_all(b"{\"cmd\":\"status\"}\n").await.unwrap();
        // 服务端此时早已把响应写完；晚读模拟真实的调度抖动。
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let response = read_response_line(&mut client).await;
        assert_eq!(response["name"], "mcpane-hub", "{response}");

        // 客户端关闭是服务端等待的结束信号；不关会挂到 PEER_CLOSE_TIMEOUT。
        drop(client);
        handle.await.unwrap();
    }

    async fn create_test_connection(
        repo: &mcpane_storage::ConnectionRepo,
    ) -> (mcpane_storage::ClientConnection, String) {
        repo.create(mcpane_storage::NewConnection {
            name: "revoke-test".into(),
            client_type: "claude".into(),
            default_workspace_id: None,
        })
        .await
        .unwrap()
    }

    /// 回归：撤销 Token 时 DB 写失败必须整体失败、内存不动。此前 DB 错误被
    /// `let _ =` 吞掉且内存先改——UI 显示撤销成功，库里没落，hub 一重启
    /// `hydrate_registry` 就把 Token 复活。
    /// （反向验证过：把实现改回「先动内存、吞 DB 错」，本测试立即红。）
    #[tokio::test]
    async fn revoke_token_db_failure_reports_error_and_leaves_token_active() {
        let pool = mcpane_storage::open_in_memory().await.unwrap();
        let repo = mcpane_storage::ConnectionRepo::new(pool.clone());
        let (record, token) = create_test_connection(&repo).await;
        let registry = TokenRegistry::new();
        hydrate_registry(&registry, &repo).await;
        assert!(registry.verify(&token).is_some(), "前提：token 有效");

        // 关闭连接池模拟 DB 故障：后续写入立即报错。
        pool.close().await;

        let response = set_token_enabled(&registry, Some(&repo), &record.id, false).await;
        assert_eq!(response["ok"], serde_json::json!(false), "{response}");
        assert!(
            response["error"]
                .as_str()
                .unwrap_or_default()
                .contains("数据库写入失败"),
            "{response}"
        );
        assert!(
            registry.verify(&token).is_some(),
            "DB 未落时内存不得单方面撤销——否则重启后状态回跳，两边必须一致"
        );
    }

    /// 撤销成功路径：内存立即拒绝，且落库——重启（重水合）后不复活。
    #[tokio::test]
    async fn revoke_token_persists_across_rehydration() {
        let pool = mcpane_storage::open_in_memory().await.unwrap();
        let repo = mcpane_storage::ConnectionRepo::new(pool.clone());
        let (record, token) = create_test_connection(&repo).await;
        let registry = TokenRegistry::new();
        hydrate_registry(&registry, &repo).await;

        let response = set_token_enabled(&registry, Some(&repo), &record.id, false).await;
        assert_eq!(response["ok"], serde_json::json!(true), "{response}");
        assert!(registry.verify(&token).is_none(), "内存立即失效");

        // 等价于 hub 重启：全新 registry 从 DB 水合，撤销必须还在。
        let rehydrated = TokenRegistry::new();
        hydrate_registry(&rehydrated, &repo).await;
        assert!(
            rehydrated.verify(&token).is_none(),
            "撤销未落库：重启后 Token 复活"
        );
    }

    /// 未知连接 id：明确报「连接不存在」，而不是静默 ok:false。
    #[tokio::test]
    async fn revoke_unknown_connection_reports_missing() {
        let pool = mcpane_storage::open_in_memory().await.unwrap();
        let repo = mcpane_storage::ConnectionRepo::new(pool.clone());
        let registry = TokenRegistry::new();
        let response = set_token_enabled(&registry, Some(&repo), "no-such-id", false).await;
        assert_eq!(response["ok"], serde_json::json!(false), "{response}");
        assert!(
            response["error"]
                .as_str()
                .unwrap_or_default()
                .contains("连接不存在"),
            "{response}"
        );
    }

    /// 超长单行命令被立即拒绝：不换行地灌数据不再无界累积到内存、
    /// 也不用等到读超时才有响应。
    #[tokio::test]
    async fn oversized_command_line_is_rejected_with_explicit_error() {
        let registry = Arc::new(TokenRegistry::new());
        let state = Arc::new(crate::state::AggregationState::new(None));

        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let handle = tokio::spawn(handle_control_connection(
            server,
            ControlDeps {
                registry,
                state,
                pool: None,
                connections: None,
                endpoint: "http://127.0.0.1:24860/mcp".into(),
                started_at: std::time::Instant::now(),
                read_timeout: CONTROL_READ_TIMEOUT,
                shutdown: None,
            },
        ));

        // 256KB 上限 + 2 字节，全程不发换行。
        let flood = vec![b'a'; 256 * 1024 + 2];
        client.write_all(&flood).await.unwrap();

        let response = read_response_line(&mut client).await;
        handle.await.unwrap();
        assert_eq!(response["ok"], serde_json::json!(false), "{response}");
        assert!(
            response["error"]
                .as_str()
                .unwrap_or_default()
                .contains("过长"),
            "{response}"
        );
    }

    /// 控制面 shutdown 必须先通知主循环、再回 ok：Desktop 靠这条在 Windows 上
    /// 对齐 Unix SIGTERM；通知丢失会让调用方空等 3s 再强杀。
    #[tokio::test]
    async fn shutdown_command_signals_and_returns_ok() {
        let shutdown = Arc::new(tokio::sync::Notify::new());
        let wait = shutdown.notified();
        tokio::pin!(wait);

        let (mut client, server) = tokio::io::duplex(4096);
        let handle = tokio::spawn(handle_control_connection(
            server,
            ControlDeps {
                registry: Arc::new(TokenRegistry::new()),
                state: Arc::new(crate::state::AggregationState::new(None)),
                pool: None,
                connections: None,
                endpoint: "http://127.0.0.1:24860/mcp".into(),
                started_at: std::time::Instant::now(),
                read_timeout: CONTROL_READ_TIMEOUT,
                shutdown: Some(shutdown.clone()),
            },
        ));

        client.write_all(b"{\"cmd\":\"shutdown\"}\n").await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), wait)
                .await
                .is_ok(),
            "shutdown 必须通知主循环"
        );

        let response = read_response_line(&mut client).await;
        drop(client);
        handle.await.unwrap();
        assert_eq!(response["ok"], serde_json::json!(true), "{response}");
    }

    #[test]
    fn status_contains_prefixes_but_never_full_tokens() {
        let registry = TokenRegistry::new();
        let secret = "super-secret-token-value-0123456789abcdef";
        registry.register(
            secret,
            ClientIdentity::from_raw("client_claude".into(), "claude"),
        );
        let status = hub_status(
            &registry,
            "http://127.0.0.1:24860/mcp",
            std::time::Instant::now(),
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
        );
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("client_claude"));
        assert!(json.contains(&secret[..8]));
        assert!(!json.contains(secret));
    }
}
