# TASK-702：性能基线（首轮实测）

- 日期：2026-07-11
- 环境：macOS（Apple Silicon），debug 构建（release 只会更快），
  Hub 无状态模式，loopback HTTP
- 测量方式：`binaries/mcpane-hub/tests/perf.rs`（100 次采样取 P95；
  每次 CI 均会以计划目标为上限断言，回归即失败）

| 指标 | 计划目标 | 实测（debug） | 余量 |
|---|---|---|---|
| tools/list P95（102 工具 Catalog） | < 100ms | **766µs** | ~130x |
| 路由开销代理：内建 tools/call 端到端 P95（不含下游执行） | < 20ms | **336µs** | ~59x |
| 50 并发下游 Tool Call | 不串线 | **33.6ms 全部完成，ID/内容零串线** | — |

## 说明

- Catalog 为内存 `Arc` 快照（Session-Pinned），tools/list 无锁争用、无 IO；
- "路由额外开销"以内建工具端到端延迟为代理上界（含 HTTP 解析、鉴权、
  中间件、JSON 序列化），真实聚合路由仅多一次 HashMap 查找与二次校验；
- 20 个下游 Server / 空闲内存 / 日志写入不阻塞路由 / UI 万行日志虚拟滚动
  等指标属长时运行与 UI 项，待 Phase 7 完整基准补充（当前日志写入已是
  异步 SQLite，路由路径不等待）。
- 数字调整需按计划写 ADR。
