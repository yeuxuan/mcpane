# ADR-010：TypeScript 7 与 Compiler API 兼容策略

- 状态：Accepted
- 日期：2026-07-11

## 背景

TypeScript 7（原生化编译器）移除/变更了部分旧配置与 Compiler API，
部分生态工具（依赖旧 API 的 lint/codegen 插件）尚未跟进。

## 决策

1. 类型检查统一使用 **TypeScript 7.0.2** 的 `tsc --noEmit`。
2. Lint/格式化默认使用 **Biome 2.5.3**（不依赖 TS Compiler API），
   不把依赖旧 Compiler API 的工具设为启动阻塞项。
3. 若某必需工具仍依赖旧 Compiler API，按 TS 官方兼容方案隔离使用
   （独立 devDependency + 独立脚本），不得污染主类型检查链路。
4. 不得为兼容插件而关闭 `strict`、`noUncheckedIndexedAccess` 等核心检查。
5. 版本偏离基线时记录 `docs/architecture/DEPENDENCIES.md`。

## 已验证的 TS7 迁移点（2026-07-11）

- `baseUrl` 已移除（TS5102）：`paths` 必须使用相对前缀（`"./src/*"`），
  本仓库 tsconfig 已按此调整；
- React 19 + TanStack Router/Query/Table + Zod 4 + RHF 7 + Zustand 5 +
  i18next 26 在 TS 7.0.2 下类型检查通过（见 `apps/desktop/src/compat/`）。
