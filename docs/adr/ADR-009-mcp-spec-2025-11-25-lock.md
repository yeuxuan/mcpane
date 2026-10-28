# ADR-009：P0 锁定 MCP 2025-11-25

- 状态：**Superseded**（由 [ADR-019](ADR-019-mcp-2026-07-28-adoption.md) 取代，2026-08-07）
- 日期：2026-07-11

> 本 ADR 定的两个「重启评估」信号已于 2026-08-07 复核：rmcp 3.0 stable 已发布
> （现 3.1.1），Claude Code 2.1.224 已内置 2026-07-28 客户端实现。协议改为双栈，
> rmcp 升到 3.1.1，见 ADR-019。以下内容保留作历史记录，**不再是现行约束**。

## 背景

MCP 规范持续演进（下一版本 2026-07-28 在制），SDK 对新草案的支持多以 beta 形式
先行。P0 需要一个稳定、被 Claude Code 与 Codex 双端实现的协议基线。

## 决策

1. P0 协议版本锁定 **`2025-11-25`**（含 Streamable HTTP Transport 语义）。
2. `rmcp` 只使用 crates.io 稳定发行版（基线 2.2.0，对应 release tag
   `rmcp-v2.2.0`）；禁止依赖 Git `main` 或 beta SDK。
3. 对 `2026-07-28` 草案只做 Adapter 层预留（协议版本协商与能力探测抽象），
   不在 P0 实现新草案特性。
4. P0 上游能力面：initialize、notifications/initialized、tools/list、
   tools/call、ping、progress/cancellation 必要转发、best-effort
   `notifications/tools/list_changed`。Resources / Prompts / Sampling /
   Elicitation / Tasks 等推迟到 P1。

## 后果

- 特性锚定 2025-11-25；`MCP-Protocol-Version` 头的校验由 rmcp 执行，
  其 `KNOWN_VERSIONS` 内的多个历史版本（截至 rmcp 2.2.0 共 5 个）均被接受，
  仅未知版本返回明确错误（实际行为见 AGENTS.md「协议版本」一节）；
- 升级协议版本时必须新增 ADR 并重跑全部互操作 Gate。
