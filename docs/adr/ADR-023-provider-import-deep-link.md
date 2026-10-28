# ADR-023：`mcpane://` 深链导入供应商

- 状态：Accepted
- 日期：2026-09-30
- 关联：ADR-008（竞品仅作参考）、ADR-012（凭据边界）、ADR-022（本地路由）

## 背景

中转站官网希望放一个「导入到 MCPane」按钮，用户点一下就把站点地址（和 Key）带进来，
省去手抄。竞品（magpie、cc-switch）都有类似的自定义协议导入。已有基础：单实例插件、
`import_relays`（Key 转存系统凭据库、同地址去重）、供应商页的确认交互。

## 决策

### 1. 链接格式

```text
mcpane://import/provider?app=claude&name=…&baseUrl=https://…&apiKey=…&model=…
```

- `app`：`claude` / `codex` / `gemini` / `grok`。additive 型客户端与 Claude Desktop 需要更多结构化字段，
  一个 `名称 + 地址 + Key + 模型` 的链接表达不了，暂不接。
- `app`、`name`、`baseUrl` 必填；`apiKey`、`model` 可选；未知参数忽略；重复参数取第一个。
- 解析与校验放进 `mcpane-provider::import_link`（纯函数，不碰 IO）：名称 ≤64、地址 ≤2048、Key ≤512 且无空白、
  模型 ≤128，全部拒绝控制字符；`baseUrl` 必须 https（本机回环允许 http，给本地模型服务用），
  必须有主机名，**不得内嵌用户名/密码**。`ImportDraft` 的 `Debug` 不输出 Key。

### 2. 链接是不可信输入：只产出草稿，必须经用户确认

收到链接 → 解析成草稿暂存**后端内存**（只留最新一条）→ 通知前端 → 前端弹确认框，
**突出目标主机名**、Key 只显示掩码（末 4 位）→ 用户点「导入」才落库。

- 落库复用 `import_relays`：Key 进系统凭据库，同 app 同地址已存在则跳过并如实告知。
- 导入**不切换、不开路由**，由用户自己在供应商页启用——一条恶意链接最多造出一条不生效的供应商。
- 链接没带 Key 时，确认框允许补填；也可导入后再补。
- Key 不经事件载荷与 IPC 返回值往返（预览里只有掩码）；整条 URL 不进日志，错误只记种类。
- 事件（`import-link-request` / `import-link-error`）不带草稿内容；冷启动时事件可能早于前端挂载，
  所以前端挂载时也主动取一次草稿（`import_link_peek`）。

### 3. 投递机制

用官方 `tauri-plugin-deep-link`（2.6）：`tauri.conf.json` 声明 scheme `mcpane`，安装包负责系统登记。
macOS 由系统事件送达；Windows/Linux 由二次启动的 argv 经单实例插件（启用其 `deep-link` feature）
转发进同一个回调。Linux AppImage 与 Windows 未安装的开发构建没有安装器，启动时 `register_all()` 补登记。
只用 Rust 侧 API，不引入 JS 插件，故无需新增 capability。

### 不做

- 自动导入 / 自动切换 / 自动开启路由；
- 链接里携带请求头、请求体覆盖、模型档位等高级字段（攻击面大，需要时再逐项评估）；
- 通用 Web 入口页 / 短链服务。

## 验证

- 解析器：`cargo test -p mcpane-provider import_link`（格式、解码、白名单、https/回环、内嵌凭据、长度与控制字符、Key 不入 Debug）。
- 预览不含 Key：`cargo test -p mcpane import_link`（桌面壳）。
- 确认框：`ImportLinkDialog.test.tsx`（冷启动取草稿、事件触发、补填 Key、取消丢弃、同地址跳过、失败可重试、无效链接）。
- **未验证**：系统级唤起（浏览器点链接 → 操作系统 → 应用）。macOS 只有打包后的 `.app` 才会登记 scheme，
  dev 模式无法端到端验证，需一次真机打包后执行 `open "mcpane://import/provider?…"`。
