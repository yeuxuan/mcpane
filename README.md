# MCPane

**Claude Code、Codex 与 MCP 工具的统一控制面。**

装了好几个 AI 编程工具，每个都要单独配一遍 MCP Server、单独管一套供应商
密钥？MCPane 把这些收进一个桌面应用统一打理：MCP Server 只配一次、
所有客户端共用，供应商一键切换，每次配置改动都可回滚。

本仓库用于分发 MCPane 安装包，不含源代码。

![概览页：MCP Hub 运行状态、已接入的客户端与下游服务一目了然](screenshots/dashboard.png)

## 下载

到 [Releases](https://github.com/yeuxuan/mcpane/releases/latest) 取对应平台的安装包：

| 平台 | 下载 | 系统要求 |
|---|---|---|
| macOS · Apple Silicon（M 系列） | `MCPane_x.y.z_aarch64.dmg` | macOS 12.0+ |
| macOS · Intel | `MCPane_x.y.z_x64.dmg` | macOS 12.0+ |
| Windows · x64 | `MCPane_x.y.z_x64-setup.exe` | Windows 10 1803+ |

不确定 Mac 是哪种芯片：左上角苹果菜单 → 关于本机，看「芯片」一行。

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

- **MCP Server 只配一次**——所有客户端共用同一个本地 Hub
  （`http://127.0.0.1:24860/mcp`），各自独立 Token 与会话。支持 stdio、
  Streamable HTTP、SSE 三类 Server
- **八家客户端一键接入**——Claude Code、Claude Desktop、Codex、Gemini CLI、
  Grok、OpenCode、OpenClaw、Hermes。按各家原生格式读写配置，
  你手写的注释、未知字段与键序原样保留
- **供应商一键切换**——官方订阅与第三方中转分开管理，
  支持配额查询与节点测速
- **精细控制**——按项目逐条启停 Server 与工具，工具可设别名；
  工具很多时可开启动态发现，避免占满模型上下文
- **改坏了能回来**——每次写入前自动快照，任何一次变更可回滚；
  配置被其他工具改动时会提醒，绝不静默覆盖
- **用量心里有数**——请求日志、近 24 小时统计、会话成本估算
- **数据在自己手里**——本地自动备份，可选 WebDAV / S3 云同步
- **安静常驻**——系统托盘常驻、可开机自启，不弹窗口、不占 Dock

![MCP 服务器页：逐个启停、工具计数与运行状态](screenshots/servers.png)

![客户端页：八家客户端的检测与一键接入](screenshots/clients.png)

## 更新

应用会在启动时静默检查新版本，发现后以横幅提示，就地下载安装。
更新包经签名校验，可在设置页关闭自动检查。

## 反馈

问题与建议请提 [Issues](https://github.com/yeuxuan/mcpane/issues)。

---

© 2026 MCPane
