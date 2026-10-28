# ADR-020 工具暴露：真机验证记录

- 状态：**五项已全部真机验证**（2026-08-08，隔离 Hub 实例 + 真实 CLI）
- 环境：Claude Code 2.1.226、Codex CLI 0.133.0（gpt-5.5）、Hub debug 构建
  （独立 `MCPANE_HOME` + 端口 24999 + devserver stdio 下游，不触生产实例）
- 自动化覆盖见 `binaries/mcpane-hub/tests/aggregation.rs`（ADR-020 段）与
  `http_discover.rs`；本文件记录自动化替代不了的真机行为。

## 验证结果

| # | 项目 | 客户端 | 结果 |
|---|---|---|---|
| 1 | auto→flat 判定 | Claude Code | ✅ `effectiveExposure:"flat"`，4 个下游工具平铺，三个 meta 工具正确隐藏。Claude Code 2.1.226 仍走 legacy `2025-11-25`，auto 判定在 legacy 路径同样生效 |
| 2 | 网关目录可见性 | Claude Code（显式 gateway）/ Codex（gateway） | ✅ Claude Code：instructions 目录被注入模型上下文（模型原文复述了目录与三个 meta 工具）。Codex：5 个 meta 工具全部可见可调，browse 无参返回全量目录（4 条、无 schema）。Codex 0.133 走 legacy `2025-06-18` |
| 3 | describe→升格→原生调用 | Codex | ✅ 已回答，**答案是否**：describe 返回 `promoted:true`（服务端升格成功），但 Codex 0.133 legacy 收不到 listChanged、工具列表不刷新，原生调用不可用——**execute 是 Codex 的主通道，准确率封顶 ② 级**。升格层现实受益者当前为空集（Claude Code 按设计走平铺），定位为「给将来支持 2026-07-28 + 列表刷新的客户端铺路」，与 ADR-020 预判一致 |
| 4 | execute 缺参自愈 | Claude Code（gateway） | ✅ 真实模型一轮闭环：describe 取 schema → 故意缺参 execute → 收到「本地参数校验发现 + 完整 inputSchema」→ 按提示补参 → 成功 |
| 5 | 模型「肯查目录」 | Claude Code（gateway） | ⚠️ 部分：任务措辞泛化（「用可用的工具搜索」）时模型选择了 harness 自带 WebSearch 而非 Hub 工具——**内置同类工具的竞争是教义压不住的**；点名 Hub 或任务无内置替代时全流程顺滑（browse→describe→execute 模型自主完成） |

## 真机抓到并已修复的 bug

**布尔 schema 导致 Claude Code 丢弃整张 tools/list**：schemars 对无 doc 注释的
`Option<serde_json::Value>` 字段生成 JSON Schema 布尔形态 `true`（规范合法），
`mcpane__tool_describe` 的 outputSchema 带上它后，Claude Code 2.1.226 解析失败并
**静默丢弃该 server 的全部工具**（instructions 正常注入、无任何报错——网关模式
下所有工具消失，flat 模式因无 describe 工具而幸免）。修复：字段类型改
`Option<Map<String, Value>>`；守卫 `builtin_tool_schemas_never_contain_boolean_schemas`
（已反向验证：裸布尔形态会转红）。教训与「拒绝方式无法穷举」同构：**客户端的
解析容错无法穷举，wire 形态必须保守**。

## 遗留问题处置（2026-08-08 晚，调研后落地）

- **下游脏 schema 连坐**：真机 bug 的推论——布尔 schema 若来自下游同样毒死
  聚合表，且生态还有两类同级杀伤（顶层组合子 → Claude API 400 会话不可恢复
  claude-code#45106；顶层缺 type → 严格客户端拒绝 #10031）。已在 catalog 入口
  内建规范化（`sanitize.rs`），社区此前只能靠手写 stdio 代理解决。
- **Codex headless 审批取消**：`default_tools_approval_mode = "approve"`
  （Codex ≥0.122）可解，已写进一键接入的 config 补丁（openai/codex#24135）。
- **模型偏好宿主内置工具（测试项 5 的 ⚠️）**：判定为测试设计噪音为主——
  devserver__search 自述就是「返回固定结果的开发工具」，模型选 WebSearch 是
  理性行为；真实下游（firecrawl/context7 等）自带触发式描述。不改代码；
  需要硬约束的用户可用「工作区使用准则」写路由规则。

## 其他真机事实

- Codex exec 非交互模式对 `destructive_hint=true` 的工具（tool_execute 如实申报）
  **自动取消**（`--full-auto` 也不放行，须 bypass 或交互式审批）；wire 路径本身
  验证通畅（bypass 后 execute 正常返回）。交互式 Codex 会弹审批，属 Codex 策略
  行为而非本实现缺陷。
- instructions 通道对 Claude Code 确认有效（目录文本进入模型上下文）；对 Codex
  未单独验证 instructions 注入，但 browse 确定性通道已兜底。
- Claude Code 的工具名前缀映射（`mcpane__tool_discovery` →
  `mcp__<server>__mcpane__tool_discovery`）会让模型按 instructions 里的短名检索
  失败——网关 instructions 文案不写死全名是对的（前缀因客户端配置而异），
  模型在工具可见时能自行对应。
