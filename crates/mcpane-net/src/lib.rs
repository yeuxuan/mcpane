//! MCPane 出网层（ADR-017）：全局代理配置、例外判定与本地代理探测。
//!
//! 收敛到本 crate 的理由：需要代理的两处出网互不相邻——`mcpane-provider`
//! （连通性测试 / 额度查询）走 adapter+config+secrets，`mcpane-hub-downstream`
//! （HTTP 下游 MCP server）走 hub-catalog，二者无公共依赖。各自实现必然漂移出
//! 「桌面端认代理、Hub 不认」这类分裂。
//!
//! ## 为什么不自己解析代理地址
//!
//! 代理的解析（环境变量优先级、平台设置、`user:pass@` 认证）全部交给 reqwest，
//! 本 crate 只决定**谁不该走代理**。试过自己解析：`hyper-util` 的
//! `parse_env_uri` 会把 userinfo 从 authority 里拆进 `Auth::Basic`，从
//! `Intercepted::uri()` 取回的地址**不含密码**——照那条路走会悄悄丢掉企业认证
//! 代理的凭据。让 reqwest 全权解析则零漂移、零丢失。
//!
//! ## 为什么必须自己兜例外表
//!
//! `hyper-util` 的 `mac::with_system` 只读 `HTTPProxy`/`HTTPSProxy` 等六个键，
//! **不读 `ExceptionsList`**（实测：系统例外表里明明有 `127.0.0.1`，
//! `Matcher::from_system()` 仍把 `http://127.0.0.1:3000` 判给代理）。不补这一层，
//! 开启 system-proxy 会把本地 MCP server 与本机模型端点（Ollama 等）一并塞进
//! 代理——等于用一个功能换掉另一个功能。

pub mod scan;

use std::sync::{OnceLock, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use reqwest::{Client, ClientBuilder, NoProxy, Proxy, Url};
use url::Host;

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("代理地址无法解析：{0}")]
    Parse(String),
    #[error("不支持的代理协议 `{scheme}`（支持 http / https / socks5 / socks5h）")]
    Scheme { scheme: String },
    #[error("代理地址缺少主机名")]
    MissingHost,
    #[error("构造 HTTP 客户端失败：{0}")]
    Build(String),
}

/// 全局出网代理配置。
///
/// 三态而非 cc-switch 的「URL 空 = 直连」二态：它那套里清空 URL 并**不能**真正
/// 直连——reqwest 仍会自动探测 `HTTP_PROXY` 等环境变量，用户以为关掉了其实没有。
/// `Direct` 显式调用 `.no_proxy()`，说关就是真关。
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum ProxyConfig {
    /// 跟随系统：环境变量 + macOS 系统设置 / Windows 注册表（由 reqwest 解析）。
    #[default]
    System,
    /// 强制直连：忽略一切系统与环境变量设置。
    Direct,
    /// 显式指定代理地址。
    #[serde(rename_all = "camelCase")]
    Manual { url: String },
}

/// 代理地址允许的协议。与 reqwest 实际支持的能力一致（socks 由 `socks` feature
/// 提供，见根 Cargo.toml）。
const ALLOWED_SCHEMES: &[&str] = &["http", "https", "socks5", "socks5h"];

/// 校验并规范化用户输入的代理地址。
///
/// 先自行校验协议再交给 `Proxy::all`：后者对部分畸形输入并不报错，
/// 直接透传会让用户在「保存成功、但全网不通」之间自己找原因。
pub fn validate_proxy_url(raw: &str) -> Result<Url, ProxyError> {
    let url = Url::parse(raw.trim()).map_err(|e| ProxyError::Parse(e.to_string()))?;
    let scheme = url.scheme().to_owned();
    if !ALLOWED_SCHEMES.contains(&scheme.as_str()) {
        return Err(ProxyError::Scheme { scheme });
    }
    if url.host_str().unwrap_or_default().is_empty() {
        return Err(ProxyError::MissingHost);
    }
    Ok(url)
}

/// 代理配置的单行描述，可安全写进日志（认证信息已隐去）。
///
/// 「为什么连不上」是这个功能最高频的问题，而回答它的第一步永远是「当时到底
/// 在走哪条通道」。故双端启动时都把这行记进日志。
pub fn describe(config: &ProxyConfig) -> String {
    match config {
        ProxyConfig::System => "跟随系统".to_owned(),
        ProxyConfig::Direct => "强制直连".to_owned(),
        ProxyConfig::Manual { url } => mask_proxy_url(url),
    }
}

/// 隐去代理地址中的认证信息，供日志与错误信息使用。
///
/// 重建而非做字符串替换：只保留 scheme/host/port，userinfo 在结构上就没有容身处，
/// 不会因为密码里含 `@`、`:` 之类字符而漏出来。
pub fn mask_proxy_url(raw: &str) -> String {
    let Ok(url) = Url::parse(raw.trim()) else {
        // 解析不了就别猜结构：整体打码，宁可少给信息也不能把密码写进日志。
        return "<无法解析的代理地址>".to_owned();
    };
    let host = url.host_str().unwrap_or("<无主机名>");
    match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    }
}

/// 目标是否强制直连（不受代理配置影响）。
///
/// 覆盖范围以 macOS「系统设置 → 网络 → 代理」的默认例外表为底
/// （`scutil --proxy` 的 ExceptionsList：`127.0.0.1`、`localhost`、`*.local`、
/// `10/8`、`172.16/12`、`192.168/16`）——用 std 的 `is_loopback` / `is_private`
/// 表达，无需自己解析 CIDR。
///
/// 另有三项刻意做得比那张表更宽，都是同一条理由的自然延伸（「本机与内网不该
/// 出门」），不是疏漏、别当 bug 删掉：
/// - `is_link_local`（169.254/16）与 IPv6 的 ULA（fc00::/7）、链路本地
///   （fe80::/10）：IPv4 私有网段的等价物，那张表只是没写 IPv6；
/// - `*.localhost`：RFC 6761 保证解析到 loopback。
///
/// 用户自定义的 `NO_PROXY` 不在此处理——`System` 模式由 reqwest 自动读取，
/// `Manual` 模式在构造代理时显式挂上 `NoProxy::from_env()`，两条路都已覆盖。
pub fn bypasses_proxy(target: &Url) -> bool {
    match target.host() {
        Some(Host::Ipv4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Some(Host::Ipv6(ip)) => {
            ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()
        }
        Some(Host::Domain(host)) => {
            let host = host.trim_end_matches('.').to_ascii_lowercase();
            host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local")
        }
        None => false,
    }
}

/// 字符串版：地址解析不了时保守判为「不豁免」，交给代理去报真实错误。
fn target_bypasses(target: &str) -> bool {
    Url::parse(target).is_ok_and(|url| bypasses_proxy(&url))
}

impl ProxyConfig {
    /// 试构建一遍，确认这份配置真的能用；结果丢弃。
    ///
    /// 校验口径必须与真实构建口径**是同一条路**：若只校验 URL 格式，就会留下
    /// 「校验过了、构建却失败」的窗口——调用方那时已经把配置写进库了，落得
    /// 库里存新值、运行态跑旧值、界面显示第三种。走同一条路则那个窗口不存在。
    pub fn check(&self) -> Result<(), ProxyError> {
        OutboundClients::build(self).map(|_| ())
    }

    /// 按目标地址把代理配置应用到 builder：本机/内网目标强制直连，其余按配置走。
    ///
    /// 交出 builder 而非现成 `Client`，是因为调用方（Hub 的 HTTP 下游）要先挂上
    /// 自己那份 per-server 的 `default_headers`（含 Bearer），无法共用客户端。
    /// 只有一个共享客户端的调用方请用 [`client_for`]。
    pub fn apply(&self, builder: ClientBuilder, target: &str) -> Result<ClientBuilder, ProxyError> {
        if target_bypasses(target) {
            return Ok(builder.no_proxy());
        }
        self.configure(builder)
    }

    /// 无条件把代理配置应用到 builder（不考虑目标地址的例外判定）。
    fn configure(&self, builder: ClientBuilder) -> Result<ClientBuilder, ProxyError> {
        match self {
            // 不动 builder：reqwest 的 auto_sys_proxy 默认开着，环境变量与平台
            // 设置都由它读（平台设置那半边需要 system-proxy feature，见根 Cargo.toml）。
            Self::System => Ok(builder),
            Self::Direct => Ok(builder.no_proxy()),
            Self::Manual { url } => {
                let validated = validate_proxy_url(url)?;
                let proxy = Proxy::all(validated)
                    .map_err(|e| ProxyError::Parse(e.to_string()))?
                    // 显式指定代理时 reqwest 会关掉 auto_sys_proxy，NO_PROXY 也随之
                    // 失效；这里挂回去，让用户配的例外表在手动模式下同样作数。
                    .no_proxy(NoProxy::from_env());
                Ok(builder.proxy(proxy))
            }
        }
    }
}

/// 一对出网客户端：走代理的与强制直连的。
///
/// **一般不要直接用这个**——进程内出网走 [`set_proxy`] + [`client_for`]，
/// 它们背后就是这对客户端，且跟随全局配置。这里公开只为两处：集成测试需要
/// 一个不碰全局槽的实例，以及 [`ProxyConfig::check`] 的试构建。
///
/// 分成两个而非一个「按需带代理」的客户端，是因为 `System` 模式下代理由 reqwest
/// 内部解析，我们无从把例外表注入进去（`auto_sys_proxy` 不接受 `NoProxy`）。
/// 在外层按目标地址二选一，既拿到 reqwest 的完整解析能力，又保住例外表。
/// 两个客户端各自持连接池，`reqwest::Client` 内部是 `Arc`，clone 极廉价。
pub struct OutboundClients {
    proxied: Client,
    direct: Client,
}

impl OutboundClients {
    pub fn build(config: &ProxyConfig) -> Result<Self, ProxyError> {
        Ok(Self {
            proxied: config
                .configure(Client::builder())?
                .build()
                .map_err(|e| ProxyError::Build(e.to_string()))?,
            direct: Client::builder()
                .no_proxy()
                .build()
                .map_err(|e| ProxyError::Build(e.to_string()))?,
        })
    }

    /// 按目标地址选客户端：本机与内网强制直连，其余按配置走代理。
    pub fn for_target(&self, target: &str) -> &Client {
        if target_bypasses(target) {
            &self.direct
        } else {
            &self.proxied
        }
    }

    /// 进程启动、`set_proxy` 尚未跑过时的兜底：等价于 `System`。
    fn initial() -> Self {
        Self::build(&ProxyConfig::System).unwrap_or_else(|_| Self {
            proxied: Client::new(),
            direct: Client::new(),
        })
    }
}

fn slot() -> &'static RwLock<OutboundClients> {
    static SLOT: OnceLock<RwLock<OutboundClients>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(OutboundClients::initial()))
}

/// 锁中毒不该让出网整体瘫痪：里面存的只是两个 `Client`，没有会被写坏的不变式。
fn read_slot() -> RwLockReadGuard<'static, OutboundClients> {
    slot().read().unwrap_or_else(PoisonError::into_inner)
}

fn write_slot() -> RwLockWriteGuard<'static, OutboundClients> {
    slot().write().unwrap_or_else(PoisonError::into_inner)
}

/// 换上新的代理配置，即刻对后续请求生效。
///
/// 在途请求握着旧 `Client` 的 clone（旧连接池、旧代理）跑完，不受影响；
/// 之后每次 [`client_for`] 拿到的都是新的。无需重启进程。
///
/// **每个用 [`client_for`] 出网的进程，启动时都必须调一次**，否则全局槽停在
/// 「跟随系统」，用户配的 Direct/Manual 在该进程内无声失效。
pub fn set_proxy(config: &ProxyConfig) -> Result<(), ProxyError> {
    let rebuilt = OutboundClients::build(config)?;
    *write_slot() = rebuilt;
    Ok(())
}

/// 取出适用于该目标的出网客户端。
///
/// 前置条件：本进程已调过 [`set_proxy`]。未调过时返回的是「跟随系统」的默认
/// 客户端——不会报错，只会静默按默认出网，故新增调用点时务必确认该进程的
/// 启动路径上有 `set_proxy`（桌面端在 main.rs setup，Hub 在 load_downstreams）。
pub fn client_for(target: &str) -> Client {
    read_slot().for_target(target).clone()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn rejects_unsupported_scheme() {
        let err = validate_proxy_url("ftp://127.0.0.1:2121").expect_err("ftp 不该被接受");
        assert!(matches!(err, ProxyError::Scheme { .. }));
    }

    #[test]
    fn accepts_every_scheme_reqwest_can_actually_use() {
        for raw in [
            "http://127.0.0.1:7897",
            "https://proxy.example.com:8443",
            "socks5://127.0.0.1:7891",
            "socks5h://127.0.0.1:1080",
        ] {
            assert!(validate_proxy_url(raw).is_ok(), "{raw} 应被接受");
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(validate_proxy_url("不是地址").is_err());
        assert!(validate_proxy_url("").is_err());
    }

    /// describe 是脱敏在生产里的唯一入口（双端启动日志都过它），
    /// 密码漏进日志就等于落盘。
    #[test]
    fn describe_never_leaks_credentials() {
        let described = describe(&ProxyConfig::Manual {
            url: "socks5://alice:s3cr3t@proxy.corp:1080".to_owned(),
        });
        assert_eq!(described, "socks5://proxy.corp:1080");
        assert!(!described.contains("s3cr3t"), "密码漏进了日志：{described}");
        assert!(
            !described.contains("alice"),
            "用户名漏进了日志：{described}"
        );
    }

    #[test]
    fn describe_names_the_non_manual_modes() {
        assert_eq!(describe(&ProxyConfig::System), "跟随系统");
        assert_eq!(describe(&ProxyConfig::Direct), "强制直连");
    }

    #[test]
    fn mask_strips_credentials() {
        assert_eq!(
            mask_proxy_url("http://alice:s3cr3t@proxy.corp:8080"),
            "http://proxy.corp:8080"
        );
    }

    /// 密码里含 `@`/`:` 时，字符串截断式的脱敏会漏；重建式不会。
    #[test]
    fn mask_survives_nasty_passwords() {
        let masked = mask_proxy_url("http://alice:p%40ss%3Aword@proxy.corp:8080");
        assert_eq!(masked, "http://proxy.corp:8080");
        assert!(!masked.contains("ss"), "密码片段泄漏进了脱敏结果：{masked}");
    }

    #[test]
    fn mask_never_echoes_unparseable_input() {
        let masked = mask_proxy_url("http://user:pw@[不是地址");
        assert!(!masked.contains("pw"), "脱敏失败时把原文吐了出来：{masked}");
    }

    /// 本功能的成败关键：开启 system-proxy 后系统例外表被 hyper-util 忽略，
    /// 本地/内网目标必须由我们自己拦下来，否则本地 MCP server 会被塞进代理。
    #[test]
    fn local_and_private_targets_bypass_proxy() {
        for raw in [
            "http://127.0.0.1:3000",
            "http://localhost:11434",
            "http://LocalHost:11434",
            "http://[::1]:8080",
            "http://192.168.1.10:8080",
            "http://10.1.2.3:8080",
            "http://172.16.5.4:8080",
            "http://printer.local",
            "http://foo.localhost:1234",
            "http://[fd00::1]:8080",
            "http://[fe80::1]:8080",
        ] {
            let url = Url::parse(raw).expect("测试地址应可解析");
            assert!(bypasses_proxy(&url), "{raw} 应强制直连");
        }
    }

    #[test]
    fn public_targets_go_through_proxy() {
        for raw in [
            "https://api.anthropic.com",
            "https://api.moonshot.cn",
            "http://8.8.8.8",
            // 域名里带 localhost 但不是 localhost —— 不能被前缀/子串匹配误伤。
            "https://localhost.attacker.com",
            "https://notlocalhost",
        ] {
            let url = Url::parse(raw).expect("测试地址应可解析");
            assert!(!bypasses_proxy(&url), "{raw} 不该被判为本地");
        }
    }

    #[test]
    fn client_routing_follows_bypass_rules() {
        let clients = OutboundClients::build(&ProxyConfig::Manual {
            url: "http://127.0.0.1:7897".to_owned(),
        })
        .expect("构造应成功");

        assert!(std::ptr::eq(
            clients.for_target("http://127.0.0.1:3000"),
            &clients.direct
        ));
        assert!(std::ptr::eq(
            clients.for_target("https://api.anthropic.com"),
            &clients.proxied
        ));
    }

    /// 解析不了的目标保守走代理：让代理去报真实错误，好过我们静默改道。
    #[test]
    fn unparseable_target_is_not_treated_as_local() {
        assert!(!target_bypasses("这不是一个地址"));
    }

    #[test]
    fn manual_config_rejects_bad_url_at_build_time() {
        let config = ProxyConfig::Manual {
            url: "ftp://nope".to_owned(),
        };
        assert!(OutboundClients::build(&config).is_err());
    }

    /// 前端与存储层都按这个 wire 形状读写，改动即破坏兼容。
    #[test]
    fn serde_shape_is_stable() {
        let manual = ProxyConfig::Manual {
            url: "http://127.0.0.1:7897".to_owned(),
        };
        assert_eq!(
            serde_json::to_value(&manual).expect("序列化"),
            serde_json::json!({"mode": "manual", "url": "http://127.0.0.1:7897"})
        );
        assert_eq!(
            serde_json::to_value(ProxyConfig::System).expect("序列化"),
            serde_json::json!({"mode": "system"})
        );
        let parsed: ProxyConfig =
            serde_json::from_value(serde_json::json!({"mode": "direct"})).expect("反序列化");
        assert_eq!(parsed, ProxyConfig::Direct);
    }

    #[test]
    fn default_is_follow_system() {
        assert_eq!(ProxyConfig::default(), ProxyConfig::System);
    }
}
