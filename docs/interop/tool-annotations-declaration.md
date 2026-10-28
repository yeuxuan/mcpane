# 下游工具注解的用户声明口（ADR-002 Gate 补记的实现）

- 日期：2026-08-31
- 相关：ADR-002「Gate 补记（2026-08-31）」、`docs/interop/codex-direct-http.md`
  「过程中发现并解决的互操作问题」

## 要解决的问题

下游 MCP Server 不申报 `ToolAnnotations` 时，它的工具在 headless `codex exec`
下**完全不可用**：Codex 把无注解工具误路由进 app-tool 审批路径，非交互模式读不到
stdin（EOF）被解释成"用户拒绝"，调用一律返回 `user cancelled MCP tool call`。
`approval_policy="never"`、`default_tools_approval_mode="auto"` 都压不住。

上游缺陷：openai/codex#16685、#29857、#24135；修复 PR #16632 截至 0.133.0 未合入。

MCPane 的内建工具早已靠补全注解治本，但下游工具此前没有任何出口——用户改不了
别人的 Server，Hub 又按 ADR-002 Gate 拒绝代填。

## 做法

为单个下游工具存一份**用户声明的**注解，出口只在下游缺席时采用。

| 层 | 位置 |
| --- | --- |
| 库 | `tool_declared_annotations` 表（迁移 0025） |
| 类型 | `mcpane_hub_protocol::DeclaredAnnotations`（四个 hint，storage/catalog/桌面端共用） |
| 目录 | `CatalogTool.annotations`（下游申报）与 `.declared_annotations`（用户声明）分列 |
| 决策 | `CatalogTool::effective_annotations() -> Option<(Value, AnnotationSource)>` |
| 合并 | `control.rs::load_overrides` 每个工作区重建时并入同一份全局声明 |
| 出口 | `catalog_tool_to_rmcp`（tools/list）、`mcpane__tool_describe` |
| 写入 | `tool_annotations_set` 命令 → `ServerRepo::set_tool_annotations` |
| 界面 | 工作区页工具行 + MCP 服务器页工具面板，两处都有「注解」按钮 |

四个 hint 一次全给，不做部分声明：触发上游缺陷的是"整个 annotations 缺席"，
半份声明既修不干净、又要在界面上多出一堆三态。

### 为什么键里没有 workspace

注解陈述的是工具行为（"这个工具只读"），跨工作区恒成立；它不是按项目分的策略，
和 `enabled`/`alias` 那种真·工作区级治理不是一回事。挂在工作区上有两个坏处：
用户每建一个工作区就要把同样的话重说一遍；且同一个工具能在两个工作区里被声明成
互相矛盾的两种行为——一个以"谁说的要分得清"为全部前提的功能，不该允许用户自相矛盾。

独立成表还顺带消掉了一整类 bug：启停/别名在 `workspace_tools`，注解在
`tool_declared_annotations`，两者在物理上就不可能互相覆盖，不用靠"UPSERT 只列
自己那几列"这种纪律来维持。

合并发生在 `load_overrides`：用 `entry().or_default()` 而**不是** `insert`——
一个工具可能既被工作区禁用、又有全局声明，整条覆盖项换成默认值会把禁用悄悄翻回
启用（本仓最忌讳的「关掉的东西自己回来」）。守卫钉住了这一点。

### 两处入口

- **工作区页**：治理主场，按工作区看全部工具，逐行标注解状态。
- **MCP 服务器页的工具面板**：刚装完一个服务器就是撞上这个坑的时机——面板顶部
  按服务器汇总一句「这个服务器有 N 个工具没申报注解」。"整个服务器一个注解都不
  申报"本就是服务器级的事实（取决于它怎么写的），在这个单位上观察比在工作区里
  混着别的服务器的工具看更清楚。

两处都只说明、不代劳：逐个工具确认，不给一键铺全部。

注解弹窗开在 MCP 服务器编辑弹窗**里面**，这是全仓第一处真正的嵌套 Modal。
共享 `Modal` 此前没为这种用法设计过：ESC 监听挂在 window 上，两层都收到同一次
按键，一按全关（用户在外层编辑器里的位置也一并丢了）。已改成按层序只让最上层
处理。层序取自**渲染期**序号而非 effect 里 push 栈——React 的 effect 是子先父后
执行的，用栈算出来的"栈顶"恰是最外层，方向正好反；这个坑是写守卫时当场踩中并
被用例抓出来的。

## Codex 到底认哪个字段（四变体探针，2026-08-31）

本文档写成之后补跑的实验，回答一个直接影响本方案存废的问题：Codex 看的是
「有没有 annotations 字段」，还是「有没有具体的行为断言」？如果是前者，
Hub 大可自动补一个空壳，根本不必让用户动手。

同一工具、同一提问，只换注解形态：

| 变体 | annotations | 结果 |
|---|---|---|
| none | 不申报 | 发起了但被取消 ⛔ |
| title | `{"title": "..."}` | **发起了但被取消** ⛔ |
| readonly | `{"readOnlyHint": true}` | **调用成功** ✅ |
| full | title + 四项 hint 齐全 | 调用成功 ✅ |

**结论一：Codex 认的是 `readOnlyHint`，不是「字段存在」。**

这条实证支撑了本方案的边界（见下节）：`title` 是纯显示名、补它不构成替下游
声明语义，**但它不管用**；管用的 `readOnlyHint: true` 是一句安全断言——声明
「本工具不修改环境」，客户端据此跳过审批。下游那工具会不会写数据，Hub 无从
得知，替它断言等于拿用户的数据赌一个我们没有的知识。**所以只能让用户声明，
Hub 自动补空壳这条路是被实验堵死的，不是没想到。**

**结论二：最小必要断言就是 `readOnlyHint` 一个字段。**

`ToolAnnotationsModal` 的文案据此写：勾一个「只读」就够解决 Codex 的问题，
不必让用户填满四项 hint。其余 hint 是给别的客户端看的额外信息，不是必填项。

实验记录见 `docs/interop/tool-adoption-adr021.md`。

## 边界：为什么这不是"允许伪造下游"

见 ADR-002 Gate 补记的四条约束。落到代码上：

- **下游优先**：`effective_annotations()` 先看 `annotations`，非空就原样返回，
  用户声明连读都不读；
- **永不代填**：两边都空返回 `None`，出口不写 `annotations` 字段；
- **署名**：`mcpane__tool_describe` 带 `annotationsSource: downstream | user`；
  Catalog 重建按工具记 info 日志（"采用用户声明的注解" / "用户声明未生效"）；
  桌面端工具行标 `注解：你声明的`。上游 `ToolAnnotations` 是规范闭合结构，
  塞不进自定义标记，所以带内无法署名，只能走这三条通道。

## 守卫与反向验证

每条守卫都按项目规矩改坏源码复核过——绿灯不代表守得住。

| 守卫 | 位置 |
| --- | --- |
| `hub_never_fabricates_annotations` | `crates/mcpane-hub-catalog/src/lib.rs` |
| `user_declaration_fills_the_gap_when_downstream_is_silent` | 同上 |
| `downstream_annotations_are_never_overridden_by_user_declaration` | 同上 |
| `declarations_are_global_not_per_workspace` | `crates/mcpane-storage/src/servers.rs` |
| `withdrawing_a_declaration_removes_the_row` | 同上 |
| `corrupt_declaration_degrades_to_unset_without_losing_the_rest` | 同上 |
| `global_declarations_are_merged_into_every_workspace_catalog` | `binaries/mcpane-hub/tests/aggregation.rs` |
| `declared_annotations_reach_tools_list_without_overriding_downstream` | `binaries/mcpane-hub/tests/aggregation.rs` |
| `catalog_tool_json_keys_match_the_desktop_and_mcp_contract` | `crates/mcpane-hub-catalog/src/lib.rs` |
| 嵌套弹窗 ESC 三条 | `apps/desktop/src/components/ui.modal.test.tsx` |

反向验证记录（六次改坏，六次转红）：

| # | 改坏方式 | 转红的守卫 |
| --- | --- | --- |
| 1 | `effective_annotations` 优先级颠倒（用户盖过下游） | 单测 + e2e |
| 2 | 两边都没有时 `unwrap_or_default()` 代填 | 单测 + e2e |
| 3 | 用户声明被无声忽略（返回 `None`） | 单测 + e2e |
| 4 | 坏 JSON 不降级而是 panic | storage |
| 5 | `declaredAnnotations` 键名改成 snake_case | 契约守卫 |
| 6 | `load_overrides` 整段漏掉读库合并 | 合并守卫 |
| 7 | 合并用 `insert` 而非 `entry().or_default()` | 合并守卫 |
| 8 | Modal 取消 ESC 层序判断（两层都吃） | 嵌套弹窗守卫 |
| 9 | Modal 层序方向搞反（`min` 代替 `max`） | 嵌套弹窗守卫 |

第 7 条是反向验证**当场抓出来的漏网**：第一版合并守卫只断言注解到位，
用 `insert` 破坏后照样全绿——而那个版本会把"既被禁用、又有声明"的工具悄悄翻回
启用。补上禁用态断言才转红。

第 5 条守的是一类**没有失败信号**的错：键名一旦漂移，桌面端读不到就永远显示
「无注解」，客户端读不到 `readOnlyHint` 就当没申报——两边都不报错。这几个键名
同时是 `apps/desktop/src/lib/ipc.ts` 里 `CatalogTool` / `DeclaredAnnotations`
两个 interface 的字段名，改一边必须改两边。

## 升级与数据迁移

0025 只新建一张表，不动任何既有表——升级路径无风险，`workspace_tools` 的存量
行完全不受影响。

`server_id` 不加外键，与 `workspace_tools.server_id` 同口径：`mcp_servers` 被
CHECK 逼着重建过两次（0005/0007），每多一张 CASCADE 子表就多一个"重建父表前
先备份子表"的陷阱（见 0007 注释）。删 Server 后残留的声明行无害——键再也匹配
不上；这与 `workspace_tools` 现有的行为一致。

恢复**旧备份**（无此表）到新库安全：`import_attached` 只处理两侧共有的表
（`local_tables.contains(table)`），旧快照里没有这张表就不动它——本机已有的
声明原样保留。反向（新快照进旧应用）由 schema 版本闸门挡住。

**第 1 次反向验证抓到过一个假绿灯，值得记一笔**：e2e 守卫最初给两个下游写了
**相同**的声明值，而 stdio devserver 自己申报的恰好也是这一组值——优先级颠倒后
算出的 JSON 一模一样，断言等于没写。改成给 `srv_github` 一份**处处相反**的声明
后才真的转红。测试里的取值撞上被测数据，断言就会静默失效。

## 真机验证结果（2026-09-01，Codex CLI 0.133.0 / gpt-5.5）

隔离 Hub（`MCPANE_HOME=/tmp/annot-verify` + 端口 24994）+ 一个 stdio 下游，
它同时提供两个工具构成对照：`team_handbook` **不申报**注解、`style_guide`
**申报**完整注解。`CODEX_HOME` 也隔离（auth.json 用符号链接指回原位，
凭据不复制）。提问固定为「这个项目的错误处理应该用 anyhow 还是 thiserror？」。

| 步骤 | 预期 | 实测 |
|---|---|---|
| 1 | `tools/list` 里 `team_handbook` 无注解、`style_guide` 有 | ✅ |
| 2 | 声明前 `codex exec` 调用被取消 | ✅ `user cancelled MCP tool call`，下游 0 次调用，模型退回凭记忆答「顶层应用用 anyhow」 |
| 3 | 声明只读后 Catalog 重建，出口带上用户声明 | ✅ `team_handbook` 的 annotations 变为四项 hint |
| 4 | 重跑同一条命令，调用通畅 | ✅ 下游收到 1 次调用、**零取消**，模型改口「都不用，按团队手册统一用 snafu」 |
| 5 | 署名可追溯 | ✅ `tool_describe` 回 `annotationsSource: "user"`；Hub 日志「下游未申报注解，采用用户声明的注解」 |

**对照组同轮验证**：给已申报注解的 `style_guide` 也写了一份**故意相反**的声明
（`readOnlyHint:false, destructiveHint:true`）——出口原样透传下游那份、
`annotationsSource: "downstream"`、日志「下游已申报注解，用户声明未生效」。
约束 1「只补空位不改口径」在真机上成立，不只是单测里成立。

**声明只填了 `readOnlyHint`**（其余取规范默认），与四变体探针的结论一致：
最小必要断言就是这一项，用户不必填满四项。

## 复现方法（真机）

验证"无注解 → 不可用 / 有注解 → 可用"这条因果，需要一个不申报 annotations 的
下游。仓库里现成的就有：`binaries/mcpane-hub/tests/aggregation.rs` 的
`HttpDownstream`（`search`/`confirm`/`slow` 三个工具一个注解都没申报），
与 stdio devserver（每个工具都带完整注解）恰好构成对照。

1. 装一个不申报 annotations 的下游 Server，确认桌面端「工作区」页对应工具行
   标着 `无注解`；
2. 用 `codex exec` 非交互调用该工具，预期失败：`user cancelled MCP tool call`；
3. 在该工具行点「注解」，按工具真实行为勾四个 hint，保存（Catalog 随即重建，
   对**新会话**生效——Session-Pinned，ADR-004）；
4. 重跑同一条 `codex exec`，预期通畅；
5. 复核署名：Hub 日志应有「下游未申报注解，采用用户声明的注解」，
   `mcpane__tool_describe` 该工具应返回 `annotationsSource: "user"`。

对照组：对一个**已申报** annotations 的下游工具做同样的声明，第 5 步应看到
「下游已申报注解，用户声明未生效」且 `annotationsSource: "downstream"`——
声明没生效是设计如此，不是 bug。

## 未覆盖

- 覆盖下游**已申报**的注解：与 ADR-002 Gate 约束 1 直接冲突，不做（理由见 ADR）；
- 批量声明：按项目惯例不替用户一键铺全部，逐个工具确认。
