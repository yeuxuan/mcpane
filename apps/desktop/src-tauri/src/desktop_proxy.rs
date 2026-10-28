//! 本地路由托管（进程内单例）：Claude Desktop 网关 + Claude Code/Codex 路由接管（ADR-022）。
//!
//! 生命周期语义对齐 cc-switch ProxyService，按我们的架构收敛：
//! - **期望态由 provider store 推导**：当前 Desktop 供应商是 proxy 模式中转站，
//!   或任一 app 开启路由且当前为中转站 → 必须在跑且快照一致；都不需要则停机释放端口。
//! - 同步时机：app 启动（setup，早于托盘）、每次供应商切换/编辑/路由配置变更后。
//! - 退出时把接管中的 app 还原为直连（MCPane 不在时客户端仍可用），启动再重新接管。
//! - 上游 Key 从 Keychain 物化后只存于代理内存快照，不落盘（对齐 ADR 凭据边界）。
//!
//! 与 hub sidecar（hub_process.rs）不同，这里是进程内 tokio 任务：Desktop 的
//! profile 指向 loopback 网关，「保持 MCPane 运行」本就是 proxy 模式的产品语义，
//! 独立进程只会增加一层监督复杂度而无收益。

use std::sync::{Mutex, OnceLock};

use std::sync::Arc;

use mcpane_desktop_proxy::{
    AppRoute, AppRouteStatus, DesktopUpstream, ModelSlots, ProxyServer, ProxyState, RequestRecord,
    RouteTuning, RouteUpstream, UpstreamAuth, UpstreamFormat,
};
use mcpane_provider::{
    ClaudeApiFormat, ClaudeAuthField, ClientApp, CodexWireApi, DesktopMode, ExhaustedUntil,
    ProviderCategory, ProviderProfile, RoutingConfig, RoutingStrategy,
};

struct Manager {
    state: ProxyState,
    server: Mutex<Option<ProxyServer>>,
    /// 生命周期互斥：sync/start/stop 全程持有。没有它，两个并发的
    /// `sync_from_store`（provider 编辑与备份恢复完全可能同时收尾）会在
    /// 「检查已运行 → 启动」之间互相穿插，各起一个 server——后者把前者从
    /// 槽里顶掉且不 shutdown，泄漏一个仍在监听的实例或直接撞 EADDRINUSE。
    lifecycle: tokio::sync::Mutex<()>,
}

fn manager() -> &'static Manager {
    static MANAGER: OnceLock<Manager> = OnceLock::new();
    MANAGER.get_or_init(|| Manager {
        state: ProxyState::new(""),
        server: Mutex::new(None),
        lifecycle: tokio::sync::Mutex::new(()),
    })
}

/// 代理运行态（IPC 序列化面）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopProxyStatus {
    pub running: bool,
    /// 实际监听端口（未运行为 null）。
    pub port: Option<u16>,
    /// 是否有生效的上游快照（false = 请求会 503）。
    pub has_upstream: bool,
}

pub fn status() -> DesktopProxyStatus {
    let mgr = manager();
    let port = mgr
        .server
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(ProxyServer::port));
    DesktopProxyStatus {
        running: port.is_some(),
        port,
        has_upstream: mgr.state.has_upstream(),
    }
}

/// 是否运行中（status 命令与端口修改校验共用）。
pub fn is_running() -> bool {
    status().running
}

/// 按 provider store 现状同步代理（阻塞版；托盘/启动等同步上下文用）。
/// 返回失败原因，呈现渠道由调用方定：切换命令进 warnings，启动路径记日志。
/// 此前失败一律只 eprintln，Claude Desktop 拿到与网关不一致的 token 后
/// 用户只会看到「无效的本地网关 token」，无从归因。
pub fn sync_from_store_blocking() -> Result<(), String> {
    tauri::async_runtime::block_on(sync_from_store()).map(|_| ())
}

/// 按 provider store 现状同步代理：
/// 当前 Desktop 供应商为 proxy 模式 → 确保运行 + 刷新上游快照；否则停机。
pub async fn sync_from_store() -> Result<DesktopProxyStatus, String> {
    let _lifecycle = manager().lifecycle.lock().await;
    sync_from_store_locked().await
}

/// 前置条件：调用方已持 lifecycle 锁。
///
/// 守住不变量「客户端配置指向路由 ⇒ 路由在跑」：同步后仍有接管需求却没在监听
///（端口被占、上游 Key 读不出等），把接管中的客户端回退直连并记下原因。
async fn sync_from_store_locked() -> Result<DesktopProxyStatus, String> {
    let result = sync_inner().await;
    let mgr = manager();
    let routes_unserved = mgr.state.has_routes() && !is_running();
    if let Err(error) = &result
        && routes_unserved
    {
        mgr.state.clear_routes();
        let fallback = tauri::async_runtime::spawn_blocking(|| {
            let svc = crate::commands::provider_service_for_desktop_proxy();
            for app in svc.routed_apps().unwrap_or_default() {
                if let Err(e) = crate::commands::restore_direct_unguarded(app) {
                    eprintln!("[mcpane] {} 回退直连失败: {e}", app.as_str());
                }
            }
        })
        .await;
        if fallback.is_err() {
            eprintln!("[mcpane] 回退直连任务异常退出");
        }
        eprintln!("[mcpane] 本地路由未能启动，已回退直连: {error}");
    }
    set_last_error(result.as_ref().err().cloned());
    result
}

async fn sync_inner() -> Result<DesktopProxyStatus, String> {
    let (desktop, routes) =
        tauri::async_runtime::spawn_blocking(|| (desired_upstream(), desired_routes()))
            .await
            .map_err(|e| e.to_string())?;
    let mgr = manager();
    let routes = routes?;
    for (app, route) in &routes {
        mgr.state.set_route(*app, route.clone());
    }
    let needs_routes = routes.iter().any(|(_, route)| route.is_some());
    // Desktop 上游推导失败不连累其他客户端的路由：先把路由服务起来，最后再报错。
    let (desktop, desktop_error) = match desktop {
        Ok(desktop) => (desktop, None),
        Err(error) => (None, Some(error)),
    };
    match desktop {
        Some((gateway_token, listen_port, upstream)) => {
            mgr.state.set_gateway_token(gateway_token);
            mgr.state.set_upstream(Some(upstream));
            ensure_running(listen_port).await?;
        }
        None => {
            mgr.state.set_upstream(None);
            if needs_routes {
                let gateway = tauri::async_runtime::spawn_blocking(ensure_gateway)
                    .await
                    .map_err(|e| e.to_string())??;
                mgr.state.set_gateway_token(gateway.token);
                ensure_running(gateway.listen_port).await?;
            } else {
                stop_server().await;
            }
        }
    }
    match desktop_error {
        Some(error) => Err(error),
        None => Ok(status()),
    }
}

fn last_error_slot() -> &'static Mutex<Option<String>> {
    static LAST_ERROR: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    LAST_ERROR.get_or_init(|| Mutex::new(None))
}

fn set_last_error(error: Option<String>) {
    if let Ok(mut slot) = last_error_slot().lock() {
        *slot = error;
    }
}

/// 最近一次同步失败原因（成功后清空），其次是启动接管失败，界面据此提示。
pub fn last_error() -> Option<String> {
    last_error_slot()
        .lock()
        .ok()
        .and_then(|slot| slot.clone())
        .or_else(|| {
            takeover_errors().lock().ok().and_then(|errors| {
                errors
                    .first()
                    .map(|(app, error)| format!("{} 未能接管，当前保持直连：{error}", app.as_str()))
            })
        })
}

fn ensure_gateway() -> Result<mcpane_provider::DesktopGateway, String> {
    crate::commands::provider_service_for_desktop_proxy()
        .ensure_desktop_gateway()
        .map_err(|e| e.to_string())
}

/// 路由接管中各 app 的候选健康状态。
pub fn route_status() -> Vec<AppRouteStatus> {
    manager().state.route_status()
}

/// 无重置时间的用尽态（余额耗尽等）的信号寿命：后台每 5 分钟刷新一次，留三轮余量，
/// 刷新不到（凭据失效、网络断）时信号自行过期，不会把供应商永久后置。
const QUOTA_SIGNAL_TTL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// 按额度缓存重建路由的「额度用尽」信号并整体替换（额度写入后调用）。
pub fn push_quota_signals() {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let now = std::time::Instant::now();
    let signals = crate::quota_cache::snapshot()
        .into_iter()
        .filter_map(|(id, quota)| {
            let until = match quota.exhausted_until(now_ms)? {
                ExhaustedUntil::At(reset_ms) => {
                    now + std::time::Duration::from_millis((reset_ms - now_ms).max(0) as u64)
                }
                ExhaustedUntil::NextRefresh => now + QUOTA_SIGNAL_TTL,
            };
            Some((id, until))
        })
        .collect();
    manager().state.set_quota_exhausted(signals);
}

/// 开启了「额度优先」的路由里的全部候选（当前项 + 故障队列）：后台刷新额度的范围。
pub fn quota_aware_candidate_ids() -> Vec<String> {
    let svc = crate::commands::provider_service_for_desktop_proxy();
    let (Ok(current), Ok(profiles)) = (svc.current(), svc.list()) else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    for app in ClientApp::ALL
        .into_iter()
        .filter(|app| app.supports_routing())
    {
        let Ok(routing) = svc.routing(app) else {
            continue;
        };
        if !routing.enabled || routing.strategy != RoutingStrategy::QuotaAware {
            continue;
        }
        ids.extend(current.get(&app).cloned());
        ids.extend(routing.failover_queue);
    }
    // 官方登录不查额度（也不经路由）；额度信号只针对中转站。
    ids.retain(|id| {
        profiles
            .iter()
            .any(|p| p.id == *id && p.category == ProviderCategory::Relay)
    });
    ids.sort();
    ids.dedup();
    ids
}

/// 手动恢复被熔断的供应商。
pub fn reset_breaker(app: ClientApp, provider_id: &str) {
    manager().state.reset_breaker(app, provider_id);
}

/// 推导各路由 app 的快照：开启路由且当前为中转站 → 当前项 + 故障转移队列；否则 None。
fn desired_routes() -> Result<Vec<(ClientApp, Option<AppRoute>)>, String> {
    let svc = crate::commands::provider_service_for_desktop_proxy();
    let current = svc.current().map_err(|e| e.to_string())?;
    let profiles = svc.list().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for app in ClientApp::ALL
        .into_iter()
        .filter(|app| app.supports_routing())
    {
        let routing: RoutingConfig = svc.routing(app).map_err(|e| e.to_string())?;
        let primary = current
            .get(&app)
            .and_then(|id| profiles.iter().find(|p| p.id == *id))
            .filter(|p| p.category == ProviderCategory::Relay);
        let route = match primary {
            Some(primary) if routing.enabled => {
                let mut candidates = vec![route_upstream(&svc, primary)?];
                for id in routing
                    .failover_queue
                    .iter()
                    .filter(|id| **id != primary.id)
                {
                    let Some(profile) = profiles.iter().find(|p| p.id == *id) else {
                        continue;
                    };
                    // 备用项缺地址/Key 不阻断主链路，只是不参与转移。
                    match route_upstream(&svc, profile) {
                        Ok(upstream) => candidates.push(upstream),
                        Err(error) => eprintln!("[mcpane] 故障转移候选已跳过：{error}"),
                    }
                }
                Some(AppRoute {
                    candidates,
                    tuning: route_tuning(&routing.tuning),
                    strategy: routing.strategy,
                })
            }
            _ => None,
        };
        out.push((app, route));
    }
    Ok(out)
}

fn route_tuning(tuning: &mcpane_provider::RoutingTuning) -> RouteTuning {
    let secs = |value: u32| std::time::Duration::from_secs(u64::from(value));
    RouteTuning {
        first_byte: secs(tuning.first_byte_timeout_secs),
        idle: secs(tuning.idle_timeout_secs),
        failure_threshold: tuning.failure_threshold,
        cooldown: secs(tuning.cooldown_secs),
    }
}

fn route_upstream(
    svc: &crate::commands::ProviderServiceHandle,
    profile: &ProviderProfile,
) -> Result<Arc<RouteUpstream>, String> {
    let base_url = profile
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("中转站「{}」缺少接口地址", profile.name))?;
    let key = svc
        .reveal_key(&profile.id)
        .map_err(|e| format!("中转站「{}」的 API Key 读取失败：{e}", profile.name))?;
    let models = if profile.app == ClientApp::Claude {
        ModelSlots::resolve(
            profile.model.clone(),
            profile.model_haiku.clone(),
            profile.model_sonnet.clone(),
            profile.model_opus.clone(),
            profile.model_fable.clone(),
        )
    } else {
        ModelSlots {
            main: profile.model.clone(),
            ..ModelSlots::default()
        }
    };
    Ok(Arc::new(RouteUpstream {
        provider_id: profile.id.clone(),
        name: profile.name.clone(),
        base_url: base_url.trim_end_matches('/').to_owned(),
        api_key: key.expose().to_owned(),
        auth: match profile.app {
            // Claude 与接 Anthropic 端点的 Codex 都按「认证字段」选 x-api-key / Bearer。
            ClientApp::Claude | ClientApp::Codex
                if profile.api_key_field == ClaudeAuthField::ApiKey
                    && (profile.app == ClientApp::Claude
                        || profile.codex_wire_api == CodexWireApi::Anthropic) =>
            {
                UpstreamAuth::XApiKey
            }
            ClientApp::Gemini => UpstreamAuth::XGoogApiKey,
            _ => UpstreamAuth::Bearer,
        },
        models,
        format: match profile.app {
            ClientApp::Codex => match profile.codex_wire_api {
                CodexWireApi::Responses => UpstreamFormat::Native,
                CodexWireApi::Chat => UpstreamFormat::OpenaiChat,
                CodexWireApi::Anthropic => UpstreamFormat::Anthropic,
            },
            ClientApp::Claude => claude_format(profile.claude_api_format),
            _ => UpstreamFormat::Native,
        },
        headers: profile
            .headers
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect(),
        body_override: Some(profile.route_body_override.clone()).filter(|v| v.is_object()),
    }))
}

fn claude_format(format: ClaudeApiFormat) -> UpstreamFormat {
    match format {
        ClaudeApiFormat::Anthropic => UpstreamFormat::Native,
        ClaudeApiFormat::OpenaiChat => UpstreamFormat::OpenaiChat,
        ClaudeApiFormat::OpenaiResponses => UpstreamFormat::OpenaiResponses,
    }
}

/// 最近的路由请求（新 → 旧）。
pub fn recent_requests(app: Option<ClientApp>, limit: usize) -> Vec<RequestRecord> {
    manager().state.recent_requests(app, limit)
}

/// 清空路由请求记录（`app` 为空清全部）。
pub fn clear_requests(app: Option<ClientApp>) {
    manager().state.clear_requests(app);
}

/// 启动时先清理强杀残留（未开路由却仍指向路由的 live），再对开启路由的 app
/// 重新接管（上次退出已还原为直连）。接管失败时客户端保持直连，原因留给界面展示。
pub fn reapply_routing_on_startup() {
    for app in ClientApp::ALL
        .into_iter()
        .filter(|app| app.supports_routing())
    {
        match crate::commands::heal_stale_takeover(app) {
            Ok(true) => eprintln!("[mcpane] {} 发现残留的路由接管，已还原直连", app.as_str()),
            Ok(false) => {}
            Err(error) => eprintln!("[mcpane] {} 残留接管清理失败: {error}", app.as_str()),
        }
    }
    let svc = crate::commands::provider_service_for_desktop_proxy();
    for app in svc.routed_apps().unwrap_or_default() {
        record_takeover(app, crate::commands::reapply_routing(app));
    }
}

fn takeover_errors() -> &'static Mutex<Vec<(ClientApp, String)>> {
    static ERRORS: OnceLock<Mutex<Vec<(ClientApp, String)>>> = OnceLock::new();
    ERRORS.get_or_init(|| Mutex::new(Vec::new()))
}

/// 记录某 app 接管结果：失败留存原因，成功（含用户重新开关路由）清除。
pub fn record_takeover(app: ClientApp, result: Result<(), String>) {
    if let Err(error) = &result {
        eprintln!("[mcpane] {} 本地路由接管失败: {error}", app.as_str());
    }
    if let Ok(mut errors) = takeover_errors().lock() {
        errors.retain(|(a, _)| *a != app);
        if let Err(error) = result {
            errors.push((app, error));
        }
    }
}

/// 退出前把接管中的 app 还原为直连，MCPane 不在时客户端仍能工作。
pub fn restore_direct_on_exit() {
    let svc = crate::commands::provider_service_for_desktop_proxy();
    for app in svc.routed_apps().unwrap_or_default() {
        if let Err(error) = crate::commands::restore_direct(app) {
            eprintln!("[mcpane] {} 退出还原直连失败: {error}", app.as_str());
        }
    }
}

/// 手动启动（UI 路由开关）：即使当前没有 proxy 模式供应商也可拉起（空上游 503）。
pub async fn start_manual() -> Result<DesktopProxyStatus, String> {
    let _lifecycle = manager().lifecycle.lock().await;
    let gateway = tauri::async_runtime::spawn_blocking(|| {
        crate::commands::provider_service_for_desktop_proxy()
            .ensure_desktop_gateway()
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    manager().state.set_gateway_token(gateway.token);
    ensure_running(gateway.listen_port).await?;
    // 有生效的 proxy 供应商时顺带刷新上游快照。
    sync_from_store_locked().await
}

/// 手动停止：当前 Desktop 供应商仍处 proxy 模式、或有 app 路由接管中时拒绝
///（客户端请求会直接打不通；对齐 cc-switch 停止前的接管校验语义）。
pub async fn stop_manual() -> Result<(), String> {
    let _lifecycle = manager().lifecycle.lock().await;
    let proxy_mode_active =
        tauri::async_runtime::spawn_blocking(|| Ok::<bool, String>(desired_upstream()?.is_some()))
            .await
            .map_err(|e| e.to_string())??;
    if proxy_mode_active {
        return Err(
            "当前 Claude Desktop 供应商正在使用本地路由，停止会使其无法对话；请先切换到直连供应商或官方登录".into(),
        );
    }
    if manager().state.has_routes() {
        return Err(
            "Claude Code / Codex 正在经本地路由转发，停止会使其无法对话；请先在供应商页关闭本地路由".into(),
        );
    }
    stop_server().await;
    Ok(())
}

/// 推导期望上游：(gateway_token, listen_port, upstream)；None = 不需要代理。
#[allow(clippy::type_complexity)]
fn desired_upstream() -> Result<Option<(String, u16, DesktopUpstream)>, String> {
    let svc = crate::commands::provider_service_for_desktop_proxy();
    let current = svc.current().map_err(|e| e.to_string())?;
    let Some(current_id) = current.get(&ClientApp::ClaudeDesktop) else {
        return Ok(None);
    };
    let profiles = svc.list().map_err(|e| e.to_string())?;
    let Some(profile) = profiles.iter().find(|p| p.id == *current_id) else {
        return Ok(None);
    };
    if profile.category != ProviderCategory::Relay || profile.desktop_mode != DesktopMode::Proxy {
        return Ok(None);
    }
    let base_url = profile
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("中转站「{}」缺少接口地址", profile.name))?;
    let key = svc.reveal_key(&profile.id).map_err(|e| e.to_string())?;
    let gateway = svc.ensure_desktop_gateway().map_err(|e| e.to_string())?;
    let mut upstream = DesktopUpstream::new(
        base_url,
        key.expose(),
        profile.api_key_field == ClaudeAuthField::ApiKey,
        profile.desktop_routes.clone(),
    )?;
    upstream.format = claude_format(profile.claude_api_format);
    Ok(Some((gateway.token, gateway.listen_port, upstream)))
}

/// 确保监听于目标端口；端口变了先在新端口绑定成功再停旧实例，
/// 绑定失败时旧实例照常服务（改端口不会把正在用的客户端打断）。
async fn ensure_running(listen_port: u16) -> Result<(), String> {
    let mgr = manager();
    {
        let guard = mgr.server.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(server) = guard.as_ref()
            && server.port() == listen_port
        {
            return Ok(());
        }
    }
    let server = ProxyServer::start(listen_port, mgr.state.clone()).await?;
    let previous = {
        let mut guard = mgr.server.lock().unwrap_or_else(|p| p.into_inner());
        guard.replace(server)
    };
    if let Some(previous) = previous {
        previous.shutdown().await;
    }
    Ok(())
}

async fn stop_server() {
    let server = {
        let mut guard = manager().server.lock().unwrap_or_else(|p| p.into_inner());
        guard.take()
    };
    if let Some(server) = server {
        server.shutdown().await;
    }
}
