# ADR-016：envsync——MCPane 代管环境变量持久化，env 模式一键化

- 状态：Accepted（修订 ADR-015 §1/§3 的 env 模式语义）
- 日期：2026-07-12

## 背景

ADR-015 确立凭据双模式后，Inline 模式已零手动步骤，但 EnvVar 默认模式仍要求
用户手动发 Token、改 shell 配置、重启——与产品理念「接入一键完成，不给用户留
下一步」直接冲突。用户拍板：**两种模式都必须一键完成**。

障碍在于 Token 明文的生命周期（ADR-003）：Hub 持久层只存哈希+前缀，明文只在
create/rotate 响应出现一次。要自动化 env 模式，MCPane 必须在那一刻把明文持久
化到客户端进程可见的位置，并在轮换时可重写。

## 决策

新增桌面侧模块 `envsync`（apps/desktop/src-tauri/src/envsync/），接管
`MCPANE_*_TOKEN` 的全生命周期。env 模式 apply 流程变为：
`ensure_connection_token`（复用 Inline 的发放逻辑）→ `persist_env_var` →
写客户端配置（仅变量引用）。顺序不变式：**配置引用了变量 ⇒ 变量已持久化**。

### 持久化方案（按平台）

| 平台 | 凭据存储 | CLI 覆盖 | GUI 覆盖 |
|---|---|---|---|
| macOS | `$MCPANE_HOME/env.sh`（0600，原子写） | shell rc 注入一行带 `# mcpane:env` 标记的 source | `launchctl setenv` 即时 + LaunchAgent 登录重放 + App 启动 resync 兜底 |
| Linux | 同上 | 同上 | `~/.config/environment.d/50-mcpane.conf`（注销重登生效，不撒谎） |
| Windows | `HKCU\Environment` 直写 | 注册表即全覆盖 | 同左 + `WM_SETTINGCHANGE` 广播 |

关键取舍：

1. **rc 注入而非直写 Token 进 rc**：rc 只得到一行稳定的 source（幂等标记，
   路径变更整行替换），明文集中在 env.sh（0600、不随 dotfiles 同步）——这反而
   解决了 ADR-015 自己指出的「dotfiles 恰恰是被同步最多的文件」问题。目标 rc
   按 `$SHELL` 分派：zsh→`.zshrc`；bash→`.bashrc`（macOS 额外 `.bash_profile`，
   login shell 不读 bashrc）；fish→独立 `conf.d/mcpane.fish`（`set -gx` 语法，
   POSIX `export` 在 fish 报错）；其它→`.profile`。
2. **LaunchAgent 要装**（macOS）：不装则重启后 Dock 启动的 Cursor/VS Code 拿不
   到变量 → 401 且无从排查；「登录项多一项」的观感成本远小于此。plist 只写文件
   不主动 bootstrap（当前会话已被 setenv 覆盖，登录时 launchd 自动加载）。
3. **弃 setx 改 HKCU 直写**（Windows）：setx 弹窗闪烁、无删除能力（清理做不了）、
   1024 截断。unsafe 面收敛在广播调用单文件内。
4. **失败分级**：凭据存储写入失败 = 致命（不写客户端配置，杜绝半截接入）；
   rc 注入 / 会话同步 / LaunchAgent 失败 = 降级为警告并附手动兜底指引。
5. **轮换联动扩展**（修订 ADR-015 §3）：`connection_rotate_sync` 对 env 形态
   （有 Hub 条目且非 Inline）调 `persist_env_var` 重写；一次性明文展示只剩
   「完全未接入」一种情形。所有 env 形态一律接管，不区分"代管/手动"——用户在
   我们的 source 行之后手写的 export 仍然生效（后者赢），自主权不受损。
6. **清理不变式**：disconnect 即刻 `remove_env_var`；最后一个受管变量移除时
   全量清理（env.sh / rc 标记行 / fish 文件 / environment.d / LaunchAgent），
   系统回到接入前状态，零残留。rc 注入行带 `[ -f ... ]` 守卫，清理失败的残留
   行无害空转。
7. **Cline 例外**：不支持环境变量展开，前端固定直写模式（不再展示模式选择），
   后端对 cline+env 请求显式报错。

## 后果

- ADR-015 §1「用户自行注入」、§3「EnvVar 形态维持一次性展示」语义由本 ADR 取代；
  ADR-012 的手动 `export`/`setx` 指引降级为「使用说明」末尾的备用节。
- env/inline 的产品区别收敛为一句话：**凭据保存的位置**（MCPane 数据目录 vs
  客户端配置文件）。两者都一键、都以「重启客户端」为唯一残留动作。
- 明文落点新增 env.sh（0600）：本地同权限进程威胁模型下与明文写配置等同
  （ADR-015 威胁模型分析仍成立），浏览器侧攻击仍由 Bearer 校验拦截。
- `mcpane_home()` Linux 分支修为绝对路径（`$HOME/.mcpane`）——rc 注入依赖绝对
  路径，相对路径会产出坏行。
- Hub 侧不变（ADR-003 继续有效）。
