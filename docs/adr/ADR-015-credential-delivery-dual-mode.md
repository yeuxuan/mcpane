# ADR-015：客户端凭据双模式投递（EnvVar 默认 / Inline 直写可选），取代 ADR-012

- 状态：Accepted（取代 ADR-012；§1 EnvVar 手动注入与 §3 的 EnvVar 一次性展示
  语义已由 [ADR-016](ADR-016-envsync-managed-env-persistence.md) 修订——env
  模式同样一键完成，Token 由 envsync 代管持久化）
- 日期：2026-07-12

## 背景

ADR-012 规定 Bearer Token 一律不落客户端配置文件，只写环境变量引用。实际使用中
暴露两个问题：

1. **首次接入必坏**：接入后用户还要手动发 Token、改 shell 配置、重启客户端，
   漏任何一步 Codex 直接报
   `Environment variable MCPANE_CODEX_TOKEN for MCP server 'mcpane' is not set`，
   且因 `required = true` 整个会话不可用，错误无法归因到 MCPane。
2. **安全收益与代价不匹配**：ADR-012 推荐把 `export MCPANE_*_TOKEN` 写进
   `~/.zshrc`——dotfiles 恰恰是被 git 同步最多的文件，落盘面并未消失，只是换了
   一个更常被同步的文件。

## 威胁模型分析

| 威胁 | env 引用 | 明文写配置 | 结论 |
|---|---|---|---|
| 同用户本地恶意进程 | 可读 shell rc / Hub SQLite | 可读配置文件 | 两者等同，均不防 |
| 本机其他用户 | 文件权限解决 | 文件权限解决 | 与放哪无关 |
| 浏览器 DNS rebinding / CSRF 打 localhost:24860 | Bearer 校验拦截 | Bearer 校验拦截（浏览器读不到本地文件） | Token 机制本身防住，与投递形态无关 |

即：**Token 必须保留（防浏览器侧攻击），但"禁止明文落盘"对本地威胁没有增量
收益**。业界同类（Codex 自身 `auth.json`、Claude Code `.claude.json`、cc-switch）
均明文落盘。

## 决策

1. **双模式投递**，接入预览弹窗内切换，全部 6 个客户端（Claude Code / Codex /
   Cursor / VS Code / Cline / Devin）一致：
   - **EnvVar（默认）**：配置只写环境变量引用（Codex `bearer_token_env_var`，
     其余 `${ENV}` / `${env:ENV}` 展开）。Token 不落任何客户端配置文件，适合
     配置会被同步/共享的用户。（ADR-016 修订：变量由 envsync 自动持久化，
     用户不再手动注入；Cline 不支持展开，固定走 Inline。）
   - **Inline（可选，「直接写入 Token」）**：apply 时经 Hub 控制面自动
     轮换/新建该客户端的连接，拿到明文 Token 直写 `Authorization: Bearer <token>`
     （Codex 为 `http_headers.Authorization`）。零手动步骤，写完重启即用。
     两种形态互斥写入，杜绝双源歧义。
2. **`required = false`**（Codex）：Hub 停机或凭据未就绪时客户端只是缺 mcpane
   工具，不再阻塞整个会话——这使 EnvVar 默认模式下"变量未设置"从致命错误降级
   为可恢复状态。
3. **轮换联动**（`connection_rotate_sync`）：凭证页轮换 Token 时，若该客户端
   配置当前是 Inline 形态则就地改写（新明文只在 rotate 响应出现的那一刻写入）。
   （ADR-016 修订：EnvVar 形态由 envsync 重写环境变量存储；仅完全未接入才
   维持一次性展示。）
4. **Inline 接入的完整性不变式**：Hub 不可达时 Inline apply 直接失败，不写
   半截配置；EnvVar apply 不依赖 Hub 在线。
5. **脱敏边界**：apply 返回的 diff 中 Token 替换为前 8 位 + `…`；快照保真不脱敏
   （脱敏会让回滚丢值；被轮换的旧 Token 已失效，无泄露价值）。回滚 UI 对触及
   mcpane 条目的记录提示"回滚不会恢复已轮换的 Token"。
6. **Hub 侧不变**（ADR-003 继续有效）：持久层只存哈希 + 前缀，明文只在
   create/rotate 响应出现一次，常量时间比较，日志脱敏。

## 后果

- ADR-012 标记 Superseded；`validate_hub_entry` 系列从"拒绝明文"改为按模式校验
  （Inline 要求可用 Bearer、EnvVar 要求变量引用，互斥检查）。
- 首次接入报错的自愈路径统一为"重新接入"（Inline 下= 轮换 + 改写，一步修复）。
- 「使用说明」以 EnvVar 默认路径叙事，Inline 作为免环境变量的替代项提示。
