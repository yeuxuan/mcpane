# ADR-012：Claude/Codex Bearer Token 凭据传递与明文落盘禁止策略

- 状态：Superseded by [ADR-015](ADR-015-credential-delivery-dual-mode.md)
- 日期：2026-07-11
- 备注：「默认禁止明文落盘」经威胁模型复盘被推翻——对本地同权限进程无增量
  收益，却使首次接入必坏。ADR-015 改为 EnvVar 默认 + Inline 直写可选。

## 背景

Hub 的 Bearer Token 需要传递给 Claude Code 与 Codex。最省事的方式是把 Token
明文写进 `~/.claude.json` / `config.toml`，但这会让凭据进入普通文件、备份与
同步链路，违反安全红线。

## 决策

1. **默认禁止**把完整 Bearer Token 写入 `~/.claude.json`、`.mcp.json` 或
   `config.toml`。
2. 环境变量约定：
   - Claude Code：`MCPANE_CLAUDE_TOKEN`，配置内使用
     `"Authorization": "Bearer ${MCPANE_CLAUDE_TOKEN}"` 环境变量展开；
     版本支持 `headersHelper` 时提供从系统凭据库动态取 Header 的增强模式；
   - Codex：`bearer_token_env_var = "MCPANE_CODEX_TOKEN"`；
     不把 Token 写入 `http_headers`。
3. 互操作 PoC（Phase 1）使用临时环境变量注入测试 Token，不写入客户端配置。
4. UI 必须展示环境变量配置状态，提供各 shell 的设置/清除命令、重新检测与
   Token 轮换。
5. 用户明确选择静态 Header 明文落盘时，必须显示风险并二次确认，操作记入历史。
6. P1 评估本地 OAuth，把凭据交由客户端自身的安全凭据机制管理。

## Hub 侧配套

- Hub 进程只保存 Token 哈希与前缀；完整 Token 仅创建时展示一次（ADR-003）；
- 日志、诊断包对 Authorization 头一律脱敏。

## 后果

- 一键接入流程（TASK-600/601）多一步"环境变量可见性检测"；
- 需要按 Claude Code / Codex 官方文档在执行时复核字段名（DoD 附来源链接）。
