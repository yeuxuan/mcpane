# 贡献指南

## 开发环境

- Rust 1.97.0 stable（由 `rust-toolchain.toml` 固定）
- Node.js >= 24
- pnpm 11.11.0（由根 `package.json` 的 `packageManager` 固定，建议启用 corepack）

## 仓库结构

```text
apps/desktop/     Tauri 2 + React 桌面应用（前端 + src-tauri 独立 workspace）
binaries/         mcpane-hub（聚合 Hub，随桌面应用以 sidecar 分发）
crates/           mcpane-* Rust 库（测试在各 crate 的 tests/ 内）
fixtures/         测试夹具（claude/codex 配置、损坏配置）
docs/             执行计划、ADR、架构、互操作报告
```

## 编译产物维护

本仓库有两个独立 Cargo workspace（根 + `apps/desktop/src-tauri`），共同依赖各编译一份，
而 cargo 从不自动回收旧产物。跑过 Windows 交叉编译或几轮发版之后 `target/` 很容易堆到
十几 GB，建议每隔几周跑一次：

```bash
pnpm clean:target          # 清 7 天前的陈旧产物，保留增量缓存
pnpm clean:target --deep   # 连增量缓存一起清，回收最多，下次编译慢一轮
```

回收的是陈旧交叉编译产物、旧版发版暂存和 cargo-sweep 认定的过期产物；
`<triple>/release/bundle` 里的安装包、宿主平台产物不受影响。

## 提交前检查

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
pnpm typecheck
pnpm lint
pnpm test
pnpm build
pnpm e2e   # CI 有独立 e2e job，本地跑过才能复现 CI 结果
pnpm brand-scan
```

依赖漏洞扫描由 CI 的「依赖漏洞扫描」job 负责，日常提交不必本地跑。它红了要复现的话：

```bash
cargo install cargo-audit --locked   # 首次
cargo audit                          # 根 workspace
(cd apps/desktop/src-tauri && cargo audit)
pnpm audit --prod --audit-level high  # --prod：devDependencies 的告警不进分发包，不卡 CI
```

## 硬性规则

- 遵守 `AGENTS.md` 全部规则（品牌标识、依赖策略、密钥红线）；
- 禁止提交 `.env`、Token、用户本地配置和 `mcpane.db`（`.gitignore` 已覆盖，
  同时受 CI 扫描）；
- 架构级变更必须新增或修订 `docs/adr/` 中的 ADR；
- 依赖版本变更必须同步 `docs/architecture/DEPENDENCIES.md` 与 lockfiles。

## 分支与提交

- 主分支 `main` 保持可构建；
- 提交信息使用祈使句，正文说明动机与验证方式；
- 功能改动建议先开 Issue 对齐方向，再从 fork 或特性分支提交 PR；
- 大的功能与架构级改动对应 `docs/MCPane_EXECUTION_PLAN.md` 中的一个 Task，
  PR 描述里附验收证据（测试输出 / 截图 / 日志）。

## 许可证

提交即表示你同意以 [MIT](LICENSE) 许可证授权你的贡献。

## 行为准则

保持专业与友善：讨论对事不对人，反馈给出依据与建议。
