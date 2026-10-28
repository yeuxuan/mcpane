# 发布手册

单仓库：源码、文档与 Release 附件都在 `yeuxuan/mcpane`。仓库必须保持
public——私有仓库的 Release 附件要认证才能下载，updater 拿不到。

## 发布一个版本（标准流程）

平时的规矩：合入 main 的用户可见变化，随手记进 CHANGELOG 的 `[Unreleased]`
段。条目面向用户书写——它会出现在 Release 描述、应用内更新横幅、仓库
CHANGELOG 三个触点；根因分析、内部模块名、QA 状态写进 commit / PR。

```bash
# 1. 定版：版本号四处 + 两个 Cargo.lock 一并改
bash scripts/bump-version.sh 0.2.2
#    CHANGELOG.md：把 [Unreleased] 的内容挪成 `## [0.2.2] - 日期`，
#    文末补对比链接，[Unreleased] 留空

# 2. 提交推送 main，等 CI 绿
git push origin main

# 3. Windows 专属分支验证（Rust 与前端，不随 push 跑）：
#    dispatch 后等全绿，release-tag 预检会硬卡这一条
gh workflow run platform-ci.yml

# 4. 打 tag（预检全过才真打；tag 不自动触发任何构建）
bash scripts/release-tag.sh 0.2.2

# 5. 出包：两条通道并行，谁先跑都行；**全部上传完之前不要 Publish**
#    （保持草稿或已发版只追加附件）。
#    - macOS 开发机本地：双架构 6 件
bash scripts/release-local.sh 0.2.2
#    - Windows（NSIS 2 件）+ Linux（AppImage 2 件）走 CI
gh workflow run release.yml -f tag=v0.2.2

# 6. 产物齐后（默认期望 darwin-aarch64 / darwin-x86_64 / windows-x86_64 /
#    linux-x86_64）：重建 latest.json → 核对 → Publish → 验证
bash scripts/publish-release.sh 0.2.2
#    若该 tag 已提前 Publish、后来才补齐附件：先
#    bash scripts/rebuild-latest-json.sh v0.2.2
#    再跑 publish-release.sh 做核对与通道验证

CI 成本结构（为什么这么分）：GitHub Actions 按 runner 折算计费，ubuntu 1 倍、
Windows 2 倍、macOS **10 倍**。据此分工：

| 环节 | 跑在哪 | 为什么 |
|---|---|---|
| 日常 push 的 CI | ubuntu | 1 倍计费，覆盖跨平台共享代码（占绝大多数）。实测 ≈21 折算分钟/次，是最大的固定开销 |
| Windows 专属分支验证 | `platform-ci.yml`（只在发版前 dispatch） | 唯一没人日常编过的那一面，实测 ≈56 折算分钟/次（Rust 24.2 分钟跑满两个 workspace）。不设定时巡检：发版频率本就高于巡检周期，cron 是重复消耗 |
| macOS 专属分支验证 | 开发机日常构建 | 每天都在编，再花 10 倍计费重跑一遍是纯重复 |
| 发版出包 · Windows + Linux | `release.yml` | 加起来 ≈78 折算分钟/次（v0.4.0 实测：Windows 30.6 分钟 ×2，Linux 14.5 分钟 ×1，建草稿 1），比在 Windows 机器 + WSL2 上手工出两个平台省事 |
| 发版出包 · macOS 双架构 | 开发机本地 | 签名私钥本就只在那台机器上；双架构实测 19+13 分钟，放进 CI 一轮就是 ≈320 折算分钟 |

⚠️ **已知风险接受项**：`#[cfg(windows)]` 专属分支（`mcpane-config`/
`mcpane-secrets`/`mcpane-hub-downstream`/`apps/desktop/src-tauri` 均有）
不在日常 ubuntu CI 里编译，合入 main 到被验证之间的空窗一直延续到下次发版
预检（`release-tag.sh` 硬卡 `platform-ci.yml` 全绿）才会暴露。这是有意的成本
取舍，不是遗漏——真要收紧这个空窗，触发条件是：平台专属代码改动频率明显
上升，或者发版节奏拉长到让积压变得难修。

⚠️ 由此带来的操作预期：**发版前那次 Platform CI 可能一次红出好几个问题**，
那是自上次发版以来积压的账，不是这次改坏了——逐个修干净再打 tag。

`#[cfg(target_os = "macos")]` 那一支不在此列：开发机就是 macOS，日常
`cargo test` 天天在编它，空窗只有几小时量级。

macOS 为什么留在本地：updater 签名私钥本就只存在开发者手里，本地构建不新增
信任让步；而放进 CI 一轮 ≈320 折算分钟——按当前节奏那是月配额的十分之一到六分之一，
只为买一件本地免费就能做的事。
产物来源由 `release-local.sh` 的前置守卫保证：HEAD 必须正好停在 tag 上且
工作树干净。

Windows 与 Linux 为什么走 CI：这两个 runner 便宜（2 倍 / 1 倍），加起来
≈78 折算分钟/次；更重要的是省掉了本地出这两个平台的一整套环境——Windows
要 VS Build Tools + msvc 工具链，Linux 还得在 WSL2 里再备一份（WebKitGTK
系统依赖、Rust/Node/pnpm/gh 全套、私钥拷贝），而 CI 上只是几行 apt。
`release-local.sh` 的这两个分支保留着，作 Actions 不可用时的兜底路径。

各环节职责：

- **`release-tag.sh`**（预检 → 打 tag → 推送）：工作树干净、在 main 且与
  远端一致、版本号四处一致且等于目标、CHANGELOG 条目与链接就绪且
  `[Unreleased]` 已清空、无演练配置残留、tag 未被占用、HEAD 的 CI 全绿。
  任何一条不过都不打 tag——tag 推出去才在 CI 里炸的话，得修完重推
  再 dispatch 重跑，慢一轮
- **`release-local.sh`**（本地出包半程）：守卫 HEAD 在 tag 上 + 工作树
  干净，草稿按 tag_name 找到就复用、找不到才创建（多台机器与 CI 之间
  幂等，谁先跑都行），按所在平台构建（macOS 双架构 / Windows NSIS /
  Linux AppImage）、校验 `.sig` 非空、按 asset id 覆盖上传。常规路径下
  只用它出 macOS；另两个平台的分支是 Actions 不可用时的兜底。上传失败
  重试用 `SKIP_BUILD=1` 免去重编
- **Release workflow**（仅手动 dispatch，`.github/workflows/release.yml`）：
  出 Windows 与 Linux 两个平台，两段式——`create-release` 在本仓库建
  草稿（含 token 预检与 releaseId 纯数字校验）→ 两个平台的构建 job 按
  `releaseId` 并行上传。tag 推送不自动触发：发版半程分散在多条通道上，
  自动开跑会与手工重跑撞成对同一附件的重复上传
- **`publish-release.sh`**（重建 latest.json → 核对 → Publish → 验证）：
  草稿态先跑 `rebuild-latest-json.sh` 按实际 `.sig` 单写者
  重建 `latest.json`（缺任一平台即红），再核对附件齐全（11 件：
  `latest.json` + macOS 6 + Windows 2 + Linux 2）、
  `latest.json` 的版本 / 平台 / 签名 / notes 逐项校验，确认后
  `draft=false` + `make_latest=true` 一次做对，轮询
  `releases/latest/download/latest.json` 确认老用户真的能收到新版。
  已发布状态下重跑它只做验证，可随时用来体检更新通道

**唯一不可逆的动作是第 6 步的 Publish**：`latest.json` 一旦能从
`releases/latest` 取到，老用户的 updater 立即开始拉新版。在那之前，
草稿怎么删、构建怎么重跑都行。

Release Notes 的切片规则只有一份实现（`scripts/changelog-notes.sh`），
workflow、release-local、rebuild-latest-json、预检全都调它。

## 故障排查

| 症状 | 原因 | 处置 |
|---|---|---|
| 构建 job 全绿但草稿 Release 没附件 | workflow 的 token 权限不够——tauri-action 对非法 releaseId **静默跳过全部上传且 job 照绿**（v0.2.1 事故） | 确认 release.yml 带 `permissions: contents: write`、用 `GITHUB_TOKEN`（或轮换后的 PAT）后 dispatch 重跑。现有三道防线：create-release 的 token 预检、releaseId 纯数字校验、publish-release 的附件齐全核对 |
| `gh release view v0.2.2` 报 not found，网页上草稿明明在 | 草稿的 tag 尚不存在，`gh release view / download` 按 tag 都查不到 | 用列表接口按 `tag_name` 找 id（publish / release-local / rebuild-latest-json 三个脚本都内置此逻辑，附件一律按 asset id 操作） |
| publish-release 红：latest.json 缺平台 | 对应平台产物没传上来或缺 `.sig`——Windows/Linux 看 CI job 日志；macOS 多半是签名变量没注入（变量名是 `TAURI_SIGNING_PRIVATE_KEY`，**没有 `_PATH` 后缀**，写错不报错只是静默不签名，`release-local.sh` 已内置 `.sig` 非空校验） | Windows/Linux：修复后 dispatch 重跑；macOS：重跑 `release-local.sh`。草稿与已传附件会被复用 |
| 发布后老用户收不到更新 | Latest 标记没跟上——`PATCH draft=false` **不会**自动置 Latest，`releases/latest` 仍指旧版 | `publish-release.sh` 已内置 `make_latest=true`；手工补救：`gh api --method PATCH repos/yeuxuan/mcpane/releases/<id> -f make_latest=true` |
| 需要对已有 tag 重跑 Windows / Linux 构建 | 偶发失败 / Actions 计费恢复后 | `gh workflow run release.yml -f tag=v0.2.2`（两个平台一起重跑；同名附件会被 tauri-action 覆盖，幂等） |
| Actions 计费受限（扣款失败 / spending limit 用尽），构建 job 起不来 | GitHub → Settings → Billing & plans 里调限额或修支付方式 | macOS 半程不受影响可先跑；计费恢复后 dispatch 重跑，最后照常 publish。等不起就走下面的兜底路径 |
| Linux job 红在 AppImage 打包（`xdg-open not found` 等） | ubuntu runner 的系统依赖装漏了——Rust 全编完才走到打包，一轮 20 分钟白等 | 补进 release.yml 的「安装 Linux 构建依赖」步骤后重跑 |

排查 gh 命令时的两个坑（都真实咬过人）：`gh api ... \| head -1` 这类管道会
吞掉 gh 的退出码，错误 JSON 会被当成正常输出往下传——先赋值再截取；
交叉编译 msvc 目标在 macOS 上必然失败，别把它的输出接进管道后看退出码。

## Secrets 与密钥

仓库的 Actions Secrets（均已配置）：

- `TAURI_SIGNING_PRIVATE_KEY`——`~/.tauri/mcpane.key` 的**文件内容**（不是路径）。
  私钥无口令，`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` 不用配

Release 附件的上传用 workflow 内置的 `GITHUB_TOKEN`（release.yml 已声明
`permissions: contents: write`），单仓发布不再需要跨仓库 PAT。

- [ ] **备份 `~/.tauri/mcpane.key` 到密码管理器**
  - ⚠️ 私钥目前只存在于这台开发机。**丢失即无法再为任何已安装用户签更新包**，
    他们的自动更新会永久失效，只能重新下载安装。公钥已固化进 tauri.conf.json，
    换密钥对等于抛弃所有老用户的更新通道。

## 自动更新链路（设计决策存档）

- `latest.json` 走**单写者**：构建通道（CI 的 tauri-action、本地 macOS
  脚本）一概不碰它，发布前由 `publish-release.sh` 跑
  `scripts/rebuild-latest-json.sh`，按 Release 里实际存在的 `.sig` 一次算全
  并校验齐全——产物来自两条独立通道、先后不定，收口只能放在发布动作之前
  - ⚠️ 为什么不让 tauri-action 自己写：它对 `latest.json` 做的是**无锁的
    读-改-写**（下载 → 合并本平台 → 删旧 asset → 传新的）。多个写者并行有
    两种坏法：后写覆盖先写，被吃掉的平台从清单里消失且**没有任何报错**；或
    两个写者同删同一 asset id，晚的撞 404 直接把 job 弄挂（tauri-action
    issue #1270，v0.6.1 仍未修）。单写者把竞态从根上消掉
- 草稿 Release「按 tag_name 找到就复用、找不到才创建」，创建逻辑只有
  create-release job 与 `release-local.sh` 两处、串行幂等；两个构建 job 按
  `releaseId` 往里传附件，不参与创建
  - ⚠️ 让构建 job 各自「找不到就创建」会撞出重复草稿、附件被拆散到多个
    Release（tauri-action issue #914）；故构建 job 一律显式传 `releaseId`，
    创建收敛在 create-release 一处
- `bundle.createUpdaterArtifacts` 已开启；更新端点固化在 tauri.conf.json：
  `https://github.com/yeuxuan/mcpane/releases/latest/download/latest.json`
- [ ] 升级/降级保护策略确认（updater 默认只认更高版本）

### 本地演练（端到端验证更新链路）

1. `bash scripts/bump-version.sh 0.1.1`，带签名构建一次，把
   `bundle/macos/MCPane.app.tar.gz` 与 `.sig` 拷到一个空目录当「更新包」：
   ```bash
   TAURI_SIGNING_PRIVATE_KEY="$HOME/.tauri/mcpane.key" \
   TAURI_SIGNING_PRIVATE_KEY_PASSWORD="" \
   pnpm --filter @mcpane/desktop tauri build
   ```
2. 在该目录写 `latest.json`，`signature` 字段填 `.sig` 文件的**全文**，
   `url` 指向本地服务；`python3 -m http.server 8787 --bind 127.0.0.1` 起服务
3. 版本改回 `0.1.0`，endpoint 临时指向 `http://127.0.0.1:8787/latest.json`，
   **并加上 `"dangerousInsecureTransportProtocol": true`**——
   release 构建下 updater 强制 TLS，只改 endpoint 不加这个开关的话，
   插件初始化就 panic，app 直接起不来（dev 模式不强制，所以 dev 下试不出来）
4. 重新构建并运行，观察静态服务的访问日志：
   `GET /latest.json` 说明检查链路通，`GET /*.tar.gz` 说明下载链路通
5. **演练完把 endpoint 和那个开关都改回去**——`scripts/release-guard.sh`
   会拦住残留，CI、release-tag 预检与 release workflow 都跑这道检查

## 兜底路径：Actions 彻底不可用时

Actions 停摆时（典型是计费受限）出包与质量关口一起失效，两件事分开补：

**质量关口**：`platform-ci.yml` 的 Windows 验证起不来，就在出包机上手动
执行等价的 Windows Rust + 前端验收命令（见 CONTRIBUTING），`release-tag.sh`
加 `SKIP_CI_CHECK=1`（其余预检一条都不跳）。判据是 job 有没有真正跑起来——
秒挂且状态 `not started` 才是基础设施故障，跑起来才红的是代码问题，老实修。

**出包**：`release-local.sh` 三个平台的分支都在，Windows 与 Linux 也能在
本机出（Windows 需 VS Build Tools + msvc 工具链；Linux 需 WebKitGTK 系统依赖，
一次性环境准备各做一份）。

真要少发平台（长期修不好又等不起），收尾用
`EXPECTED_PLATFORMS="darwin-aarch64 darwin-x86_64" bash scripts/publish-release.sh <版本>`
缩小清单。代价：该版 `latest.json` 缺其余平台条目，那批老用户收不到这版
更新——能等就等 Actions 恢复后 dispatch 补跑再照常发。

## macOS 签名与公证

macOS 产物由开发机本地的 `scripts/release-local.sh` 构建，Windows/Linux 才走
`.github/workflows/release.yml`。因此 Apple 凭据必须注入运行本地脚本的当前终端，
不能只添加 GitHub Secrets。

一次性准备：

- 安装 Xcode Command Line Tools，确认有 `codesign`、`notarytool` 和 `stapler`；
  本项目的官网 DMG 分发不要求安装完整 Xcode；
- “登录”钥匙串中安装带私钥的 `Developer ID Application`；
- 没装完整 Xcode 时，另外安装 Apple 官方 `Developer ID - G2` 中间证书；
- 创建具备“开发者”权限的 App Store Connect API Key，并把 `.p8` 放在本机
  私有目录，权限设为 `600`；
- Bundle identifier 固定为 `io.mcpane.desktop`，首次正式签名发布后不得改变。

每次发版只在当前终端设置以下变量，不写入仓库、shell profile 或普通日志：

```bash
export APPLE_SIGNING_IDENTITY="Developer ID Application: <法定名称> (<TEAM_ID>)"
export APPLE_API_ISSUER="<Issuer ID>"
export APPLE_API_KEY="<Key ID>"
export APPLE_API_KEY_PATH="/绝对路径/AuthKey_<Key ID>.p8"
```

构建前先运行只读预检：

```bash
bash scripts/release-local.sh --check-apple-distribution
```

正常 macOS 发版仍运行 `bash scripts/release-local.sh <版本>`。脚本会在上传任何附件前，
对 Apple Silicon 和 Intel 两个最终 DMG 分别执行 Developer ID、Hardened Runtime、
安全时间戳、最终 DMG 二次公证、`stapler validate`、`spctl`、内嵌 Mach-O 签名与
架构校验；任一检查失败即停止，不上传附件。Tauri 默认公证内层 App，发布脚本会再把
最终 DMG 提交给 Apple 并装订票据，确保离线 Gatekeeper 也能直接验证安装介质。

Apple `.p8`、`.p12`、密码和 Tauri updater 私钥不得提交仓库或写入普通日志。

另：Bundle identifier `io.mcpane.desktop` 首次签名发布前最终确认，
此后**不得改变**（ADR-011 品牌冻结）。

## Windows

- [x] 交叉编译 sidecar、NSIS 打包、windows-latest CI job 均已就绪
  - ⚠️ **msvc 目标不能在 macOS 上交叉验证**：`aws-lc-sys` / `ring` 的 build
    脚本要 MSVC 的 `windows.h`，`cargo check --target x86_64-pc-windows-msvc`
    必然失败（exit 101）
  - ✅ **gnu 目标可以**（`brew install mingw-w64`），两个 workspace 都能跑：
    ```bash
    CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
      cargo clippy --workspace --all-targets --all-features \
      --target x86_64-pc-windows-gnu -- -D warnings
    ```
    src-tauri 需先放占位 sidecar 让 build.rs 通过（`binaries/` 已 gitignore，
    但**别用 `: >` 截断已存在的真实产物**）。只覆盖编译期，`target_env` 在
    msvc/gnu 下有分叉（如 WebView2 loader 链接方式），
    **不能替代 windows-latest 原生 job**
- [ ] **0.2.1 已发布的 Windows 修复批次尚未真机回归验证**（行尾/BOM、MSIX
      定位、管道回执、凭据超限、黑窗、进程树、DACL 收紧等），随下一次
      真机冒烟一并过
- [ ] Windows 真机冒烟清单：Named Pipe 控制面（含 DACL 收紧后仍可连）、
      envsync（HKCU\Environment + 广播）、凭据读写（含超 2560 字节的 DPAPI
      降级）、配置行尾/BOM 保真、stdio 子进程无黑窗且退出时进程树杀净、
      Claude Desktop MSIX 路径命中、WebView2 引导安装、updater 自更新一轮
- [ ] `mcpane://` 深链真机验证（ADR-023，dev 模式无法验证）：打包后的应用里用
      `open "mcpane://import/provider?app=claude&name=t&baseUrl=https://x.example.com"`
      （Windows：`start`；Linux：`xdg-open`）唤起 → 出现确认框、主机名与 Key 掩码正确 →
      导入后供应商页多一条且**未被切换**；应用已在运行与冷启动两种情形都要试；
      Windows/Linux 还要确认二次启动的链接被单实例转发而不是另开窗口
- [ ] Hub 方言规范化真机回归：发版包里的 Hub 接入 Firecrawl 后，经网关调一次
      `firecrawl_scrape` 成功（此前报 `unsupported dialect "draft-07"`）
- [ ] 远程 MCP 的 OAuth：发版前先跑 `scripts/e2e-oauth-hub.py`（真实 Hub 二进制 + 假授权服务器，26 项，
      构建方式见脚本头注释），全过再做下面的真机验证
- [ ] 远程 MCP 的 OAuth 真机验证（ADR-024，需要账号与浏览器）：用真实的 Notion
      （`https://mcp.notion.com/mcp`）或 Linear 添加远程服务器 → 卡片「需要登录」→ 点登录 → 浏览器授权 →
      回到应用状态为运行中且工具出现；重启应用与 Hub 后仍是已登录；行尾菜单「退出登录」后回到「需要登录」；
      `$MCPANE_HOME/mcp-oauth/` 下令牌文件权限仅属主。**Windows 上完整再走一遍**（回环回调、文件权限）
- [ ] 按地址查找（ADR-025，需联网）：在桌面应用里添加服务器 → 官方预设页顶部粘贴
      `https://github.com/upstash/context7`，约十几秒内出现「地址一致」的两张卡（远程 + npx）→「使用」后表单被填好
      且**没有被保存**；再试 `https://mcp.notion.com/mcp` 与一个 Registry 里没有的仓库（应提示没有，不给命令）。
      **开着 Clash 等代理**与断网各试一次：前者能查到，后者给出「无法访问官方 MCP Registry」并指向代理设置。
      Windows 上再走一遍。`cargo test -p mcpane-registry -- --ignored` 是同一件事的命令行版
- [ ] MCP 服务器导出与导入（ADR-027）：打包后的应用里，服务器页「导出」→「保存为文件」→ 「下载」里出现
      `mcpane-servers.json` 且访达定位到它，再存一次得到 `mcpane-servers (2).json`；打开文件确认 Token 类的值都是
      `${…}` 占位符。「导入」→「从文件或 JSON 导入」→「选择文件…」弹出系统文件面板，选刚才的文件 → 已存在的
      标「已存在，跳过」；在另一台机器（或删掉其中一个后）导入，补填占位符后服务器能连上，不填的以停用状态出现。
      Windows 上再走一遍（资源管理器定位、文件面板）
- [ ] 按模型设置价格（ADR-026）：桌面应用里打开用量页 → 给一个「无定价」的模型点「设置价格」填价 →
      成本不再是 `-` 且出现「自定义」，总览的估算成本同步变化；按倍率套用一次；「恢复默认」后回到无定价；
      本机备份恢复后覆盖仍在。Windows 上再走一遍
- [ ] 超限归一在真实客户端上生效（ADR-022 §10）：让一家措辞非标准的中转站（或本地假上游）返回
      「对话过长」类 400，确认 Claude Code 与 Codex 触发的是**自动压缩**，而不是停在原错误上
- [ ] 流首事件转移手测一次：本地假上游先回 200 再在流里报过载，Claude Code 的回答应由备用供应商给出
- [ ] Windows 覆盖安装/更新专项（0.3.2 咬过：安装到一半报
      `Error opening file for writing: ...\mcpane-hub.exe`）：
      应用内更新一轮不中断；hub 运行中双击 setup.exe 覆盖安装不中断；
      装完 hub 能重新拉起；卸载后无残留 mcpane-hub.exe 进程与 `.old` 文件
- [ ] 代码签名证书：2023-06 起私钥强制存硬件，CI 只能走云签名
  - **先确认资格**：Azure Artifact Signing（原 Trusted Signing，$9.99/月）
    有**地域限制**——组织限美国 / 加拿大 / 欧盟 / 英国，**个人开发者只限
    美国和加拿大**。不在范围内就用不了，别按它做预算
  - 用不了就只能买 OV 证书（约 $200–500/年，私钥须存硬件 token 或云 HSM，
    CI 里用云 HSM 方案）
  - 两者都**不给**即时 SmartScreen 信任，新签名要靠真实下载量攒声誉

## 发布产物（待办）

- [ ] SBOM（cargo auditable / cyclonedx 任选）
- [ ] 第三方依赖清单
