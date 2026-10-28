# ADR-025：按地址发现 MCP 服务器（官方 Registry）

- 状态：Accepted
- 日期：2026-10-04
- 关联：ADR-017（出站网络与代理）、ADR-024（远程 MCP 的 OAuth）、`docs/competitors/REFERENCES.md`（magpie）

## 背景

添加 MCP 服务器只有十来个策展预设、手填和导入 JSON。用户看到一个 GitHub 仓库、npm / PyPI 包名或远程网址，
得自己去查启动命令与环境变量——这是 Hub 核心流程里最大的摩擦。官方 MCP Registry
（`registry.modelcontextprotocol.io`）公开了结构化的安装信息，正好可用；与 ADR-024 衔接后，
粘贴 `https://mcp.notion.com/mcp` 能一路走到「需要登录 → 登录 → 可用」。

## 决策

### 1. 只读、不执行、只预填

发现阶段**不调 npx / uvx**，候选交给界面预填现有的「添加服务器」表单，**绝不直接保存**：用户在表单里看得到
完整命令，保存前可先「测试连通」。Registry 是公开的第三方登记（实测混有大量仿冒 / 同名条目），所以：

- 固定到 Registry 登记的版本（`npx -y pkg@x.y.z`、`uvx pkg@x.y.z`）；
- 展示命名空间原文、仓库链接，以及「地址一致 / 未经 Registry 登记 / 暂不支持」徽标与固定的核对提示；
- Registry 的文字当纯文本渲染并限长，非 https 的链接丢弃；
- 远程端点只收 https（本机回环的 http 除外）。

### 2. 新 crate `mcpane-registry`（纯逻辑 + 薄客户端）

`address`（解析与规范化）→ `resolve`（检索词、并发、精确匹配）→ `candidate`（条目映射）→ `client`（HTTP）。
`resolve` 的检索函数是注入的，单测不碰网络。出网走 `mcpane_net::client_for`（ADR-017：代理与内网豁免），
因为应用 CSP 是 `default-src 'self'`，前端不能直接请求外部地址。

### 3. 实测得出的 Registry 事实（设计依据）

- **搜索只匹配服务器名的一部分**：`context7` 命中，只写 owner（`crystaldba`）或带空格的词都是 0 条。
  所以地址要自己拆成检索词，再用 `repository.url` / 包标识符 / 远程 URL 的**规范化值精确匹配**。
- **npm 包名与 Registry 名常对不上**：`@upstash/context7-mcp` 在 Registry 里叫 `io.github.upstash/context7`。
  检索词因此同时带「去掉 `mcp` 前后缀」的变体（最多 3 个并发；Registry 单次响应 5–30 秒，串行会叠加）。
- `version=latest` 才能去重；同一包可登记 stdio / sse / streamable-http 多种变体。

### 4. 候选生成规则（来自真实条目的坑）

- npm → `npx -y <id>@<version>`；pypi → `uvx <id>@<version>`；条目带 `--from` 运行时参数时包名已由它指定，
  不再追加 `<id>@<version>`（否则 `uvx` 去跑一个不存在的可执行名）。
- 具名参数补 `--` 前缀（Registry 里既有 `--port` 也有裸的 `allowed-directories`）；缺值的必填参数不预填，
  记入「待办」提示。可选且无值的环境变量不预填（空串会被部分服务器当成「已配置」），只在卡片上提示变量名。
- **包的传输方式不是 stdio**（sse / streamable-http，要先在本机监听端口）→ 暂不支持，不当 stdio 处理。
- oci / nuget / mcpb → 明确标「暂不支持」，不拼命令。
- 远程：同一服务器同时有 streamable-http 与 sse 时只列前者；`Authorization` 头模板不预填成请求头行，
  但条目要求时加一条「在 Bearer Token 里填写」的提示（多数是 OAuth 服务器，走 ADR-024 登录）。
- 预填用的远程地址**保留原协议与主机**（`www.` 不擅自去掉、http 不悄悄改成 https），只丢 userinfo / query /
  fragment；与 Registry 条目比较时才用不看协议的规范化值。

### 5. 没找到时怎么办

- npm / PyPI 包名与远程 URL：按字面意思生成「直接候选」，徽标「未经 Registry 登记」；明文 http 的远程不生成。
- GitHub 仓库：提示「Registry 里没有」，**不抓 README 猜命令**。
- 一路检索失败而另一路成功：用成功的结果并提示「结果可能不全」；全部失败才报错。
- 10 分钟内相同检索词走内存缓存。界面对过期响应（用户中途改了查询）丢弃，且查询期间不禁用输入框回车。

## 不做

OCI / NuGet / MCPB；从 README 推断命令；代理 Registry 之外的目录（Smithery 等）；自动保存或自动安装。

## 验证

- `mcpane-registry` 单测与集成测试，条目映射用**真实抓取的 Registry JSON** 做夹具（10 个条目，覆盖 npm 多变体、
  pypi `--from`、oci / nuget / mcpb、smithery 的 Authorization 头、官方 Notion 的 http + sse）；
  `resolve` 用注入的假检索函数。关键行为做过反向验证（去掉精确匹配置顶 / 去掉不支持类型守卫，对应测试必然变红）。
- 序列化回归：内部标签枚举不能带「裸字符串」变体（`Address::Words` 曾因此在运行时才序列化失败）。
- 前端：候选 → 表单预填的纯函数单测；Playwright（mock IPC）覆盖精确匹配置顶、「使用」只填表不保存、
  远程落到远程页签、暂不支持无按钮、过期响应丢弃（已反向验证）、未登记提示、失败原因。
- 真实 Registry（GET 只读）：`context7`、GitHub 地址、`@upstash/context7-mcp`、`https://mcp.notion.com/mcp`
  命中「地址一致」；`crystaldba/postgres-mcp` 无结果并提示。`cargo test -p mcpane-registry -- --ignored` 复现。
- **未验证**：桌面应用里点击「按地址查找」的完整界面流程（Tauri 开发构建无法被自动驱动，只有 Playwright mock）；
  受限网络 / 代理环境下 Registry 的可达性；Windows。均已写入发版清单。
