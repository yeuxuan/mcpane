# 安全策略

## 报告漏洞

请勿公开提交 Issue。将漏洞详情发送至项目维护者（GitHub Security Advisory 优先）。
我们承诺在 72 小时内响应。

## 安全模型要点

- **Hub 仅监听 Loopback**：`127.0.0.1`（可选 `::1`），永不监听 `0.0.0.0`；
- **认证**：每个客户端独立 Bearer Token（≥256 bit，系统安全随机源）；
  数据库只保存哈希与前缀，比较使用常量时间算法；
- **会话**：Bearer Token 与 `MCP-Session-Id` 分离，撤销一个客户端的 Token
  不影响其他客户端；
- **防护**：校验 `Host`；`Origin` 存在时必须校验（防 DNS rebinding）；
- **密钥存储**：macOS Keychain / Windows Credential Manager；SQLite 只存
  Secret Reference；
- **日志脱敏**：Authorization、Token、Cookie 及常见 Secret 字段一律脱敏，
  诊断包导出前二次扫描；
- **配置安全**：客户端配置写入走 快照 → Diff → 原子替换 → 验证 → 可回滚 流程，
  永不静默覆盖。

## 红线（CI 强制）

- 任何 Secret 不得进入 Git 历史、SQLite、普通日志、测试快照或导出文件；
- 完整 Token 只在创建时展示一次。
