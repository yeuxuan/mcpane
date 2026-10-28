# ADR-002：统一 Streamable HTTP `/mcp` 入口

- 状态：Accepted
- 日期：2026-07-11

## 背景

Claude Code 与 Codex 均原生支持 MCP Streamable HTTP Transport。MCP Router 采用
Node CLI Bridge（stdio ↔ HTTP）作为默认接入方式，增加了一层进程与故障点。

## 决策

1. Hub 对上游只暴露**一个** Streamable HTTP Endpoint：
   `http://127.0.0.1:24860/mcp`（POST / GET / DELETE）。
2. Claude Code 与 Codex **默认原生直连**该地址，不默认依赖 Bridge。
3. `mcpane-bridge`（stdio ↔ Streamable HTTP）仅作为兼容后备，只为真实测试
   中直连失败的客户端启用（GATE-1 兜底路径）。
4. 数据面仅监听 `127.0.0.1`（可选 `::1`），不监听 `0.0.0.0`；校验 `Host`，
   `Origin` 存在时必须校验。
5. 普通请求优先返回 `application/json`；仅在通知、进度或多消息响应时使用 SSE。

## 理由

- 单一入口 = 单一防护面（认证、日志、限流集中一处）；
- 去掉默认 Bridge 减少 Node 运行时依赖与串联故障；
- 与 MCP 2025-11-25 规范的 Streamable HTTP 语义一致。

## 后果

- 必须在 GATE-1 用真实发布版 Claude Code 与 Codex 验证直连（TASK-101/102/103）；
- 若失败，按计划降级到最小 Bridge，并在本 ADR 追加失败版本与原因记录。

## GATE-1 验证结果（2026-07-11）

- Claude Code 2.1.207 与 codex-cli 0.133.0 原生直连**双双通过**
  （报告：`docs/interop/claude-codex-same-endpoint.md`）；
- **无需启用 Bridge**，`mcpane-bridge` 维持"仅兼容后备"定位；
- 已知上游缺陷及 MCPane 侧对策：Codex exec 会取消无 ToolAnnotations 的
  自定义工具调用（openai/codex#16685）——MCPane 内建工具一律携带完整
  注解（`docs/interop/codex-direct-http.md`），下游聚合透传时需标注风险。

## Gate 补记（2026-08-31）：下游注解的用户声明口

**为什么开这个口子。** 上面那条"下游注解缺失时不伪造、原样透传"原则本身没问题，
但它给不申报 annotations 的下游留下了一个死结：这类工具在 headless `codex exec`
下**完全不可用**——Codex 把无注解工具误路由进 app-tool 审批路径，非交互模式的
stdin EOF 被解释成"用户拒绝"，调用一律报 `user cancelled MCP tool call`
（openai/codex#16685、#29857、#24135，修复 PR #16632 截至 0.133.0 未合入）。
内建工具当时靠补全注解治了本，下游工具则毫无出口：用户既改不了别人的 Server，
Hub 又不肯代填，工具就此永久不可用。

**决定。** 允许用户为**单个下游工具**声明一份 ToolAnnotations
（`tool_declared_annotations` 表，迁移 0025）。

声明的键是 `(server_id, tool)`，**不含 workspace**。注解陈述的是工具行为
（"这个工具只读"），不是按项目分的策略：挂在工作区上会让用户每建一个工作区
就重标一遍，更糟的是同一个工具能在两个工作区里被声明成互相矛盾的两种行为
——一个以"谁说的要分得清"为全部前提的功能，不该允许用户自相矛盾。
独立成表的附带好处：启停/别名与注解在物理上就不可能互相覆盖。

**边界——这不是"允许伪造"，四条约束把口子焊死在补空位上：**

1. **只补空位，不改口径。** 下游一旦自己申报了 annotations，用户声明自动让位，
   出口原样透传下游那份。改写下游的自述才是伪造，本口子不提供这个能力
   （守卫：`downstream_annotations_are_never_overridden_by_user_declaration`）。
2. **两个来源分列而存。** `CatalogTool.annotations`（下游申报）与
   `CatalogTool.declared_annotations`（用户声明）是两个字段、库里是两张表，
   任何一处都分辨得出这句话是谁说的。
3. **Hub 自己仍然永不代填。** 下游没申报、用户也没声明，出口就是没有注解
   （守卫：`hub_never_fabricates_annotations`）。默认行为与本 Gate 原判一致。
4. **署名可见。** 桌面端工具行标注"注解：你声明的"（工作区页与 MCP 服务器页
   的工具面板都标；后者还按服务器汇总一句"这个服务器有 N 个工具没申报注解"
   ——刚装完服务器正是发现它的时机），`mcpane__tool_describe`
   返回 `annotationsSource: downstream | user`，Catalog 重建时按工具记一条
   info 日志。上游 `ToolAnnotations` 是规范里的闭合结构、塞不进自定义标记，
   所以署名只走这三条带内之外的通道。

**已知取舍。** 用户认为下游申报得不对（比如把会写盘的工具标成 `readOnlyHint`）
时，本口子帮不上忙——那需要的是覆盖下游申报，与约束 1 直接冲突。真遇到了应当
禁用该工具或去上游反馈，而不是在 Hub 里替它改说法。

实现与复现见 `docs/interop/tool-annotations-declaration.md`。

**为什么不是 Hub 自动补。** ADR-021 的四变体探针实测：Codex 认的是
`readOnlyHint`，不是「有没有 annotations 字段」——只带 `title` 的空壳注解照样
被取消。`title` 补了不构成替下游声明语义但**不管用**，管用的那句恰恰是安全
断言。这条实验把「自动补空壳」这个看似无害的选项堵死了，也顺带定出了最小
必要断言（一个 `readOnlyHint`），弹窗文案据此告诉用户「勾一项就够」。
