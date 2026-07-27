# MCPane

**Claude Code、Codex 与 MCP 工具的统一控制面。**

本仓库用于分发 MCPane 安装包，不含源代码。

## 下载

到 [Releases](https://github.com/yeuxuan/mcpane/releases/latest) 取对应架构的
安装包，要求 macOS 12.0 及以上：

| 你的 Mac | 下载 |
|---|---|
| Apple Silicon（M 系列） | `MCPane_x.y.z_aarch64.dmg` |
| Intel | `MCPane_x.y.z_x64.dmg` |

不确定是哪种：左上角  → 关于本机，看「芯片」一行。

Windows 版尚未发布。

## 首次打开

当前安装包未经 Apple 公证，首次打开会提示「无法验证开发者」。放行方式：

1. 双击一次（会被拦下），点「完成」
2. 打开「系统设置 → 隐私与安全性」，滚到底部会看到 MCPane 被阻止的提示
3. 点「仍要打开」，再确认一次

只需做一次。此后的版本升级由应用内自动更新接管，不会再触发该提示。

## 功能

- **统一 MCP Hub** —— 多个客户端共用同一个 `http://127.0.0.1:24860/mcp`，
  各自独立 Token 与会话。下游支持 stdio、Streamable HTTP、SSE
- **八家客户端一键接入** —— Claude Code、Claude Desktop、Codex、Gemini CLI、
  Grok、OpenCode、OpenClaw、Hermes。按各自原生格式读写配置，
  注释、未知字段与键序一律保留
- **供应商管理** —— 官方订阅与第三方中转分类管理，一键切换，
  配额查询与节点测速
- **逐条启停** —— Project / Workspace 级的 Server 与 Tool/Prompt/Resource
  精细控制，工具支持别名；工具动态发现模式避免大工具集挤爆模型上下文
- **配置安全** —— 写入前生成最小 Patch 与快照，原子写入，失败自动恢复，
  任意变更可回滚，外部改动不静默覆盖
- **观测** —— 请求日志、近 24 小时聚合统计、会话成本估算
- **数据** —— 本地备份与 WebDAV / S3 云同步

## 更新

应用会在启动时静默检查新版本，发现后以横幅提示，就地下载安装。
更新包经签名校验，可在设置页关闭自动检查。

## 反馈

问题与建议请提 [Issues](https://github.com/yeuxuan/mcpane/issues)。

---

Copyright © 2026. 保留所有权利。
