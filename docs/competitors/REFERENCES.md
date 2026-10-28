# 竞品参考与边界

> 用途仅限：公开功能、交互、模块边界、失败场景和架构思路研究。
> MCPane 必须独立设计和实现，**不复制**竞品源代码、组件、文案、视觉资源或独特目录结构。
> 边界约束见 ADR-008。

## 1. CC Switch

- 地址：https://github.com/farion1231/cc-switch
- 重点参考（公开功能层面）：
  - Tauri + React + Rust 桌面分层
  - Provider 列表、添加、编辑、切换与托盘快捷操作
  - Claude Code / Codex 客户端 Adapter 思路
  - MCP 配置导入、导出和同步流程
  - SQLite 本地状态；配置备份、原子写入和恢复
  - 系统托盘、窗口生命周期、自动更新；多客户端状态展示
  - 全局出网代理与本地代理端口扫描（ADR-017）
- MCPane 改进原则：
  - 客户端原生配置是"实际状态"，数据库不得用过期快照无条件覆盖
  - 每次写入前重新读取文件并检查内容哈希
  - Codex TOML 语法树级最小修改，保留注释与未知字段
  - Windows Native / macOS / WSL Target 不混用路径或命令
  - Provider 管理与 MCP Hub 解耦，P0 不做模型协议转换和代理
  - 出网代理三态而非二态：它的「URL 空 = 直连」是假直连，清空后 reqwest 仍读
    环境变量，用户以为关了其实没关；MCPane 的 `Direct` 显式 `.no_proxy()`
  - 代理必须豁免本机与内网目标：它未处理，而 hyper-util 不读系统例外表，
    照做会把本地 MCP server 与本机模型端点一并塞进代理（ADR-017 §4）
  - 端口扫描逐个实测而非按端口猜协议：按端口猜协议既漏（mihomo/Clash Verge
    的默认口）又误报（8080 多是本地 dev server）
  - 代理测试目标用自己实际要访问的端点，不用 httpbin.org / google.com——
    后者在国内本身就不通，会把好代理测成坏的
  - **第三方 MCP 预设数据不可当事实源**：部分竞品预设里的 `time` 一条写的是
    `npx -y @modelcontextprotocol/server-time`，而该 npm 包 **404、不存在**
    （官方 time 是 Python 包 `uvx mcp-server-time`）。预设逐条去 registry /
    上游核准（核验方法见 `apps/desktop/src/lib/serverPresets.ts` 头部）
  - Windows `cmd /c` 包装不适用于 MCPane：竞品把预设写进**客户端**配置、
    由客户端 spawn（Claude Code 的 /doctor 要求带 `cmd /c`）；MCPane 只往客户端
    写 Hub 的 HTTP 入口，stdio 由 Hub 自己 spawn，故解法是补 `.cmd` 后缀让
    Rust std 接管 cmd.exe 包裹与安全引号（见 mcpane-hub-downstream）

## 2. MCP Router

- 地址：https://github.com/mcp-router/mcp-router
- 重点参考（公开功能层面）：
  - 一个聚合 MCP Server 连接多个下游 MCP Server
  - Project、Workspace、Server 开关和 Tool 开关
  - 本地与远程 MCP Server 接入
  - Tool/Resource/Prompt 三类聚合的思路
  - stdio ↔ Streamable HTTP 桥接的兼容思路
  - 一键接入客户端和请求日志体验
- MCPane 改进原则：
  - 默认让 Claude Code / Codex 原生直连 Streamable HTTP，不默认依赖 Node CLI Bridge
  - Bridge 仅作为兼容后备
  - Bearer Token 在 HTTP 中间件验证，不当普通 MCP 参数传递
  - Bearer Token 标识客户端连接，`MCP-Session-Id` 单独标识协议会话
  - Tool 使用稳定命名空间，避免同名 Tool 覆盖
  - Workspace 在会话创建时固定为 Catalog Snapshot
  - 正式 Hub 使用 Rust Sidecar，与桌面 UI 进程隔离

## 3. magpie（yetone/magpie，MIT，Go + Wails，2026-09-23 开源）

- 地址：https://github.com/yetone/magpie　站点：https://usemagpie.ai
- 定位：「每个 agent 的模型，一处切换」——20+ 编码 agent 的模型选择器 + 本地多协议网关。
- 重点参考（公开功能层面）：
  - 网关对外暴露通用端点（`/v1/chat/completions`、`/v1/responses`、`/v1/messages`、Gemini），
    任何支持 base URL 的工具都能接，模型统一写成 `provider/model`
  - 路由组（routing group）：多 Key / 多账号按「配额最早重置优先」「顺序」「轮转」「最少使用」
    选择，会话粘性（保提示词缓存）；同名模型多供应商自动成组
  - 额度驱动调度：订阅额度窗口（已用%、重置时间）喂给路由；遇 429 读 `Retry-After` 冷却
  - Agent 侧「每个 agent 各自的模型白名单」（Showing 5 / 32）
  - 切回原生模型时从 stash 精确还原，而非猜默认值
  - Profiles 应用顺序：先 provider、再 model、最后子档位
  - `magpie://` 深链导入供应商（内存暂存一次性取回，UI 确认后落库）
  - 按 agent × 模型的用量与标价成本（models.dev 价目），`magpie usage today|7d|30d`
  - 网关超限归一：把各家「对话装不下」的措辞改写成客户端自己的 API 会说的形态，
    附厂商措辞表与「不误判限速与 max_tokens」的排除规则
  - 工具回合配对：严格 Chat 上游对孤立输出 / 未应答调用直接报错
  - 流失败记账：200 之后才报错的流不记成一次回答，转移到下一成员
  - 远程 MCP 的 OAuth（RFC 9728 / 8414 / 7591 + PKCE，令牌存文件，网关反代）
  - Library 的 MCP 服务器健康状态：逐行展示工具数 / 需要登录 / 启动失败原因 / 连接错误
  - 价目覆盖：magpie 可按供应商 / 模型覆盖标价。MCPane 的用量事件没有供应商维度，故只做按模型覆盖
    （ADR-026）；价目表数据仍以 models.dev 为准
  - 按地址发现 MCP 服务器：Library 里粘贴 GitHub / npm / 远程地址，经地址规范化与匹配打分到
    Registry 找候选（思路参考；规范化与匹配规则 MCPane 自行实现，见 ADR-025）
  - 其余更新（Library 的 skills 与指令集、插件、成本与窗口覆盖、OTLP、多机同步、镜像）评估后不吸收，
    理由见 ADR-022 §10 与 ADR-024
- MCPane 改进原则：
  - 按地址发现只读且不执行：候选仅预填表单、绝不直接保存；Registry 里没有就如实说没有，不抓 README 猜命令
  - 远程 MCP 的 OAuth 复用 rmcp 官方 `auth` 模块，不手写协议；令牌只存本机文件、不进备份与同步（ADR-024）
  - 不做 magpie 那样的网关 `/mcp/<name>` 反代：我们的 Hub 本身就是共享出口
  - 超限归一的措辞表自行按各厂商公开文档整理，不移植其正则
  - **不采用** Claude 订阅桥接与请求「伪装」（驱动本地 `claude` 二进制绕过第三方流量识别、
    伪造 billing header / 指纹）：属规避风控，违背 ADR-022「官方登录不经路由」
  - 不读 shell 环境变量里的 Key 这点一致：凭据只进 `mcpane-secrets`
  - 它的降级只有 1 分钟固定冷却、无熔断半开、无请求整流；ADR-022 已更成熟，勿倒退

## 4. 官方依据（优先级高于竞品）

- MCP 稳定规范（P0 锁定 2025-11-25）：https://modelcontextprotocol.io/specification/2025-11-25
- Streamable HTTP Transport：https://modelcontextprotocol.io/specification/2025-11-25/basic/transports
- MCP 官方 Rust SDK：https://github.com/modelcontextprotocol/rust-sdk
- Claude Code MCP 文档：https://code.claude.com/docs/en/mcp
- Codex MCP 文档：https://developers.openai.com/codex/mcp
- Codex 开源仓库：https://github.com/openai/codex
