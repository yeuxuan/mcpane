# ADR-001：Tauri Desktop + Rust Hub Sidecar

- 状态：Accepted
- 日期：2026-07-11

## 背景

MCPane 需要一个桌面控制面（UI）和一个长期运行、可被多个 AI 客户端并发访问的
MCP Hub。两者的生命周期、稳定性要求和故障域完全不同。

## 决策

1. 桌面端使用 **Tauri 2 + React 19**（`apps/desktop`，可执行标识 `mcpane`）。
2. Hub 是**独立 Rust 进程** `mcpane-hub`（Tokio + Axum + 官方 rmcp SDK），
   以 Sidecar 形式随桌面端分发，由 Desktop 的 Hub Supervisor 管理。
3. Desktop 与 Hub 之间通过控制面 IPC（UDS / Named Pipe，见 ADR-007）通信，
   不通过公开的 `/mcp` 数据面。

## 理由

- Hub 崩溃不拖垮 UI；UI 窗口关闭后 Hub 仍可继续服务客户端；
- Hub 可单独重启、升级和诊断；MCP 子进程生命周期与 WebView 隔离；
- 未来可支持 Hub 开机自启，无需启动桌面 UI；
- Tauri + Rust 使桌面端与 Hub 共享领域 crates（`crates/mcpane-*`）。

## 后果

- 需要维护进程监督、版本握手与 IPC 协议（TASK-400）；
- 打包时需正确捆绑 Sidecar 二进制（TASK-703）。

## 参考

- CC Switch 的 Tauri 分层（仅架构参考，见 ADR-008）；
- MCP Router 的聚合进程模型（Electron 内嵌，MCPane 改为独立 Sidecar 以隔离故障域）。
