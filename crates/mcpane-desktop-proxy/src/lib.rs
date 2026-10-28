//! MCPane 本地路由（单端口、按路径分命名空间）：
//! - `/claude`、`/codex`：Claude Code / Codex 路由接管与故障转移（ADR-022，见 `router`）；
//! - `/claude-desktop`：Claude Desktop 网关（下文）。
//!
//! Claude Desktop 网关（anthropic 直通代理）：
//! Desktop 的 3P gateway profile 指向本进程 `http://127.0.0.1:{port}/claude-desktop`，
//! 本代理做三件事：
//! 1. **鉴权**：Bearer 校验 gateway token（`ProviderState.desktop_gateway`）；
//! 2. **模型映射**：route_id → 上游真实模型（含 `[1m]` 剥离、角色回落、
//!    MiMo thinking 历史归一化——全部复用 `mcpane_provider::claude_desktop`）；
//! 3. **直通转发**：上游是 Anthropic Messages 兼容端点（我们收录的 proxy 预设
//!    全部如此），请求体除 model 外不动，SSE 逐字节透传。
//!
//! 实现取舍：标准 axum + reqwest 栈；Desktop 命名空间不做故障转移
//! （其 profile 只绑定一家）。

mod breaker;
mod claude_chat;
mod claude_responses;
mod codex_anthropic;
mod codex_chat;
mod effort;
mod forward;
mod media;
mod namespace;
mod overflow;
mod rectify;
mod request_log;
mod router;
mod server;
mod sse;
mod stream_error;

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use mcpane_provider::claude_desktop;
use mcpane_provider::{ClientApp, DesktopRoute, RoutingStrategy};

pub use breaker::{BreakerSnapshot, BreakerState};
pub use forward::UpstreamAuth;
pub use request_log::{Attempt, ProviderTotals, RequestRecord, Usage};
pub use router::{
    AppRoute, AppRouteStatus, CandidateStatus, ModelSlots, RouteTuning, RouteUpstream,
    StickyStatus, UpstreamFormat,
};
pub use server::ProxyServer;

/// 当前生效的上游快照（切换供应商时由 service 层整体替换；
/// 上游 Key 从 Keychain 物化后仅存于本结构，不落盘）。
#[derive(Debug, Clone)]
pub struct DesktopUpstream {
    /// 上游 base_url（Anthropic 兼容端点，请求打 `{base_url}/v1/messages`）。
    pub base_url: String,
    pub api_key: String,
    /// true = 上游要求 `x-api-key` 头（ClaudeAuthField::ApiKey）；false = Bearer。
    pub use_x_api_key: bool,
    /// 原始存储路由（Desktop 缓存旧 manifest 时按 raw id 回落匹配）。
    pub routes_raw: Vec<DesktopRoute>,
    /// `resolve_proxy_routes` 产物（模型列表与精确匹配用）。
    pub routes_resolved: Vec<DesktopRoute>,
    /// 端点域名即 MiMo（thinking 历史归一化的触发条件之一）。
    pub endpoint_is_mimo: bool,
    /// 上游协议：`Native` = Anthropic 直通；`OpenaiChat` / `OpenaiResponses` 经转换
    ///（此时 `base_url` 为 OpenAI 根地址）。
    pub format: UpstreamFormat,
}

impl DesktopUpstream {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        use_x_api_key: bool,
        routes_raw: Vec<DesktopRoute>,
    ) -> Result<Self, String> {
        let base_url: String = base_url.into();
        let routes_resolved = claude_desktop::resolve_proxy_routes(&routes_raw)?;
        let endpoint_is_mimo = claude_desktop::is_mimo_identifier(&base_url);
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: api_key.into(),
            use_x_api_key,
            routes_raw,
            routes_resolved,
            endpoint_is_mimo,
            format: UpstreamFormat::Native,
        })
    }
}

struct ProxyShared {
    gateway_token: String,
    upstream: Option<Arc<DesktopUpstream>>,
    routes: BTreeMap<ClientApp, Arc<AppRoute>>,
    last_served: BTreeMap<ClientApp, String>,
    /// 临时接替：主站失败后由备用接手，窗口内优先走它（不改用户的当前选择）。
    sticky: BTreeMap<ClientApp, (String, Instant)>,
    /// 额度已用尽的供应商及其预计恢复时刻（`RoutingStrategy::QuotaAware` 下后置；
    /// 由托管层按额度缓存整体替换，过期条目自然失效）。
    quota_exhausted: BTreeMap<String, Instant>,
}

/// 临时接替窗口：与熔断冷却同长，到期后主站经半开探测自动回切。
const STICKY_WINDOW: Duration = Duration::from_secs(60);

/// 该供应商额度仍处于用尽状态时的剩余秒数（至少 1）；未用尽或已过期为 `None`。
fn exhausted_remaining(shared: &ProxyShared, provider_id: &str) -> Option<u64> {
    shared
        .quota_exhausted
        .get(provider_id)
        .filter(|until| **until > Instant::now())
        .map(|until| {
            until
                .saturating_duration_since(Instant::now())
                .as_secs()
                .max(1)
        })
}

/// 代理共享状态：token + 上游快照。切换/编辑供应商时由托管层更新，
/// 在途请求持有旧快照跑完，天然无锁竞争。
#[derive(Clone)]
pub struct ProxyState {
    inner: Arc<RwLock<ProxyShared>>,
    breakers: Arc<breaker::Breakers>,
    request_log: Arc<request_log::RequestLog>,
}

impl ProxyState {
    pub fn new(gateway_token: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(ProxyShared {
                gateway_token: gateway_token.into(),
                upstream: None,
                routes: BTreeMap::new(),
                last_served: BTreeMap::new(),
                sticky: BTreeMap::new(),
                quota_exhausted: BTreeMap::new(),
            })),
            breakers: Arc::default(),
            request_log: Arc::default(),
        }
    }

    /// 整体替换某 app 的路由快照；`None` = 该 app 未接管（请求 503）。
    pub fn set_route(&self, app: ClientApp, route: Option<AppRoute>) {
        if let Ok(mut shared) = self.inner.write() {
            match route {
                Some(route) => {
                    shared.routes.insert(app, Arc::new(route));
                }
                None => {
                    shared.routes.remove(&app);
                    shared.last_served.remove(&app);
                    shared.sticky.remove(&app);
                }
            }
        }
    }

    /// 整体替换额度用尽信号（provider_id → 预计恢复时刻）。
    pub fn set_quota_exhausted(&self, signals: BTreeMap<String, Instant>) {
        if let Ok(mut shared) = self.inner.write() {
            shared.quota_exhausted = signals;
        }
    }

    /// 清空全部路由快照（路由服务起不来、客户端已回退直连时）。
    pub fn clear_routes(&self) {
        if let Ok(mut shared) = self.inner.write() {
            shared.routes.clear();
            shared.last_served.clear();
            shared.sticky.clear();
        }
    }

    pub fn has_routes(&self) -> bool {
        self.inner
            .read()
            .map(|shared| !shared.routes.is_empty())
            .unwrap_or(false)
    }

    /// 各接管中 app 的候选健康状态。
    pub fn route_status(&self) -> Vec<AppRouteStatus> {
        let Ok(shared) = self.inner.read() else {
            return Vec::new();
        };
        shared
            .routes
            .iter()
            .map(|(app, route)| AppRouteStatus {
                app: *app,
                last_served_by: shared.last_served.get(app).cloned(),
                sticky: shared
                    .sticky
                    .get(app)
                    .filter(|(_, until)| *until > Instant::now())
                    .map(|(id, until)| StickyStatus {
                        provider_id: id.clone(),
                        remaining_secs: until
                            .saturating_duration_since(Instant::now())
                            .as_secs()
                            .max(1),
                    }),
                candidates: route
                    .candidates
                    .iter()
                    .enumerate()
                    .map(|(index, c)| CandidateStatus {
                        provider_id: c.provider_id.clone(),
                        name: c.name.clone(),
                        primary: index == 0,
                        breaker: self
                            .breakers
                            .snapshot(&router::breaker_key(*app, &c.provider_id)),
                        totals: self.request_log.totals(*app, &c.provider_id),
                        quota_exhausted_secs: (route.strategy == RoutingStrategy::QuotaAware)
                            .then(|| exhausted_remaining(&shared, &c.provider_id))
                            .flatten(),
                    })
                    .collect(),
            })
            .collect()
    }

    /// 最近的路由请求（新 → 旧）。
    pub fn recent_requests(
        &self,
        app: Option<ClientApp>,
        limit: usize,
    ) -> Vec<request_log::RequestRecord> {
        self.request_log.recent(app, limit)
    }

    /// 清空请求记录（`app` 为空清全部），供应商累计用量保留。
    pub fn clear_requests(&self, app: Option<ClientApp>) {
        self.request_log.clear(app);
    }

    pub(crate) fn request_log(&self) -> Arc<request_log::RequestLog> {
        self.request_log.clone()
    }

    /// 本次请求的尝试顺序：熔断中的跳过；额度优先策略下额度已用尽的后置；
    /// 临时接替窗口内接替者排最前；全部熔断时仍兜底尝试当前供应商，不把用户彻底锁死。
    pub(crate) fn candidate_order(
        &self,
        app: ClientApp,
        route: &AppRoute,
    ) -> Vec<Arc<RouteUpstream>> {
        let mut order: Vec<Arc<RouteUpstream>> = route
            .candidates
            .iter()
            .filter(|c| {
                self.breakers
                    .is_available(&router::breaker_key(app, &c.provider_id))
            })
            .cloned()
            .collect();
        let shared = self.inner.read().ok();
        let exhausted = |id: &str| {
            route.strategy == RoutingStrategy::QuotaAware
                && shared
                    .as_ref()
                    .is_some_and(|s| exhausted_remaining(s, id).is_some())
        };
        // 稳定后置：可用者保持原相对顺序，用尽者垫底（全用尽时顺序不变）。
        order.sort_by_key(|c| exhausted(&c.provider_id));
        let sticky = shared.as_ref().and_then(|shared| {
            shared
                .sticky
                .get(&app)
                .filter(|(_, until)| *until > Instant::now())
                .map(|(id, _)| id.clone())
        });
        if let Some(id) = sticky
            && !exhausted(&id)
            && let Some(pos) = order.iter().position(|c| c.provider_id == id)
        {
            let chosen = order.remove(pos);
            order.insert(0, chosen);
        }
        drop(shared);
        if order.is_empty()
            && let Some(primary) = route.candidates.first()
        {
            order.push(primary.clone());
        }
        order
    }

    /// 主站成功即结束接替；主站失败后由备用服务则开启 / 续期接替窗口。
    pub(crate) fn update_sticky(
        &self,
        app: ClientApp,
        served_by: &str,
        is_primary: bool,
        primary_failed: bool,
    ) {
        let Ok(mut shared) = self.inner.write() else {
            return;
        };
        if is_primary {
            shared.sticky.remove(&app);
        } else if primary_failed
            || shared
                .sticky
                .get(&app)
                .is_some_and(|(id, _)| id == served_by)
        {
            shared
                .sticky
                .insert(app, (served_by.to_owned(), Instant::now() + STICKY_WINDOW));
        }
    }

    /// 手动恢复某供应商（清除熔断与失败计数）。
    pub fn reset_breaker(&self, app: ClientApp, provider_id: &str) {
        self.breakers.reset(&router::breaker_key(app, provider_id));
        // 手动恢复主站即结束临时接替，下个请求直接回到主站。
        if let Ok(mut shared) = self.inner.write()
            && shared
                .routes
                .get(&app)
                .and_then(|route| route.candidates.first())
                .is_some_and(|primary| primary.provider_id == provider_id)
        {
            shared.sticky.remove(&app);
        }
    }

    pub(crate) fn breakers(&self) -> &breaker::Breakers {
        &self.breakers
    }

    /// 流结束回调要活得比一次请求长，持有一份共享引用。
    pub(crate) fn breakers_shared(&self) -> Arc<breaker::Breakers> {
        self.breakers.clone()
    }

    pub(crate) fn set_last_served(&self, app: ClientApp, provider_id: &str) {
        if let Ok(mut shared) = self.inner.write() {
            shared.last_served.insert(app, provider_id.to_owned());
        }
    }

    fn route_snapshot(&self, app: ClientApp) -> (String, Option<Arc<AppRoute>>) {
        match self.inner.read() {
            Ok(shared) => (
                shared.gateway_token.clone(),
                shared.routes.get(&app).cloned(),
            ),
            Err(_) => (String::new(), None),
        }
    }

    pub fn set_gateway_token(&self, token: impl Into<String>) {
        if let Ok(mut shared) = self.inner.write() {
            shared.gateway_token = token.into();
        }
    }

    /// 整体替换上游快照；`None` = 当前无 proxy 模式供应商（请求 503）。
    pub fn set_upstream(&self, upstream: Option<DesktopUpstream>) {
        if let Ok(mut shared) = self.inner.write() {
            shared.upstream = upstream.map(Arc::new);
        }
    }

    pub fn has_upstream(&self) -> bool {
        self.inner
            .read()
            .map(|shared| shared.upstream.is_some())
            .unwrap_or(false)
    }

    fn snapshot(&self) -> (String, Option<Arc<DesktopUpstream>>) {
        match self.inner.read() {
            Ok(shared) => (shared.gateway_token.clone(), shared.upstream.clone()),
            // RwLock 中毒仅在持锁 panic 后出现；按「无上游」降级而非扩散 panic。
            Err(_) => (String::new(), None),
        }
    }
}
