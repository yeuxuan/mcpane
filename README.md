# MCPane

**Claude Code、Codex 与 MCP 工具的统一控制面。**

本仓库用于分发 MCPane 安装包，不含源代码。

## 下载

到 [Releases](https://github.com/yeuxuan/mcpane/releases/latest) 取对应平台的安装包：

| 平台 | 下载 | 系统要求 |
|---|---|---|
| macOS · Apple Silicon（M 系列） | `MCPane_x.y.z_aarch64.dmg` | macOS 12.0+ |
| macOS · Intel | `MCPane_x.y.z_x64.dmg` | macOS 12.0+ |
| Windows · x64 | `MCPane_x.y.z_x64-setup.exe` | Windows 10 1803+ |

不确定 Mac 是哪种芯片：左上角  → 关于本机，看「芯片」一行。

## 首次打开

当前安装包未经代码签名，首次打开会被系统拦下。放行一次即可，
此后的版本升级由应用内自动更新接管，不会再触发。

**macOS**——提示「无法验证开发者」：

1. 双击一次（会被拦下），点「完成」
2. 打开「系统设置 → 隐私与安全性」，滚到底部会看到 MCPane 被阻止的提示
3. 点「仍要打开」，再确认一次

**Windows**——SmartScreen 提示「已保护你的电脑」：

1. 点提示里的「更多信息」
2. 点「仍要运行」

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
- **常驻后台** —— 系统托盘常驻，可设为开机自启：登录后静默驻留托盘，
  不弹窗口、不占 Dock，Hub 随之就位

## 更新

应用会在启动时静默检查新版本，发现后以横幅提示，就地下载安装。
更新包经签名校验，可在设置页关闭自动检查。

## 反馈

问题与建议请提 [Issues](https://github.com/yeuxuan/mcpane/issues)。

---

Copyright © 2026. 保留所有权利。
