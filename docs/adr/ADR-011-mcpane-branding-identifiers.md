# ADR-011：MCPane 品牌、包名、环境变量与持久化标识

- 状态：Accepted
- 日期：2026-07-11

## 背景

品牌与工程标识一旦进入用户配置文件、Keychain、数据目录和自动更新链路，
就成为永久兼容性表面，必须在第一行代码前冻结。

## 决策

采用执行计划 §0.1 的冻结标识表，完整基准见 `docs/branding/NAMING.md`。要点：

- 产品名 `MCPane`；Hub 二进制 `mcpane-hub`；Bridge `mcpane-bridge`；
- Rust crate 前缀 `mcpane-*`；TS 包前缀 `@mcpane/*`；
- 环境变量前缀 `MCPANE_*`；数据目录变量 `MCPANE_HOME`；
- SQLite 文件 `mcpane.db`；Keychain Service `MCPane`；
- 客户端 MCP Entry `mcpane`；内建 Tool 前缀 `mcpane__`；
- 默认上游 `http://127.0.0.1:24860/mcp`；
- Tauri Bundle Identifier `io.mcpane.desktop`（首次签名发布前可调整，
  发布后永久冻结）。

对外文案禁止品牌错误写法，由 `scripts/brand-scan.sh` 在 CI 强制执行；
引用禁用写法的文档行使用 `brand-scan: allow-prohibited-spellings` 标记豁免。

## 本地目录偏差

开发机本地文件夹名为 `MCPanePro`（用户既有目录），仅为本地别名；
仓库规范标识与远端仓库名仍为 `mcpane`，所有工程标识不受影响。

## 后果

- 任何新增可执行文件、配置键、环境变量都必须匹配前缀规则；
- 变更任一冻结标识需要修订本 ADR 并评估迁移成本。
