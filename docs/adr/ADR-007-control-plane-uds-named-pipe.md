# ADR-007：控制面使用 UDS / Named Pipe

- 状态：Accepted
- 日期：2026-07-11

## 背景

Desktop 需要对 Hub 执行管理操作（status / reload / shutdown / rotate-token /
diagnostics）。这些操作绝不能暴露在客户端可访问的 `/mcp` 数据面上。

## 决策

1. 控制面与数据面物理分离：
   - macOS：Unix Domain Socket，文件权限 `0600`；
   - Windows：Named Pipe，ACL 仅限当前用户。
2. 管理操作不经过 `/mcp`；Hub 状态以事件方式推送给桌面端。
3. 控制面协议带版本号，Desktop 与 Hub 版本不兼容时明确提示。

## 理由

- OS 级别的文件权限/ACL 即为认证边界，无需在 loopback TCP 上再造一层管理鉴权；
- 数据面被攻破（如 Token 泄漏）时，管理面不受影响。

## 后果

- 需要跨平台 IPC 抽象 `mcpane-control-ipc`（TASK-400）；
- P0 早期（TASK-100）先提供最小只读状态端点，完整控制面到 Phase 4。
