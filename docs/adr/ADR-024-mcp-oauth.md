# ADR-024：下游远程 MCP 的 OAuth 登录

- 状态：Accepted
- 日期：2026-10-04
- 关联：ADR-019（双栈协议，下游握手回退）、ADR-007（控制面）、AGENTS.md 第 8 条（令牌不进 SQLite / 日志 / 快照 / 导出）

## 背景

Notion、Linear、Supabase、Neon、Sentry 等的远程 MCP 只提供 OAuth 授权，没有可手填的静态 Token。
此前 `SpawnSpec::Http` 只有静态 `bearer` / `headers`，这类服务器回 401 后被当成普通连接失败，
进入健康监控的重连循环——它们根本接不进 Hub。而「登录一次、所有客户端共用」恰是聚合 Hub 的天然价值：
不接入 Hub 时，每个客户端各自跑一遍登录。

二次调研 magpie（见 `docs/competitors/REFERENCES.md`）时确认它已实现这一能力。我们的实现思路不同：
复用 rmcp 官方 SDK 的 `auth` 模块，不手写协议。

## 决策

### 1. 协议全部交给 rmcp，我们只补三样

rmcp 3.1.4 的 `auth` 特性已含授权服务器发现（RFC 9728 / 8414）、动态客户端注册（RFC 7591）、PKCE、
换令牌、过期前自动刷新，以及 `AuthClient<C>`——它实现 `StreamableHttpClient`，可直接替换连接用的
HTTP 客户端；遇 401 有专门的 `StreamableHttpError::AuthRequired`。我们补的是：

1. 令牌的**文件存储**（`FileCredentialStore`，实现 rmcp 的 `CredentialStore`）；
2. 一次登录的**两段式编排**（`OAuthLogins`：`begin` 产出授权链接，`complete` 凭回调换令牌）；
3. 连接时把已存令牌挂到客户端上（`authorized_client`）。

**不把令牌塞进静态 `bearer`**：访问令牌会过期，静态值很快失效，也失去了刷新。

### 2. 令牌存文件，不存系统钥匙串

`$MCPANE_HOME/mcp-oauth/<server_id>.json`，Unix 目录 0700、文件 0600，写入走临时文件 + 原子改名。

- Windows 凭据存储放不下 OAuth 令牌（`CRED_MAX_CREDENTIAL_BLOB_SIZE`，keyring-rs 的 UTF-16 存法使 ASCII
  有效上限减半到 1280 字节），业界（gh / gcloud / npm / cc-switch）共识是改存文件；
- macOS 上 sidecar 读主应用写的钥匙串条目有签名与授权弹框问题；
- Hub 是令牌**唯一**的读写方：桌面从不接触令牌。

符合 AGENTS.md 第 8 条：不在 SQLite、日志、快照、导出里。备份只导出数据库快照与 `providers.json` 边车，
云同步也基于快照，都不含该目录（`backup_never_carries_mcp_oauth_tokens` 守卫）。换机恢复后令牌不会跟来，
对应服务器显示「需要登录」，这是预期。服务器被删除时，Hub 在下一次重载里回收孤儿令牌文件
（只按「库里还有没有这个 Server」判定，禁用的保留）。

### 3. 登录流程分工

桌面负责 UI、系统浏览器与本机回环回调，Hub 负责协议与令牌：

1. 桌面绑定 `127.0.0.1:0` 取端口，得 `redirect_uri`（动态注册时就要用它）；
2. 控制面 `oauth-begin`：Hub 做发现与注册，返回授权链接，授权状态（含 PKCE verifier）暂存内存 10 分钟；
3. 桌面打开浏览器，等回调（最长 5 分钟，可取消），回调监听只接受 `/callback`，其余请求回 404；
4. 控制面 `oauth-complete`：Hub 交给 SDK 校验 `state`（防 CSRF）并换令牌、落盘，随后重连该服务器。

另有 `oauth-cancel`（丢弃进行中的登录）、`oauth-logout`（删令牌，**先注销旧连接再重载**——重载对
「规格没变且 Ready」的下游会原样保留连接，那条连接手里还拿着刚删掉的令牌）。命令字面量是桌面与 Hub
之间的契约，有 wire 测试守卫。

### 4. 连接与状态

- `SpawnSpec::Http` 增加 `oauth: Option<OAuthSpec>`。**手配了 Bearer / Authorization 头时不启用 OAuth**，
  以用户的配置为准；这种情形下被 401 报的是「服务器拒绝了已配置的认证信息」，登录帮不上忙。
- 服务器回 401 → `DownstreamError::AuthRequired`，Hub 状态 `needs-auth`。**不是故障**：握手阶段直接上抛，
  不回退 legacy 再撞一次；**健康监控跳过它**，不探测也不重连（实测无此保护时每轮都会再撞服务器）。
  登录、退出登录、配置变化触发的重载才会重新尝试。
- 令牌过期且刷新被授权服务器明确拒绝 → 同样落到「需要登录」；刷新的瞬时失败交给健康监控重试。
- 诊断为每个下游带出 `lastErrorKind`（失败原因代号）与 `signedIn`。

### 5. 失败原因（T5）

下游错误归类为稳定代号：`needs_auth` / `command_not_found` / `exited` / `no_response` /
`connection_refused` / `http_<状态码>` / `config` / `other`，界面据此显示「无法启动：找不到命令」
「进程已退出」「无应答」「连接被拒绝」「HTTP 404」…，而不是一律「连接失败」，原文仍在 tooltip。
归类只认操作系统与 reqwest / rmcp 文案里最稳的特征，认不出归 `other`。

## 不做

- **scope 升级**（403 `insufficient_scope`）：本期原样报错；
- **网关反代 `/mcp/<name>`**（magpie 的做法）：我们的 Hub 本身就是共享出口，客户端只连 Hub；
- **SSE 旧传输**接 OAuth；
- 把令牌放进钥匙串（见 §2）。

## 验证

- `mcpane-hub-downstream/tests/oauth_e2e.rs`：假授权服务器（元数据发现、动态注册、授权码 + PKCE 校验、刷新）
  + 受令牌保护的 MCP 服务器；覆盖 401 上报挑战、登录后列出工具、令牌自动刷新、刷新被拒回到需要登录、
  伪造 `state` 被拒且不落盘、退出登录、令牌文件 0600、手配 Bearer 优先。
- `mcpane-hub/tests/oauth_control.rs`：经 Hub 控制面全程——装载得 `needs-auth` 且监控不再撞服务器 →
  begin / 浏览器同意 / complete → 连上 → 退出登录 → 删除服务器回收令牌。三项关键行为都做过反向验证。
- **真实进程端到端**：`scripts/e2e-oauth-hub.py`（26 项检查）。起真实 `mcpane-hub` 二进制与独立的假 OAuth /
  MCP 服务器进程（`cargo run -p mcpane-hub-downstream --example oauth_fake_server`），像桌面端那样走控制面
  UDS 完成 begin / 浏览器 / 回环回调 / complete，再像 Claude Code 那样走数据面调用受保护的工具；覆盖
  未登录不重连、Hub 重启后保持登录、退出登录、令牌 5 秒过期后自动刷新、孤儿令牌回收，并确认令牌不进 SQLite。
  脚本本身做过负向对照（去掉健康监控跳过，对应检查必然失败）。
- 用真实 Hub 无凭据连接真实服务器：Notion、Linear、Sentry、Neon 均判为 `needs-auth`，无需认证的
  DeepWiki 为 `ready`（不误判）。这一步不向对方注册客户端、也不登录。
- 桌面回环回调：解析、噪声请求、真实回环连接、取消；前端 `cardStatus` 单测与 Playwright 登录入口用例。
- **未验证**：真实 Notion / Linear 的浏览器授权全流程与对真实授权服务器的动态注册（需要账号与浏览器；
  动态注册会在对方登记一个客户端，故未自动化）；Windows 上的回环回调与文件权限；桌面应用里点击「登录」
  的完整界面流程（Tauri 开发构建无法被自动驱动）。已写入发版清单。
