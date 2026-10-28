# GATE-2：配置安全验收报告

- 日期：2026-07-11
- 结论：**通过**（全部条件由自动化测试背书，74/74 workspace 测试全绿）

## 交付物

| Crate | 职责 |
|---|---|
| `mcpane-config` | 配置安全核心：ConfigDocument（JSON/TOML）、内容哈希、PatchPlan、结构化+unified Diff、Snapshot（支持脱敏钩子）、临时文件+fsync+原子 rename、写后重读验证、失败自动恢复、JSONL 操作历史（Phase 4 迁 SQLite）、notify 外部修改监听、冲突错误类型 |
| `mcpane-adapter-claude` | 检测（`claude --version`）、配置发现（`~/.claude.json` / `~/.claude/settings.json` / `.mcp.json`）、MCP scope（User/Project/Local）识别、Hub Entry Patch（`${MCPANE_CLAUDE_TOKEN}` 环境变量展开）、Provider env Patch、删除保留、业务验证（拒明文 Token / 拒非 loopback） |
| `mcpane-adapter-codex` | 检测（`codex --version`）、配置发现（`CODEX_HOME` 语义 + 项目级）、`[mcp_servers.mcpane]` 键级最小 Patch（表内用户自定义键保留）、`bearer_token_env_var`、验证拒绝 `http_headers` 明文 Bearer |

> **勘误（2026-07-19）**：本报告初版列了一个独立的 `mcpane-domain` crate（Target 模型
> MacNative / WindowsNative / WslDistro）。该独立 crate **并未落地**——P0 最终 crate 布局
> 收敛为 10 个（见 `docs/architecture/DEPENDENCIES.md`），平台/路径相关处理内联在各
> Adapter 中，没有单独的 Target 领域 crate。上表已据实删去该行。

## GATE-2 检查表

| 条件 | 验证方式 |
|---|---|
| 未知 JSON 字段保留率 100% | e2e：真实风格 `.claude.json` fixture 接入后，除新增 Entry 外与原文件**语义全等**（serde 值级比较）；解绑后完全恢复 |
| Codex TOML 注释和未知表不丢失 | e2e：注释（顶部/行内/表头）、未知表、`model_providers` 逐字保留；解绑后与原文件逐字一致 |
| 每次写入都有 Diff 和 Snapshot | 引擎强制：apply 必产出结构化 Diff + unified diff + 写前快照（含元信息落盘） |
| 外部修改后不会静默覆盖 | load 与 apply 之间的哈希对比：外部编辑 → `ConfigError::Conflict`，文件原样保留，两个 Adapter 均有用例 |
| 模拟失败后原配置完整 | 失败注入：rename 失败（含临时文件清理断言）、磁盘满、只读目录——原内容全部完整，恢复记录入历史 |
| 任意历史版本可回滚 | v1→v2→v3 后回滚至 v1；回滚前自动再备份，可"回滚回滚"回到 v3；历史 4 条完整 |

## 设计要点与已知边界

- **快照脱敏与回滚的矛盾处理**：`redact` 钩子只允许剔除 MCPane 自己管理、
  可从 Secret Store 重新物化的字段（引擎文档注明）；用户自有字段原样保留。
- **TOML 写入为键级 Patch**：`[mcp_servers.mcpane]` 内用户手工加的键与注释
  在重复接入时保留（有测试）。
- **操作历史暂为 JSONL**：`config_operations` SQLite 表在 Phase 4（TASK-400+）
  接管，记录结构已按表结构对齐。
- **Adapter 的 Target 绑定**：`TargetPath` 已就绪，Provider Service（Phase 3）
  统一在服务层持有 Target 并绑定路径；P0 仅 MacNative/WindowsNative。
- **Windows 实机验证**：条件编译代码可在 CI Windows 矩阵编译；本机（macOS）
  无法执行 Windows 行为测试，标记**未验证**，依赖 CI。
