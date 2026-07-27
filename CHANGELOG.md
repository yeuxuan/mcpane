# 更新日志

本文件记录 MCPane 每个发布版本的变更，格式遵循
[Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。


## [Unreleased]

## [0.1.0] - 2026-07-27

首个公开预览版。0.x 期间配置结构与数据格式仍可能调整，升级前建议在
设置页导出一次备份。

### 统一 MCP Hub

- 独立 Hub 进程（`mcpane-hub`），多客户端共用同一个
  `http://127.0.0.1:24860/mcp`，各自独立 Token 与 Session，互不串线
- 下游支持 stdio、Streamable HTTP、SSE（legacy）三种传输
- Tools / Prompts / Resources 三类聚合与命名空间隔离，
  下游 `list_changed` 通知触发 Catalog 自动重建
- 工具动态发现模式：`tools/list` 只暴露检索与执行两个 meta 工具，
  避免大工具集挤爆模型上下文
- Session-Pinned 语义、调用二次权限校验、下游健康监控与自动恢复
- 只支持 stdio 的客户端经 `mcpane-hub bridge` 接入，不需要 node/npx
- 应用托管 Hub sidecar：自动拉起、崩溃退避重启、退出优雅关停，
  已有实例则 adopt 而非重复拉起

### 客户端接入

- 八家客户端一键接入并深度托管配置：Claude Code、Claude Desktop、
  Codex、Gemini CLI、Grok、OpenCode、OpenClaw、Hermes
- 各客户端按其原生格式读写：JSON、JSONC、JSON5、YAML、TOML，
  注释、未知字段与原有键序一律保留
- 凭据投递双模式（直连 / 代理）与 envsync 代管环境变量
- 客户端 Server 级访问控制（deny-list）

### 供应商管理

- 官方订阅与第三方中转站分类管理，Claude 系与 Codex 系各自独立切换
- 供应商编辑器直接呈现目标配置文件全文（Codex 走 TOML 原生格式），
  仅隐藏工具自身受管的认证路由键
- 配置片段、配额查询、节点测速、模型列表拉取
- 内置预设仅收厂商官方与中立聚合入口

### 配置安全

- 写入前生成最小 Patch 与快照，原子写入，失败自动恢复
- 任意一次变更可回滚，操作历史独立成页
- 外部修改检测：文件被其他工具改动时提醒，不静默覆盖

### 数据与观测

- Hub 请求日志（含脱敏）与近 24 小时聚合统计仪表
- 会话解析与成本估算，用量独立成页
- 本地备份，以及 WebDAV / S3 云同步与自动同步（带乐观锁）

### 桌面端

- Tauri 2 + React 19，浅色 / 深色双主题
- 多 Workspace，Project / Workspace 级 Server 与 Tool/Prompt/Resource
  逐条启停，tool/prompt 支持别名
- 系统托盘常驻（含 Provider 快捷切换），关闭即驻托盘的轻量模式
- 简体中文 / English 双语
- 首次启动按需触发的分步引导，聚光高亮真实控件
- 全局出网代理与本地代理探测

### 自动更新

- 应用内检查、下载、安装、重启，更新包经 minisign 签名校验
- 启动时静默检查，发现新版本以横幅提示；可在设置页关闭自动检查
- 更新检查遵循应用内配置的代理设置

### 已知限制

- **本版仅发布 macOS（Apple Silicon 与 Intel 分别打包），
  最低 macOS 12.0。** Windows 支持已在代码层实现（控制面 Named Pipe 经交叉
  编译核验），但尚未经真机运行时验证，故本版不发布 Windows 安装包。
- **安装包未经 Apple 公证**，首次打开会被 Gatekeeper 拦下，
  需在「系统设置 → 隐私与安全性」中点「仍要打开」。

[Unreleased]: https://github.com/yeuxuan/mcpane/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/yeuxuan/mcpane/releases/tag/v0.1.0
