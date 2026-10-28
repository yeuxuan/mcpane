# ADR-017：全局出网代理——MCPane 自己联网走用户的代理

- 状态：Accepted
- 日期：2026-07-15

## 背景

国内用户配官方 Claude / OpenAI 供应商时连不上，即便本机挂着 Clash 也一样。
核查后确认这不是玄学，MCPane 出网的两条通道**同时是死的**：

1. **环境变量拿不到值**。reqwest 默认会读 `HTTP_PROXY` / `HTTPS_PROXY` /
   `ALL_PROXY`（`hyper-util` 的 `Builder::from_env`，无 feature 门控），但 GUI
   应用从 Dock 启动**不继承 shell 环境**——用户写在 `.zshrc` 里的代理变量
   MCPane 根本看不见。这个断裂本项目早已知情：`envsync`（ADR-016）之所以要
   `launchctl setenv` + LaunchAgent 登录重放，正是因为它。
2. **系统代理设置根本不读**。`reqwest` 的 `system-proxy` 在其 default features
   里，而本仓两处 reqwest 声明都是 `default-features = false` 且未列该项，于是
   `hyper-util` 的 `client-proxy-system` 被关掉，
   `#[cfg(all(feature = "client-proxy-system", target_os = "macos"))]`
   门控的 `mac::with_system` 被整段编译掉。**Clash 的「系统代理」开关写的正是
   这处设置**。

实测（开发机，系统代理开在 `127.0.0.1:7897`、shell 无任何代理变量）：

| 解析来源 | 对 `https://api.anthropic.com` 的结果 |
|---|---|
| `Matcher::from_system()`（feature 开启后） | `http://127.0.0.1:7897` |
| 纯环境变量（feature 关闭时的等效行为） | `None` |

即：改动前 MCPane 对这台机器上明明开着的代理**视而不见**。

需要澄清与 ADR-014 的关系：那份 ADR 里「MCPane 无代理架构」指的是
**MCPane 不自建本地转发代理**（不接管上游流量、不做 wire 转换、不做 failover
重试循环）。本 ADR 的方向相反——MCPane 作为**客户端**去用用户已有的代理。
两者不冲突，ADR-014 的决策继续有效。

## 决策

### 1. 开启 reqwest 的 `system-proxy` 与 `socks` feature

- `system-proxy`：代价为 macOS 的 `system-configuration` + Windows 的
  `windows-registry`，`core-foundation` 本就在树里。
- `socks`：门控 socks5。Clash 的 7891、通用 1080、v2rayN 的 10808 全是 socks5，
  不开则扫出来也用不了。0.13 起该 feature 不再拉 `tokio-socks`，为内建实现，
  代价为零。

**全仓只保留一处 reqwest 声明**（根 workspace）。`apps/desktop/src-tauri` 是独立
workspace，原本也有一份自己的 reqwest 声明——但那是**从未被引用的死依赖**
（全目录搜不到一处 `reqwest::`），已删除。features 经 path 依赖
（`mcpane-net` / `mcpane-provider`）从根 workspace 单点传导，桌面端与 Hub
在结构上不可能分裂，无需靠「两处保持对齐」的人工纪律。

**feature 哨兵必须验行为，不能验构造。** 曾把哨兵写成
「`OutboundClients::build(socks5 地址).is_ok()`」，那是假的：reqwest 0.13.4 里
带 `#[cfg(feature = "socks")]` 门控、会报 `unknown proxy scheme` 的
`ProxyScheme::parse` **整段被注释掉了**（`src/proxy.rs` 97–757 行是一个注释块）；
实际生效的 `IntoProxy::into_proxy` 对 socks 既不做 feature 门控也不做 scheme
校验，只解析 URL 并补默认端口 1080——于是无论 feature 开没开，构造恒 `Ok`，
那条断言实际只测了「URL 能解析」。现由
`mcpane-net/tests/bypass.rs::socks5_feature_is_actually_compiled_in` 把关：
用本地假代理读首字节，断言收到 SOCKS5 握手的版本号 `0x05`。实测裁掉该 feature
后此测试立刻转红（收到空字节）。

### 2. 三态配置：`system` / `direct` / `manual`

存 `app_settings` 的 `proxy_mode` + `proxy_url` 两列（迁移 0006，`ADD COLUMN`
不触发表重建），默认 `system` —— 与改动前 reqwest 的默认行为一致，老用户升级
不改变出网行为。

明确**不采纳** cc-switch 的二态模型（URL 空 = 直连）：它那个「直连」是假的，
清空 URL 后 reqwest 仍会自动探测环境变量，用户以为关掉了其实没有；其全仓
只在防自环时调过一次 `.no_proxy()`。`Direct` 必须显式 `.no_proxy()`。

### 3. 代理解析全权交给 reqwest，MCPane 只决定「谁不走代理」

试过自己解析并否决了：`hyper-util` 的 `parse_env_uri` 会把 userinfo 从
authority 里拆进 `Auth::Basic`，从 `Intercepted::uri()` 取回的地址**不含密码**
——那条路会悄悄丢掉企业认证代理的凭据。让 reqwest 全权解析则零漂移、零丢失。

### 4. 例外表必须自建（本 ADR 最反直觉的一条）

`hyper-util` 的 `mac::with_system` 只读 `HTTPProxy`/`HTTPSProxy`/`Port`/`Enable`
六个键，**不读 `ExceptionsList`**。实测后果：

```
https://api.anthropic.com  -> 127.0.0.1:7897   （想要的）
http://127.0.0.1:3000      -> 127.0.0.1:7897   （本地 MCP server 被劫走）
http://localhost:11434     -> 127.0.0.1:7897   （本地 Ollama 被劫走）
http://192.168.1.10:8080   -> 127.0.0.1:7897   （局域网被劫走）
```

后三个明明都在系统的例外表里。**光开 feature 会修好「连不上官方 API」，同时
打断「连得上本地 server」——拿一个功能换另一个功能。** 故 `mcpane-net` 自带
一份例外判定，覆盖范围对齐 macOS 的默认例外表（loopback / 私有网段 /
`localhost` / `*.local`），用 std 的 `is_loopback()` / `is_private()` 表达，
无需解析 CIDR。用户自定义的 `NO_PROXY` 不在此处理：`system` 模式由 reqwest
自动读取，`manual` 模式显式挂上 `NoProxy::from_env()`，两条路都已覆盖。

因为 `system` 模式下代理由 reqwest 内部解析、无从注入例外表
（`auto_sys_proxy` 不接受 `NoProxy`），故持有**一对**客户端（走代理的 +
强制直连的），在外层按目标地址二选一。`reqwest::Client` 内部是 `Arc`，
clone 极廉价且共享连接池。

### 5. 新建 `crates/mcpane-net`

需要代理的两处出网互不相邻：`mcpane-provider`（连通性测试 / 额度查询）走
adapter+config+secrets，`mcpane-hub-downstream`（HTTP 下游）走 hub-catalog，
二者无公共依赖。塞进 `mcpane-hub-protocol` 会把一个 serde-only 的常量 crate
拽上 reqwest。沿用本仓既有做法——建共享 crate 收敛双端实现以防漂移
（`mcpane_home` / `panic_hook` 就是这么收进 hub-protocol 的）。

### 6. Hub 侧复用 `ReloadServers`，不新增控制面命令

Hub 是独立进程、自有一套 reqwest 客户端，与桌面端共享同一个 SQLite。代理配置
挂在 `AggregationState` 上（健康监控循环只持有 state、拿不到连接池，而它恰恰是
重连 HTTP 下游的地方），由 `load_downstreams` 在启动与每次 reload 时刷新。

桌面端改代理后发的就是 `ReloadServers`——其语义本就是「重读配置、重连下游」，
正是需要的动作。顺带避开一件事：控制面在 Windows 上尚未落地（ADR-007），
新增命令等于给 Windows 再添一处走不通的路径；复用既有命令则与既存行为同进退
（Windows 上代理改动在 Hub 重启后生效，与 Server 改动的现状一致）。

> **后续订正（2026-07-29）**：上段「控制面在 Windows 上尚未落地」写于 Named Pipe
> 实现之前，现已不成立（见 `control.rs` 的 `#[cfg(windows)]` 分支）。复用
> `ReloadServers` 的结论不受影响——它本来就是这件事的正确语义，与平台无关。

### 7. 扫描：实测才算数，不按端口猜协议

cc-switch 维护了一张「端口 → 协议」硬编码表。这表两头漏：

- **漏**：mihomo / Clash Verge 的默认混合端口 **7897 不在表里**。开发机实测
  正是这个端口——照抄那张表会一无所获。
- **误报**：只做 TCP 探活，不验证端口真的讲代理协议。开发机上 8080 十有八九是
  本地 dev server，3128 / 8888 同理，列出来用户点了才发现不通。

故：TCP 探活只作预筛，每个开着的端口 http 与 socks5 各真用一次，通了才列。
验证目标用 HTTPS 是刻意的——reqwest 对 HTTPS 目标会向代理发 `CONNECT`，
只有真代理答得上来，dev server 收到 `CONNECT` 只会 400/405，直接判死。
既然逐个实测，端口表就退化成「值得一探的端口」，收得宽也无代价。

开发机实测：2.1s 返回 `http://127.0.0.1:7897` 与 `socks5://127.0.0.1:7897`
两条，均验证通过，无误报。

## 不在范围

- **stdio 下游的联网**（`npx` 拉包等）。那是第三方程序自己的网络栈，注入
  `HTTP_PROXY` 会静默改变它们的行为。需要的用户可用 Server 的 `env` 字段
  逐个指定（该逃生口已存在）。
- **按供应商配置代理**。全局一份足够；Clash 规则模式本就能分流，需要精细
  分流的用户可用 `NO_PROXY`。
- **改客户端的联网方式**。MCPane 只管自己出网；Claude Code / Codex 走不走
  代理由它们自己决定，本设置不写入客户端配置。

## 影响

- 新增 `crates/mcpane-net`：`ProxyConfig` / 例外判定 / 客户端对 / 本地扫描 /
  脱敏描述（`describe`，双端启动日志用）；
- 根 `Cargo.toml`：reqwest 增 `system-proxy` + `socks`；
  `apps/desktop/src-tauri/Cargo.toml`：删除其未被引用的 reqwest 声明，
  全仓收敛到单点；
- `mcpane-provider`：`http_client()` 改为 `http_client(target)`（3 处调用点，
  均在 crate 内），`quota.rs` 的请求构造改为先定地址再取客户端；
- `mcpane-registry`（ADR-025，后加）：官方 MCP Registry 查询同样经 `client_for(url)`，
  设置页的代理对它生效；
- `mcpane-hub-downstream`：`connect()` 增 `proxy: &ProxyConfig` 参数（全局配置
  走参数而非塞进 per-server 的 `SpawnSpec`，后者是建模撒谎）；
- `mcpane-storage`：迁移 0006 + `SettingsRepo::get_proxy` / `set_proxy`；
- Hub：`AggregationState` 持有代理，`load_downstreams` 刷新，`TestServer` 用
  同一份配置（测试若走另一条出网路径，「测得通、连不上」会让用户彻底失去判断依据）；
- 桌面端：4 个命令（`proxy_get` / `proxy_set` / `proxy_scan` / `proxy_test`）、
  启动时把库里配置装进客户端、设置页新增代理卡片。
