//! 快捷配置：把 MCP Hub 写入更多 AI 客户端（Claude Desktop / Cursor / Cline /
//! VS Code / Devin / Gemini CLI / Antigravity / Trae / Qoder / Qwen Code /
//! CodeBuddy / ZCode）。
//!
//! 与 Claude Code / Codex 的专用 adapter 不同，这几家客户端都是 GUI 编辑器/扩展，
//! 没有可解析版本的 CLI，故：
//! - 检测：GUI 类按独占的配置目录是否存在判断「已安装」，CLI 类按 PATH 里的
//!   可执行（见 [`InstallEvidence`]）；
//! - 写入：复用 `mcpane-config` 的快照/原子写/校验引擎，但每家的 JSON 形态不同
//!   （顶层 key、type 写法、URL 字段名、是否支持 `${env:}` 展开），由本表按客户端定制。
//!
//! 条目形态分两类（[`EntryKind`]）：
//! - Http：远程条目（url + Authorization 头），大多数编辑器类客户端；
//! - StdioBridge：stdio 桥条目（Claude Desktop 这类只认 stdio 的客户端）——
//!   `command` 指向随应用分发的 mcpane-hub 二进制，`args: ["bridge"]`，
//!   Token 与 Hub 端点经 `env` 块注入（`MCPANE_TOKEN` / `MCPANE_HUB_URL`）。
//!
//! 配置形态核对日期 2026-07-11（Claude Desktop 2026-07-19，国内五家 2026-08-29），
//! 来源见每家常量注释。

use std::path::{Path, PathBuf};

use mcpane_config::{
    ConfigDocument, ConfigFormat, CredentialDelivery, CredentialMode, PatchOp, PatchPlan,
    is_usable_inline_bearer,
};
use serde_json::{Value, json};

/// Hub 默认数据面入口（唯一事实源在 mcpane-hub-protocol）。
const DEFAULT_HUB_URL: &str = mcpane_hub_protocol::HUB_MCP_URL;
/// MCPane 管理的 Server 条目名（跨客户端统一，便于识别/解绑）。
const ENTRY_NAME: &str = "mcpane";

/// 条目形态：远程 HTTP vs stdio 桥。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    /// 远程条目：`url` + `headers.Authorization`。
    Http,
    /// stdio 桥条目：`command`（mcpane-hub 二进制）+ `args: ["bridge"]` +
    /// `env: { MCPANE_TOKEN, MCPANE_HUB_URL }`。凭据只能直写 env 块
    /// （宿主客户端不展开 `${env:}`），故此类 spec 的 `token_env` 恒为 None。
    StdioBridge,
}

/// EnvVar 模式下凭据引用的展开语法。宿主各写各的，写错就是一串没人认的字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnvSyntax {
    /// `${env:VAR}`——VS Code 系编辑器（Cursor / VS Code / Devin）。
    VsCode,
    /// `${VAR}`——dotenv 风格，CLI 系与 Qoder / CodeBuddy。
    Dotenv,
}

/// 「装没装」的判据。写成类型而不是一句「目录在就算装」——`~/.gemini` 同时是
/// Gemini CLI 与 Antigravity 的家，对前者目录存在什么也证明不了。
#[derive(Debug, Clone, Copy)]
enum InstallEvidence {
    /// 独占的配置 / 支持目录存在即算装（GUI 编辑器类，目录只有它自己会建）。
    Dir,
    /// 目录或 PATH 里的可执行，任一命中即算装（同时有 GUI 与 CLI 形态）。
    DirOrCli(&'static str),
    /// 走供应商侧的同一套判定（PATH 里的可执行，或**归属明确的痕迹文件**）。
    /// 给配置目录与别家共用的客户端用：`~/.gemini` 同时是 Gemini CLI 与
    /// Antigravity 的家，只有 `oauth_creds.json` / `.env` 这类文件分得清是谁。
    /// 复用而不是另造一套，两页答案才不会打架（本地环境面板与供应商页用的就是它）。
    ProviderApp(mcpane_provider::ClientApp),
}

/// 单个快捷配置客户端的规格。
#[derive(Debug, Clone, Copy)]
pub struct McpClientSpec {
    /// 稳定 id（前端命令入参）。
    pub id: &'static str,
    /// 展示名。
    pub display: &'static str,
    /// 条目形态。
    entry_kind: EntryKind,
    /// Server 表在文档里的键路径：多数是顶层 `["mcpServers"]`，VS Code 是
    /// `["servers"]`，ZCode 挂在嵌套的 `["mcp", "servers"]` 下。
    prefix_path: &'static [&'static str],
    /// 远程条目的 `type` 值；None 表示不写 type（Cursor/Devin 自动探测）。
    entry_type: Option<&'static str>,
    /// URL 字段名：`url` 或 `serverUrl`（Devin）。
    url_field: &'static str,
    /// Token 环境变量名；None 表示该客户端的配置不做变量展开
    /// （Cline / Trae / ZCode 只能明文占位；StdioBridge 类恒为 None）。
    pub token_env: Option<&'static str>,
    /// `token_env` 的展开语法；`token_env` 为 None 时不参与写入。
    env_syntax: EnvSyntax,
    /// 从终端启动的 CLI：env 模式下重启须在新终端里（旧 shell 读不到新 Token）。
    pub terminal: bool,
    /// 「装没装」怎么判（见 [`InstallEvidence`]）。
    install: InstallEvidence,
}

/// 不支持变量展开的客户端（Cline / Trae / ZCode）在 EnvVar 默认模式下写入的占位串，
/// 只能手填。Inline 可选模式下直接写真实 Token，无需占位（ADR-015）。
const CLINE_TOKEN_PLACEHOLDER: &str = "<在卡片「凭证」里发放的 Token>";

/// 全部快捷配置客户端（顺序即 UI 展示顺序）。
pub const CLIENTS: &[McpClientSpec] = &[
    // Claude Desktop 只支持 stdio 本地 server，经 `mcpane-hub bridge` 接入。
    // https://modelcontextprotocol.io/docs/develop/connect-local-servers
    McpClientSpec {
        id: "claude-desktop",
        display: "Claude Desktop",
        entry_kind: EntryKind::StdioBridge,
        prefix_path: &["mcpServers"],
        entry_type: None,
        url_field: "url",
        token_env: None,
        env_syntax: EnvSyntax::VsCode,
        terminal: false,
        install: InstallEvidence::Dir,
    },
    McpClientSpec {
        id: "cursor",
        display: "Cursor",
        entry_kind: EntryKind::Http,
        prefix_path: &["mcpServers"],
        entry_type: None,
        url_field: "url",
        token_env: Some("MCPANE_CURSOR_TOKEN"),
        env_syntax: EnvSyntax::VsCode,
        terminal: false,
        install: InstallEvidence::Dir,
    },
    McpClientSpec {
        id: "vscode",
        display: "VS Code",
        entry_kind: EntryKind::Http,
        prefix_path: &["servers"],
        entry_type: Some("http"),
        url_field: "url",
        token_env: Some("MCPANE_VSCODE_TOKEN"),
        env_syntax: EnvSyntax::VsCode,
        terminal: false,
        install: InstallEvidence::Dir,
    },
    McpClientSpec {
        id: "cline",
        display: "Cline",
        entry_kind: EntryKind::Http,
        prefix_path: &["mcpServers"],
        entry_type: Some("streamableHttp"),
        url_field: "url",
        token_env: None,
        env_syntax: EnvSyntax::VsCode,
        terminal: false,
        install: InstallEvidence::Dir,
    },
    // Devin（原 Windsurf，2026-06 改名 Devin Desktop；配置路径向后兼容保留 .codeium/windsurf）。
    McpClientSpec {
        id: "devin",
        display: "Devin",
        entry_kind: EntryKind::Http,
        prefix_path: &["mcpServers"],
        entry_type: None,
        url_field: "serverUrl",
        token_env: Some("MCPANE_DEVIN_TOKEN"),
        env_syntax: EnvSyntax::VsCode,
        terminal: false,
        install: InstallEvidence::Dir,
    },
    // Gemini CLI：靠字段名推断传输（无 type 字段），Streamable HTTP 用
    // `httpUrl`；settings.json 所有字符串值支持 `$VAR` 展开（见 bearer_value
    // 的语法分支）。https://github.com/google-gemini/gemini-cli docs/tools/mcp-server.md
    McpClientSpec {
        id: "gemini-cli",
        display: "Gemini CLI",
        entry_kind: EntryKind::Http,
        prefix_path: &["mcpServers"],
        entry_type: None,
        url_field: "httpUrl",
        token_env: Some("MCPANE_GEMINI_TOKEN"),
        env_syntax: EnvSyntax::Dotenv,
        terminal: true,
        install: InstallEvidence::ProviderApp(mcpane_provider::ClientApp::Gemini),
    },
    // Antigravity（Google）：IDE / CLI（`agy`）/ 2.0 共用同一份全局 mcp_config.json。
    // 远程条目是 `serverUrl` + `headers`、不写 type；配置里不做变量展开——内核自带的
    // MCP 文档只列 command/args/env/cwd/serverUrl/headers，没有展开这回事，故同 Cline 只能占位。
    // https://antigravity.google/docs/mcp
    McpClientSpec {
        id: "antigravity",
        display: "Antigravity",
        entry_kind: EntryKind::Http,
        prefix_path: &["mcpServers"],
        entry_type: None,
        url_field: "serverUrl",
        token_env: None,
        env_syntax: EnvSyntax::Dotenv,
        terminal: false,
        install: InstallEvidence::DirOrCli("agy"),
    },
    // ---- 国内 AI 编程客户端 ----
    // Trae（字节 TraeCode）：HTTP 条目只有 `url` + `headers`，不写 type；配置里
    // 唯一支持的变量是 `${workspaceFolder}`，Token 没法走 env 引用（同 Cline）。
    // https://docs.trae.cn/ide_add-mcp-servers
    McpClientSpec {
        id: "trae",
        display: "Trae",
        entry_kind: EntryKind::Http,
        prefix_path: &["mcpServers"],
        entry_type: None,
        url_field: "url",
        token_env: None,
        env_syntax: EnvSyntax::Dotenv,
        terminal: false,
        install: InstallEvidence::Dir,
    },
    // Qoder（阿里；通义灵码 Lingma 的继任者，也是已停服的 iFlow CLI 的官方迁移目标）。
    // IDE 与 CLI 共用用户级 `~/.qoder/settings.json`，远程条目写 `type: "http"`，
    // headers 值支持 `${VAR}` 展开。
    // https://docs.qoder.com/zh/cli/mcp-reference
    McpClientSpec {
        id: "qoder",
        display: "Qoder",
        entry_kind: EntryKind::Http,
        prefix_path: &["mcpServers"],
        entry_type: Some("http"),
        url_field: "url",
        token_env: Some("MCPANE_QODER_TOKEN"),
        env_syntax: EnvSyntax::Dotenv,
        terminal: false,
        install: InstallEvidence::Dir,
    },
    // Qwen Code（阿里）：Gemini CLI 的分叉，同样靠字段名判传输——Streamable HTTP
    // 用 `httpUrl`，settings.json 的字符串值认 `$VAR` / `${VAR}`。
    // https://qwenlm.github.io/qwen-code-docs/en/users/configuration/settings/
    McpClientSpec {
        id: "qwen-code",
        display: "Qwen Code",
        entry_kind: EntryKind::Http,
        prefix_path: &["mcpServers"],
        entry_type: None,
        url_field: "httpUrl",
        token_env: Some("MCPANE_QWEN_TOKEN"),
        env_syntax: EnvSyntax::Dotenv,
        terminal: true,
        install: InstallEvidence::Dir,
    },
    // CodeBuddy（腾讯 CodeBuddy Code）：显式写 `type: "http"`，headers 值支持
    // `${VAR}` 展开。用户级配置官方优先级最高的是 `~/.codebuddy/.mcp.json`
    // （`mcp.json` 已废弃、`~/.codebuddy.json` 是旧版）。
    // https://www.codebuddy.ai/docs/zh/cli/mcp
    McpClientSpec {
        id: "codebuddy",
        display: "CodeBuddy",
        entry_kind: EntryKind::Http,
        prefix_path: &["mcpServers"],
        entry_type: Some("http"),
        url_field: "url",
        token_env: Some("MCPANE_CODEBUDDY_TOKEN"),
        env_syntax: EnvSyntax::Dotenv,
        terminal: true,
        install: InstallEvidence::Dir,
    },
    // ZCode（智谱 Z.ai 的 GLM 官方 Harness）。三处与别家都不同：
    // 1. Server 表挂在嵌套的 `mcp.servers` 下，不是顶层 `mcpServers`——顶层那种
    //    形态只有兼容文件 `~/.agents/mcp.json` 用，而它仅在 `.zcode` 里一个 MCP
    //    服务都没有时才生效，写那边等于写进一个随时会被整体跳过的文件；
    // 2. 条目 schema 是严格的，多写一个未知字段整条会被丢弃，故只写
    //    type / url / headers 三项（Cline 那套 disabled / autoApprove 不能带）；
    // 3. 配置文件里的 `${...}` 不做展开（只有插件提供的 MCP 才展开），
    //    所以凭据没法走 env 引用，同 Cline / Trae。
    // https://zcode.z.ai/cn/docs/mcp-services
    McpClientSpec {
        id: "zcode",
        display: "ZCode",
        entry_kind: EntryKind::Http,
        prefix_path: &["mcp", "servers"],
        entry_type: Some("http"),
        url_field: "url",
        token_env: None,
        env_syntax: EnvSyntax::Dotenv,
        terminal: false,
        install: InstallEvidence::Dir,
    },
];

/// 按 id 取规格。
pub fn spec(id: &str) -> Option<&'static McpClientSpec> {
    CLIENTS.iter().find(|c| c.id == id)
}

/// Claude Desktop 的用户数据目录。
///
/// 复用 provider 的实现：那边写 `deploymentMode`、这边写 `mcpServers`，是同一个
/// `claude_desktop_config.json`，各算各的必然漂移（Windows 上就漂过）。
fn claude_desktop_dir(home: &Path) -> PathBuf {
    mcpane_provider::claude_desktop::user_data_dir(home)
}

fn claude_desktop_probe(home: &Path) -> PathBuf {
    mcpane_provider::claude_desktop::install_probe_dir(home)
}

/// stdio 桥的 command 值：与本进程同目录的 mcpane-hub 二进制
/// （tauri externalBin 平铺拷贝，dev 与 bundle 布局一致）。
/// 解析失败时回退裸名——依赖 PATH 兜底，至少让配置可读可改。
pub fn bridge_command() -> String {
    let name = if cfg!(windows) {
        "mcpane-hub.exe"
    } else {
        "mcpane-hub"
    };
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_owned())
}

/// stdio env 块里的 Token 值是否为可用明文（非空、非占位、非 env 引用）。
fn is_usable_raw_token(value: &str) -> bool {
    !value.is_empty() && !value.contains('<') && !value.starts_with("${")
}

/// PATH（补齐后）里有没有这个可执行文件。
fn locate_cli(name: &str) -> bool {
    mcpane_hub_protocol::cli_path::locate_cli(name).is_some()
}

/// 各平台的应用数据根目录（VS Code 系编辑器与 Electron 应用都落在这里）。
fn app_data_root(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Application Support")
    }
    #[cfg(target_os = "windows")]
    {
        // %APPDATA%（缺失时回退到 home 下的常见布局）。
        match std::env::var_os("APPDATA") {
            Some(appdata) => PathBuf::from(appdata),
            None => home.join("AppData/Roaming"),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        home.join(".config")
    }
}

/// VS Code 系编辑器的用户级 profile 目录：`<app-data>/<app>/User`。
fn editor_user_dir(home: &Path, app: &str) -> PathBuf {
    app_data_root(home).join(app).join("User")
}

/// VS Code / Cline 共用的用户级 profile 目录。
fn vscode_user_dir(home: &Path) -> PathBuf {
    editor_user_dir(home, "Code")
}

/// 取第一个存在的目录；都不存在时返回首个候选——调用方据此判「未安装」，
/// 也让 `config_path` 在全新机器上有个确定的落点。
fn first_existing_dir<const N: usize>(mut candidates: [PathBuf; N]) -> PathBuf {
    let index = candidates.iter().position(|dir| dir.is_dir()).unwrap_or(0);
    std::mem::take(&mut candidates[index])
}

/// Trae 的用户 profile 目录。国内版（`Trae CN`）与国际版（`Trae`）布局同构，
/// 装了哪个认哪个；都没装时落到国内版——这条目就是为国内用户加的。
fn trae_user_dir(home: &Path) -> PathBuf {
    first_existing_dir([
        editor_user_dir(home, "Trae CN"),
        editor_user_dir(home, "Trae"),
    ])
}

/// Antigravity 的 MCP 配置目录。2.x 把配置从运行时状态目录 `~/.gemini/antigravity/`
/// 迁到了 `~/.gemini/config/`（迁移后前者只剩 brain / 会话状态，写进去没人读），
/// 没迁移过的旧版仍读旧目录——装了哪个认哪个，全新机器落新版。
fn antigravity_config_dir(home: &Path) -> PathBuf {
    first_existing_dir([
        home.join(".gemini").join("config"),
        home.join(".gemini").join("antigravity"),
    ])
}

/// Antigravity 的安装探测目录。`~/.gemini` 本身不能当判据——Gemini CLI 用的也是它；
/// IDE 的支持目录（VS Code 系布局，改名前后各一个）与两处配置目录，任一存在即算装。
fn antigravity_probe_dir(home: &Path) -> PathBuf {
    first_existing_dir([
        home.join(".antigravity-ide"),
        home.join(".antigravity"),
        home.join(".gemini").join("config"),
        home.join(".gemini").join("antigravity"),
    ])
}

/// Qoder 的安装探测目录。CLI 建 `~/.qoder`，IDE 的应用数据目录另在一处，
/// 装了任一就算装——只认 `~/.qoder` 会让纯 IDE 用户卡在「未检测到安装」上，
/// 而两者读的是同一份用户级 `~/.qoder/settings.json`。
fn qoder_probe_dir(home: &Path) -> PathBuf {
    first_existing_dir([home.join(".qoder"), app_data_root(home).join("Qoder")])
}

impl McpClientSpec {
    /// 顶层 key 前缀（PatchPlan 用）。
    fn prefix(&self) -> Vec<String> {
        self.prefix_path.iter().map(|s| (*s).to_owned()).collect()
    }

    /// mcpane 条目的完整键路径（读取与校验共用）。
    fn entry_segs(&self) -> Vec<&'static str> {
        let mut segs = self.prefix_path.to_vec();
        segs.push(ENTRY_NAME);
        segs
    }

    /// 用户级配置文件绝对路径。
    pub fn config_path(&self, home: &Path) -> PathBuf {
        match self.id {
            // https://modelcontextprotocol.io/docs/develop/connect-local-servers
            "claude-desktop" => claude_desktop_dir(home).join("claude_desktop_config.json"),
            // https://cursor.com/docs/mcp
            "cursor" => home.join(".cursor").join("mcp.json"),
            // https://code.visualstudio.com/docs/agents/reference/mcp-configuration（1.102+ 独立 mcp.json）
            "vscode" => vscode_user_dir(home).join("mcp.json"),
            // https://docs.cline.bot/mcp/mcp-overview（VS Code 扩展 globalStorage）
            "cline" => vscode_user_dir(home)
                .join("globalStorage/saoudrizwan.claude-dev/settings")
                .join("cline_mcp_settings.json"),
            // Devin（原 Windsurf）：https://docs.devin.ai/windsurf/plugins/cascade/mcp
            // 改名后配置路径向后兼容，仍在 .codeium/windsurf。
            "devin" => home.join(".codeium/windsurf").join("mcp_config.json"),
            "gemini-cli" => home.join(".gemini").join("settings.json"),
            // 全局落点（IDE / CLI / 2.0 共用）；工作区级的 .agents/mcp_config.json 不归 MCPane 管。
            // https://antigravity.google/docs/mcp
            "antigravity" => antigravity_config_dir(home).join("mcp_config.json"),
            // https://docs.trae.cn/ide_add-mcp-servers（VS Code 系 profile 目录）
            "trae" => trae_user_dir(home).join("mcp.json"),
            // https://docs.qoder.com/zh/cli/mcp-reference（IDE 与 CLI 共用用户级）
            "qoder" => home.join(".qoder").join("settings.json"),
            "qwen-code" => home.join(".qwen").join("settings.json"),
            // https://www.codebuddy.ai/docs/zh/cli/mcp（user 作用域最高优先级）
            "codebuddy" => home.join(".codebuddy").join(".mcp.json"),
            // 用户级原生配置在 .zcode/cli/ 下（工作区级才是 .zcode/config.json）。
            // https://zcode.z.ai/cn/docs/mcp-services
            "zcode" => home.join(".zcode").join("cli").join("config.json"),
            _ => home.join(".mcpane-unknown-client.json"),
        }
    }

    /// 目录判据的探测目录。只列 [`InstallEvidence::Dir`] / `DirOrCli` 的客户端——
    /// `ProviderApp` 那几家没有能证明归属的目录，落到兜底分支也不会被 `is_installed` 读到。
    fn install_probe(&self, home: &Path) -> PathBuf {
        match self.id {
            "claude-desktop" => claude_desktop_probe(home),
            "cursor" => home.join(".cursor"),
            "vscode" => vscode_user_dir(home),
            "cline" => vscode_user_dir(home).join("globalStorage/saoudrizwan.claude-dev"),
            "devin" => home.join(".codeium/windsurf"),
            "antigravity" => antigravity_probe_dir(home),
            "trae" => trae_user_dir(home),
            "qoder" => qoder_probe_dir(home),
            "qwen-code" => home.join(".qwen"),
            "codebuddy" => home.join(".codebuddy"),
            "zcode" => home.join(".zcode"),
            _ => home.join(".mcpane-unknown-client"),
        }
    }

    /// 是否检测到安装。
    ///
    /// CLI 判据走 `cli_path`（含 GUI 进程的 PATH 补齐：Dock 启动的应用看不到
    /// Homebrew / nvm / npm 全局 bin）；`ProviderApp` 直接借供应商页那套判定，
    /// 它对痕迹文件的归属已经分得很细。
    pub fn is_installed(&self, home: &Path) -> bool {
        match self.install {
            InstallEvidence::Dir => self.install_probe(home).is_dir(),
            InstallEvidence::DirOrCli(bin) => self.install_probe(home).is_dir() || locate_cli(bin),
            InstallEvidence::ProviderApp(app) => mcpane_provider::is_installed(app),
        }
    }

    /// 文档中是否已存在 mcpane Hub 条目。
    pub fn has_hub_entry(&self, doc: &ConfigDocument) -> bool {
        doc.get(&self.entry_segs()).is_some()
    }

    /// mcpane 条目当前是否为 Inline 凭据形态（轮换 Token 联动改写的判断依据；
    /// env 引用/占位串/缺失都返回 false——那些形态不由轮换接管）。
    pub fn uses_inline_credential(&self, doc: &ConfigDocument) -> bool {
        doc.get(&self.entry_segs())
            .and_then(|entry| match self.entry_kind {
                EntryKind::Http => entry["headers"]["Authorization"]
                    .as_str()
                    .map(is_usable_inline_bearer),
                EntryKind::StdioBridge => entry["env"][mcpane_hub_protocol::bridge_env::TOKEN]
                    .as_str()
                    .map(is_usable_raw_token),
            })
            .unwrap_or(false)
    }

    /// 是否已接入：配置文件里已写入 mcpane 条目。
    pub fn is_connected(&self, home: &Path) -> bool {
        let Ok(content) = std::fs::read_to_string(self.config_path(home)) else {
            return false;
        };
        ConfigDocument::parse(&content, ConfigFormat::Json)
            .is_ok_and(|doc| self.has_hub_entry(&doc))
    }

    /// Authorization 头的 Bearer 值。
    /// Inline：明文（Cline / Trae 从此不再需要手填占位）；
    /// EnvVar：支持变量则按 `env_syntax` 展开，否则只能占位串。
    /// EnvVar 模式下 Authorization 里的展开表达式（写入与校验共用），语法按
    /// `env_syntax` 分流——同一串 `${env:X}` 在 CLI 系里就是个字面量。
    fn env_ref_expr(&self, env: &str) -> String {
        match self.env_syntax {
            EnvSyntax::VsCode => format!("${{env:{env}}}"),
            EnvSyntax::Dotenv => format!("${{{env}}}"),
        }
    }

    fn bearer_value(&self, credential: CredentialDelivery<'_>) -> String {
        match credential {
            CredentialDelivery::Inline(token) => format!("Bearer {token}"),
            CredentialDelivery::EnvVar => match self.token_env {
                Some(env) => format!("Bearer {}", self.env_ref_expr(env)),
                None => format!("Bearer {CLINE_TOKEN_PLACEHOLDER}"),
            },
        }
    }

    /// 生成 Server 条目的 JSON 值（Http 远程条目 / StdioBridge 桥条目）。
    fn entry_value(&self, hub_url: &str, credential: CredentialDelivery<'_>) -> Value {
        if self.entry_kind == EntryKind::StdioBridge {
            let token = match credential {
                CredentialDelivery::Inline(token) => token.to_owned(),
                // stdio env 块不支持 `${env:}` 展开，EnvVar 只能占位（命令层
                // 已拒绝该模式，此分支仅供「使用说明」静态片段展示）。
                CredentialDelivery::EnvVar => CLINE_TOKEN_PLACEHOLDER.to_owned(),
            };
            return json!({
                "command": bridge_command(),
                "args": ["bridge"],
                "env": {
                    mcpane_hub_protocol::bridge_env::TOKEN: token,
                    mcpane_hub_protocol::bridge_env::URL: hub_url,
                },
            });
        }
        let mut entry = serde_json::Map::new();
        if let Some(ty) = self.entry_type {
            entry.insert("type".into(), json!(ty));
        }
        entry.insert(self.url_field.into(), json!(hub_url));
        entry.insert(
            "headers".into(),
            json!({ "Authorization": self.bearer_value(credential) }),
        );
        // Cline 官方示例的必要字段，缺省会影响其 UI 展示与自动批准行为。
        if self.id == "cline" {
            entry.insert("disabled".into(), json!(false));
            entry.insert("autoApprove".into(), json!([]));
        }
        Value::Object(entry)
    }

    /// 写入 Hub 条目的 PatchPlan。
    pub fn hub_entry_patch(
        &self,
        hub_url: Option<&str>,
        credential: CredentialDelivery<'_>,
    ) -> PatchPlan {
        let mut path = self.prefix();
        path.push(ENTRY_NAME.to_owned());
        PatchPlan::new(vec![PatchOp::Set {
            path,
            value: self.entry_value(hub_url.unwrap_or(DEFAULT_HUB_URL), credential),
        }])
    }

    /// 删除 Hub 条目（不触碰其他 Server）。
    pub fn remove_hub_entry_patch(&self) -> PatchPlan {
        let mut path = self.prefix();
        path.push(ENTRY_NAME.to_owned());
        PatchPlan::new(vec![PatchOp::Remove { path }])
    }

    /// 写入后业务校验：条目存在、URL 指向 loopback、凭据形态与模式一致。
    pub fn validate_hub_entry(
        &self,
        doc: &ConfigDocument,
        mode: CredentialMode,
    ) -> Result<(), String> {
        let segs = self.entry_segs();
        let entry = doc
            .get(&segs)
            .ok_or_else(|| format!("缺少 {} 条目", segs.join(".")))?;

        if self.entry_kind == EntryKind::StdioBridge {
            let command = entry["command"].as_str().unwrap_or_default();
            let expected = if cfg!(windows) {
                "mcpane-hub.exe"
            } else {
                "mcpane-hub"
            };
            if Path::new(command).file_name() != Some(std::ffi::OsStr::new(expected)) {
                return Err(format!(
                    "mcpane 条目 command 必须是 {expected} 桥：{command}"
                ));
            }
            let args_ok = entry["args"]
                .as_array()
                .is_some_and(|a| a.first().and_then(Value::as_str) == Some("bridge"));
            if !args_ok {
                return Err("mcpane 条目 args 必须以 \"bridge\" 开头".into());
            }
            let hub_url = entry["env"][mcpane_hub_protocol::bridge_env::URL]
                .as_str()
                .unwrap_or_default();
            if !hub_url.starts_with("http://127.0.0.1") && !hub_url.starts_with("http://localhost")
            {
                return Err(format!("桥的 Hub 端点必须指向 loopback：{hub_url}"));
            }
            let token = entry["env"][mcpane_hub_protocol::bridge_env::TOKEN]
                .as_str()
                .unwrap_or_default();
            return match mode {
                CredentialMode::Inline => {
                    if !is_usable_raw_token(token) {
                        return Err("env 块必须包含可用的 Token（非空、非占位）".into());
                    }
                    Ok(())
                }
                // stdio env 块不支持 `${env:}` 展开（命令层已拒绝，此分支
                // 对应「使用说明」静态片段：保留占位串由用户手填）。
                CredentialMode::EnvVar => {
                    if !token.contains(CLINE_TOKEN_PLACEHOLDER) {
                        return Err("EnvVar 模式必须保留 Token 占位串".into());
                    }
                    Ok(())
                }
            };
        }

        let url = entry[self.url_field].as_str().unwrap_or_default();
        if !url.starts_with("http://127.0.0.1") && !url.starts_with("http://localhost") {
            return Err(format!("mcpane 条目 URL 必须指向 loopback：{url}"));
        }

        let auth = entry["headers"]["Authorization"]
            .as_str()
            .unwrap_or_default();
        match mode {
            CredentialMode::Inline => {
                if !is_usable_inline_bearer(auth) {
                    return Err("Authorization 必须是可用的 Bearer Token（非空、非占位）".into());
                }
            }
            CredentialMode::EnvVar => match self.token_env {
                Some(env) => {
                    let expected = self.env_ref_expr(env);
                    if !auth.contains(&expected) {
                        return Err(format!("Authorization 必须使用 {expected} 展开"));
                    }
                }
                // Cline / Trae：不认变量展开，EnvVar 模式只写占位串，由用户手填。
                None => {
                    if !auth.contains(CLINE_TOKEN_PLACEHOLDER) {
                        return Err(format!("{} EnvVar 模式必须保留 Token 占位串", self.display));
                    }
                }
            },
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use mcpane_config::ConfigFormat;

    fn apply_to_empty(id: &str, credential: CredentialDelivery<'_>) -> ConfigDocument {
        let spec = spec(id).unwrap();
        let mut doc = ConfigDocument::parse("{}", ConfigFormat::Json).unwrap();
        spec.hub_entry_patch(None, credential)
            .apply(&mut doc)
            .unwrap();
        doc
    }

    #[test]
    fn cursor_inline_writes_bearer_and_env_mode_uses_ref() {
        let spec = spec("cursor").unwrap();
        let doc = apply_to_empty("cursor", CredentialDelivery::Inline("tok1234"));
        spec.validate_hub_entry(&doc, CredentialMode::Inline)
            .unwrap();
        let out = doc.serialize();
        assert!(out.contains("mcpServers"));
        assert!(out.contains("Bearer tok1234"));
        assert!(!out.contains("\"type\""), "Cursor 远程条目不写 type");

        let doc = apply_to_empty("cursor", CredentialDelivery::EnvVar);
        spec.validate_hub_entry(&doc, CredentialMode::EnvVar)
            .unwrap();
        assert!(doc.serialize().contains("${env:MCPANE_CURSOR_TOKEN}"));
    }

    #[test]
    fn vscode_uses_servers_key_and_http_type() {
        let doc = apply_to_empty("vscode", CredentialDelivery::EnvVar);
        let spec = spec("vscode").unwrap();
        spec.validate_hub_entry(&doc, CredentialMode::EnvVar)
            .unwrap();
        let out = doc.serialize();
        assert!(out.contains("\"servers\""), "VS Code 顶层是 servers");
        assert!(out.contains("\"type\": \"http\"") || out.contains("\"type\":\"http\""));
        assert!(out.contains("${env:MCPANE_VSCODE_TOKEN}"));
    }

    #[test]
    fn gemini_cli_uses_http_url_field_and_dollar_var_syntax() {
        let doc = apply_to_empty("gemini-cli", CredentialDelivery::EnvVar);
        let spec = spec("gemini-cli").unwrap();
        spec.validate_hub_entry(&doc, CredentialMode::EnvVar)
            .unwrap();
        let out = doc.serialize();
        assert!(
            out.contains("\"httpUrl\""),
            "Gemini 的 Streamable HTTP 字段是 httpUrl"
        );
        assert!(
            !out.contains("\"type\""),
            "Gemini 靠字段名判传输，不写 type"
        );
        // Gemini 的展开语法是 ${VAR}，不是编辑器系的 ${env:VAR}。
        assert!(out.contains("Bearer ${MCPANE_GEMINI_TOKEN}"));
        assert!(!out.contains("${env:"));
    }

    /// 国内四家：URL 字段、type 写法、展开语法各不相同，逐家钉死。
    /// 抄错任何一项都不会报错，只会安静地连不上。
    #[test]
    fn domestic_clients_write_their_own_entry_shapes() {
        // Trae：`url` + `headers`，不写 type；配置只认 `${workspaceFolder}`，
        // 所以 EnvVar 模式退回占位串（同 Cline）。
        let trae = spec("trae").unwrap();
        assert!(trae.token_env.is_none(), "Trae 不支持凭据的变量展开");
        let doc = apply_to_empty("trae", CredentialDelivery::Inline("tok-trae"));
        trae.validate_hub_entry(&doc, CredentialMode::Inline)
            .unwrap();
        let out = doc.serialize();
        assert!(out.contains("\"url\""));
        assert!(out.contains("Bearer tok-trae"));
        assert!(!out.contains("\"type\""), "Trae 远程条目不写 type");
        let doc = apply_to_empty("trae", CredentialDelivery::EnvVar);
        trae.validate_hub_entry(&doc, CredentialMode::EnvVar)
            .unwrap();
        assert!(doc.serialize().contains(CLINE_TOKEN_PLACEHOLDER));

        // Qoder：显式 `type: "http"`，展开语法是 dotenv 风格的 ${VAR}。
        let qoder = spec("qoder").unwrap();
        let doc = apply_to_empty("qoder", CredentialDelivery::EnvVar);
        qoder
            .validate_hub_entry(&doc, CredentialMode::EnvVar)
            .unwrap();
        let out = doc.serialize();
        assert!(out.contains("\"type\": \"http\"") || out.contains("\"type\":\"http\""));
        assert!(out.contains("Bearer ${MCPANE_QODER_TOKEN}"));
        assert!(!out.contains("${env:"), "Qoder 不认编辑器系的展开写法");

        // Qwen Code：Gemini CLI 同源——`httpUrl` 判传输，${VAR} 展开。
        let qwen = spec("qwen-code").unwrap();
        let doc = apply_to_empty("qwen-code", CredentialDelivery::EnvVar);
        qwen.validate_hub_entry(&doc, CredentialMode::EnvVar)
            .unwrap();
        let out = doc.serialize();
        assert!(out.contains("\"httpUrl\""), "Qwen Code 的字段是 httpUrl");
        assert!(!out.contains("\"type\""), "Qwen Code 靠字段名判传输");
        assert!(out.contains("Bearer ${MCPANE_QWEN_TOKEN}"));

        // CodeBuddy：`type: "http"` + ${VAR}（headers 值支持展开）。
        let codebuddy = spec("codebuddy").unwrap();
        let doc = apply_to_empty("codebuddy", CredentialDelivery::EnvVar);
        codebuddy
            .validate_hub_entry(&doc, CredentialMode::EnvVar)
            .unwrap();
        let out = doc.serialize();
        assert!(out.contains("\"type\": \"http\"") || out.contains("\"type\":\"http\""));
        assert!(out.contains("Bearer ${MCPANE_CODEBUDDY_TOKEN}"));
        assert!(!out.contains("${env:"), "CodeBuddy 不认编辑器系的展开写法");
    }

    /// ZCode：嵌套键 `mcp.servers` + 严格 schema。这两条任一踩空都不报错——
    /// 键写错是写进一个没人读的位置，多写字段是整条被 ZCode 丢弃。
    #[test]
    fn zcode_writes_into_nested_mcp_servers_with_strict_schema() {
        let zcode = spec("zcode").unwrap();
        assert!(zcode.token_env.is_none(), "ZCode 配置文件不做变量展开");

        let doc = apply_to_empty("zcode", CredentialDelivery::Inline("tok-zcode"));
        zcode
            .validate_hub_entry(&doc, CredentialMode::Inline)
            .unwrap();
        assert!(zcode.uses_inline_credential(&doc), "轮换联动依赖此判定");

        // 落点是 mcp.servers.mcpane，不是顶层 mcpServers。
        // 路径不存在会在这里直接 panic，正是想要的失败方式。
        let entry = doc.get(&["mcp", "servers", "mcpane"]).unwrap();
        assert!(doc.get(&["mcpServers"]).is_none(), "不该写顶层 mcpServers");
        assert_eq!(entry["type"].as_str(), Some("http"));
        assert!(
            entry["url"]
                .as_str()
                .unwrap_or_default()
                .contains("127.0.0.1")
        );
        assert_eq!(
            entry["headers"]["Authorization"].as_str(),
            Some("Bearer tok-zcode")
        );
        // 严格 schema：多一个未知键，ZCode 会把整条服务丢掉。
        let keys: Vec<&str> = entry
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["type", "url", "headers"], "只允许这三个键：{keys:?}");

        // EnvVar 模式退回占位串（同 Cline / Trae）。
        let doc = apply_to_empty("zcode", CredentialDelivery::EnvVar);
        zcode
            .validate_hub_entry(&doc, CredentialMode::EnvVar)
            .unwrap();
        assert!(doc.serialize().contains(CLINE_TOKEN_PLACEHOLDER));
    }

    /// ZCode 的这个文件还存着权限、插件、Hooks 与用户自己的 MCP 服务，
    /// 接入与解绑都只能动 mcpane 那一条。
    #[test]
    fn zcode_connect_and_disconnect_keep_the_rest_of_the_config() {
        let zcode = spec("zcode").unwrap();
        let mut doc = ConfigDocument::parse(
            r#"{"permissions":{"defaultMode":"ask"},
               "mcp":{"servers":{"memory":{"command":"npx","args":["-y","server-memory"]}}},
               "hooks":{"PreToolUse":[]}}"#,
            ConfigFormat::Json,
        )
        .unwrap();
        zcode
            .hub_entry_patch(None, CredentialDelivery::Inline("tok"))
            .apply(&mut doc)
            .unwrap();
        assert!(
            doc.get(&["permissions", "defaultMode"]).is_some(),
            "权限被冲掉了"
        );
        assert!(doc.get(&["hooks"]).is_some(), "Hooks 被冲掉了");
        assert!(
            doc.get(&["mcp", "servers", "memory"]).is_some(),
            "用户自己的 MCP 被冲掉了"
        );
        assert!(doc.get(&["mcp", "servers", "mcpane"]).is_some());

        zcode.remove_hub_entry_patch().apply(&mut doc).unwrap();
        assert!(doc.get(&["mcp", "servers", "mcpane"]).is_none());
        assert!(
            doc.get(&["mcp", "servers", "memory"]).is_some(),
            "解绑不该动别人"
        );
        assert!(doc.get(&["permissions", "defaultMode"]).is_some());
    }

    /// 端到端：拿真机 `~/.zcode/cli/config.json` 的形态（2026-08-29 实测，
    /// 含一条 stdio 邻居和一条已存在的 mcpane），走真实引擎跑「重新接入」。
    /// 前面那些测试都在 PatchPlan 层，这条盯的是快照/原子写/校验整条链路。
    #[test]
    fn zcode_reconnect_on_real_world_config_rewrites_only_our_entry() {
        use std::time::{SystemTime, UNIX_EPOCH};

        use mcpane_config::history::JsonlHistoryStore;
        use mcpane_config::snapshot::FsSnapshotStore;
        use mcpane_config::{ConfigEngine, RealFs};

        // 真机形态：mcp_router 是 stdio 邻居（带 env 与 enabled:false），
        // mcpane 是上一次接入留下的旧 Token。
        let existing = r#"{
  "mcp": {
    "servers": {
      "mcp_router": {
        "type": "stdio",
        "command": "npx",
        "args": ["-y", "@mcp_router/cli@latest", "connect"],
        "env": { "MCPR_TOKEN": "neighbour-secret" },
        "enabled": false
      },
      "mcpane": {
        "type": "http",
        "url": "http://127.0.0.1:24860/mcp",
        "headers": { "Authorization": "Bearer old-token" }
      }
    }
  }
}
"#;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let home = std::env::temp_dir().join(format!("mcpane-zcode-{nanos}"));
        let spec = spec("zcode").unwrap();
        let path = spec.config_path(&home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, existing).unwrap();

        let engine = ConfigEngine::new(
            FsSnapshotStore::new(home.join(".mcpane")),
            JsonlHistoryStore::new(home.join(".mcpane")),
            RealFs,
        );
        let loaded = engine.load(&path, ConfigFormat::Json).unwrap();
        let outcome = engine
            .apply(
                &loaded,
                &spec.hub_entry_patch(None, CredentialDelivery::Inline("new-token")),
                "test 重新接入",
                Some(&|doc| spec.validate_hub_entry(doc, CredentialMode::Inline)),
                None,
            )
            .unwrap();
        assert!(outcome.written);

        let after = engine.load(&path, ConfigFormat::Json).unwrap();
        let doc = ConfigDocument::parse(&after.content, ConfigFormat::Json).unwrap();
        // 我们那条被改写成新 Token。
        assert_eq!(
            doc.get(&["mcp", "servers", "mcpane"]).unwrap()["headers"]["Authorization"].as_str(),
            Some("Bearer new-token")
        );
        // 邻居一个字段都不能少——尤其是它的 env 与 enabled。
        let neighbour = doc.get(&["mcp", "servers", "mcp_router"]).unwrap();
        assert_eq!(neighbour["command"].as_str(), Some("npx"));
        assert_eq!(
            neighbour["env"]["MCPR_TOKEN"].as_str(),
            Some("neighbour-secret")
        );
        assert_eq!(neighbour["enabled"].as_bool(), Some(false));
        assert_eq!(neighbour["args"].as_array().map(Vec::len), Some(3));

        std::fs::remove_dir_all(&home).ok();
    }

    /// 配置落点：写错一个文件名，接入就是往无人读的文件里写。
    #[test]
    fn domestic_clients_target_documented_config_files() {
        let home = Path::new("/tmp/mcpane-home-not-exist");
        // 期望值统一用 `/` 写：这个用例验的是落点（目录层级与文件名），
        // 不是分隔符。Windows 上 config_path 返回 `\`，先归一再比，
        // 免得五条断言各自按平台分叉。
        let path_of = |id: &str| {
            spec(id)
                .unwrap()
                .config_path(home)
                .to_string_lossy()
                .replace('\\', "/")
        };
        assert!(path_of("qoder").ends_with(".qoder/settings.json"));
        assert!(path_of("qwen-code").ends_with(".qwen/settings.json"));
        // `.mcp.json` 带点：`mcp.json` 是已废弃的旧名。
        assert!(path_of("codebuddy").ends_with(".codebuddy/.mcp.json"));
        // 用户级在 .zcode/cli/ 下，工作区级才是 .zcode/config.json——差一层就写错文件。
        assert!(path_of("zcode").ends_with(".zcode/cli/config.json"));
        // home 不存在时 Trae 落到国内版目录。
        let trae = path_of("trae");
        assert!(trae.contains("Trae CN"), "默认落国内版：{trae}");
        assert!(trae.ends_with("User/mcp.json"), "{trae}");
    }

    /// Antigravity：远程条目是 `serverUrl`——写成 `url` 或 `httpUrl` 都不报错，
    /// 只是 Antigravity 认不出传输方式，条目安静地连不上。
    #[test]
    fn antigravity_writes_server_url_without_type() {
        let spec = spec("antigravity").unwrap();
        assert!(spec.token_env.is_none(), "Antigravity 的配置不做变量展开");

        let doc = apply_to_empty("antigravity", CredentialDelivery::Inline("tok-agy"));
        spec.validate_hub_entry(&doc, CredentialMode::Inline)
            .unwrap();
        assert!(spec.uses_inline_credential(&doc), "轮换联动依赖此判定");
        let entry = doc.get(&["mcpServers", "mcpane"]).unwrap();
        assert!(
            entry["serverUrl"]
                .as_str()
                .unwrap_or_default()
                .contains("127.0.0.1")
        );
        assert!(entry["url"].is_null(), "字段名是 serverUrl，不是 url");
        assert!(
            entry["type"].is_null(),
            "Antigravity 靠字段名判传输，不写 type"
        );
        assert_eq!(
            entry["headers"]["Authorization"].as_str(),
            Some("Bearer tok-agy")
        );

        // EnvVar：不支持展开，退回占位串手填（同 Cline / Trae / ZCode）。
        let doc = apply_to_empty("antigravity", CredentialDelivery::EnvVar);
        spec.validate_hub_entry(&doc, CredentialMode::EnvVar)
            .unwrap();
        assert!(doc.serialize().contains(CLINE_TOKEN_PLACEHOLDER));
    }

    /// `~/.gemini` 是 Gemini CLI 与 Antigravity 两家的家。Gemini CLI 是纯 CLI，
    /// 判据只能是 PATH 里的可执行——退回目录判据的话，只装了 Antigravity 的机器
    /// 会把 Gemini CLI 报成「已安装」，用户点接入才发现根本没这个命令。
    #[test]
    fn gemini_cli_install_check_ignores_the_shared_dot_gemini_dir() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let gemini = spec("gemini-cli").unwrap();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let bare = std::env::temp_dir().join(format!("mcpane-gemini-bare-{nanos}"));
        let with_dir = std::env::temp_dir().join(format!("mcpane-gemini-dir-{nanos}"));
        // Antigravity 建出来的形态：`~/.gemini` 在，还带着它自己的两个子目录。
        std::fs::create_dir_all(with_dir.join(".gemini").join("config")).unwrap();
        std::fs::create_dir_all(with_dir.join(".gemini").join("antigravity")).unwrap();

        assert_eq!(
            gemini.is_installed(&bare),
            gemini.is_installed(&with_dir),
            "Gemini CLI 的安装判据不该受 ~/.gemini 是否存在影响"
        );

        // 反过来：只有 Gemini CLI 的家（没有 Antigravity 的任何痕迹）时，
        // Antigravity 不能算已安装。
        let gemini_only = std::env::temp_dir().join(format!("mcpane-gemini-only-{nanos}"));
        std::fs::create_dir_all(gemini_only.join(".gemini")).unwrap();
        std::fs::write(gemini_only.join(".gemini").join("settings.json"), "{}").unwrap();
        assert!(
            !spec("antigravity").unwrap().is_installed(&gemini_only)
                || mcpane_hub_protocol::cli_path::locate_cli("agy").is_some(),
            "裸 ~/.gemini 不是 Antigravity 装过的证据"
        );

        for dir in [bare, with_dir, gemini_only] {
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    /// Antigravity 2.x 迁移过配置落点：旧目录迁移后只剩运行时状态，写进去没人读。
    /// 装了哪个认哪个，两个都在（迁移后的机器）时认新的。
    #[test]
    fn antigravity_prefers_migrated_config_dir() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let spec = spec("antigravity").unwrap();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let home = std::env::temp_dir().join(format!("mcpane-agy-{nanos}"));
        let path_of = |home: &Path| spec.config_path(home).to_string_lossy().replace('\\', "/");

        // 全新机器：落新版目录。
        assert!(path_of(&home).ends_with(".gemini/config/mcp_config.json"));

        // 只有旧目录（未迁移的老版本）：落旧版。
        std::fs::create_dir_all(home.join(".gemini").join("antigravity")).unwrap();
        assert!(
            path_of(&home).ends_with(".gemini/antigravity/mcp_config.json"),
            "未迁移的机器得写旧目录：{}",
            path_of(&home)
        );
        assert!(spec.is_installed(&home), "旧目录在就算装了");

        // 迁移后两个都在：认新的。
        std::fs::create_dir_all(home.join(".gemini").join("config")).unwrap();
        assert!(path_of(&home).ends_with(".gemini/config/mcp_config.json"));

        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn devin_uses_server_url_field() {
        let doc = apply_to_empty("devin", CredentialDelivery::EnvVar);
        let spec = spec("devin").unwrap();
        spec.validate_hub_entry(&doc, CredentialMode::EnvVar)
            .unwrap();
        let out = doc.serialize();
        assert!(out.contains("serverUrl"));
        assert!(out.contains("${env:MCPANE_DEVIN_TOKEN}"));
    }

    #[test]
    fn cline_inline_writes_real_token_env_mode_keeps_placeholder() {
        let spec = spec("cline").unwrap();
        // Inline：真实 Token 直写，零手动步骤（cline 是最大受益者）。
        let doc = apply_to_empty("cline", CredentialDelivery::Inline("tok-cline"));
        spec.validate_hub_entry(&doc, CredentialMode::Inline)
            .unwrap();
        let out = doc.serialize();
        assert!(out.contains("streamableHttp"));
        assert!(out.contains("Bearer tok-cline"));
        assert!(out.contains("autoApprove"));

        // EnvVar：不支持展开，退回占位串手填。
        let doc = apply_to_empty("cline", CredentialDelivery::EnvVar);
        spec.validate_hub_entry(&doc, CredentialMode::EnvVar)
            .unwrap();
        assert!(doc.serialize().contains(CLINE_TOKEN_PLACEHOLDER));
    }

    /// Claude Desktop：stdio 桥条目——command 指向 mcpane-hub、args ["bridge"]、
    /// Token 与 Hub 端点写进 env 块；EnvVar 模式只能占位（供使用说明片段）。
    #[test]
    fn claude_desktop_writes_stdio_bridge_entry() {
        let spec = spec("claude-desktop").unwrap();
        assert!(
            spec.token_env.is_none(),
            "stdio env 块不支持 ${{env:}} 展开"
        );

        let doc = apply_to_empty("claude-desktop", CredentialDelivery::Inline("tok-cd"));
        spec.validate_hub_entry(&doc, CredentialMode::Inline)
            .unwrap();
        assert!(spec.uses_inline_credential(&doc), "轮换联动依赖此判定");
        let out = doc.serialize();
        assert!(out.contains("\"command\""));
        assert!(out.contains("mcpane-hub"));
        assert!(out.contains("\"bridge\""));
        assert!(out.contains("\"MCPANE_TOKEN\""));
        assert!(out.contains("tok-cd"));
        assert!(out.contains("\"MCPANE_HUB_URL\""));
        assert!(out.contains("127.0.0.1"));
        assert!(!out.contains("Authorization"), "stdio 条目不写 HTTP 头");

        // EnvVar：占位串手填形态（命令层不放行该模式，仅静态片段用）。
        let doc = apply_to_empty("claude-desktop", CredentialDelivery::EnvVar);
        spec.validate_hub_entry(&doc, CredentialMode::EnvVar)
            .unwrap();
        assert!(!spec.uses_inline_credential(&doc), "占位串不算可用凭据");
        assert!(
            spec.validate_hub_entry(&doc, CredentialMode::Inline)
                .is_err(),
            "占位串在 Inline 模式下必须被拒"
        );
    }

    /// 桥条目校验拒绝被篡改的 command / 非 loopback 端点。
    #[test]
    fn claude_desktop_validation_rejects_tampered_entry() {
        let spec = spec("claude-desktop").unwrap();
        let doc = ConfigDocument::parse(
            r#"{"mcpServers":{"mcpane":{"command":"/usr/bin/evil","args":["bridge"],
               "env":{"MCPANE_TOKEN":"tok","MCPANE_HUB_URL":"http://127.0.0.1:24860/mcp"}}}}"#,
            ConfigFormat::Json,
        )
        .unwrap();
        assert!(
            spec.validate_hub_entry(&doc, CredentialMode::Inline)
                .is_err()
        );

        let doc = ConfigDocument::parse(
            r#"{"mcpServers":{"mcpane":{"command":"/app/mcpane-hub","args":["bridge"],
               "env":{"MCPANE_TOKEN":"tok","MCPANE_HUB_URL":"http://evil.com/mcp"}}}}"#,
            ConfigFormat::Json,
        )
        .unwrap();
        assert!(
            spec.validate_hub_entry(&doc, CredentialMode::Inline)
                .is_err()
        );
    }

    #[test]
    fn inline_validation_rejects_placeholder_and_env_ref() {
        // 占位串在 Inline 模式下不是可用凭据。
        let doc = apply_to_empty("cline", CredentialDelivery::EnvVar);
        assert!(
            spec("cline")
                .unwrap()
                .validate_hub_entry(&doc, CredentialMode::Inline)
                .is_err()
        );
        // env 引用在 Inline 模式下同样不可用；明文在 EnvVar 模式下被拒。
        let doc = apply_to_empty("cursor", CredentialDelivery::EnvVar);
        assert!(
            spec("cursor")
                .unwrap()
                .validate_hub_entry(&doc, CredentialMode::Inline)
                .is_err()
        );
        let doc = apply_to_empty("cursor", CredentialDelivery::Inline("tok"));
        assert!(
            spec("cursor")
                .unwrap()
                .validate_hub_entry(&doc, CredentialMode::EnvVar)
                .is_err()
        );
    }

    #[test]
    fn validation_rejects_non_loopback() {
        let doc = ConfigDocument::parse(
            r#"{"mcpServers":{"mcpane":{"url":"http://evil.com/mcp",
               "headers":{"Authorization":"Bearer ${env:MCPANE_CURSOR_TOKEN}"}}}}"#,
            ConfigFormat::Json,
        )
        .unwrap();
        assert!(
            spec("cursor")
                .unwrap()
                .validate_hub_entry(&doc, CredentialMode::EnvVar)
                .is_err()
        );
    }

    /// 端到端：模拟命令层「首次接入」——配置文件不存在 → 建父目录 → 走真实引擎
    /// apply（快照/原子写/校验）→ 落盘内容正确且可被再次读回。
    #[test]
    fn first_connect_on_missing_file_creates_valid_config() {
        use std::time::{SystemTime, UNIX_EPOCH};

        use mcpane_config::history::JsonlHistoryStore;
        use mcpane_config::snapshot::FsSnapshotStore;
        use mcpane_config::{ConfigEngine, ConfigFormat, RealFs};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let home = std::env::temp_dir().join(format!("mcpane-test-home-{nanos}"));
        let mcpane_home = home.join(".mcpane");

        let spec = spec("cursor").unwrap();
        let path = spec.config_path(&home);
        assert!(!path.exists(), "前置：配置文件应不存在");

        // 复刻命令层逻辑：先补父目录，再走引擎。
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let engine = ConfigEngine::new(
            FsSnapshotStore::new(&mcpane_home),
            JsonlHistoryStore::new(&mcpane_home),
            RealFs,
        );
        let loaded = engine.load(&path, ConfigFormat::Json).unwrap();
        let outcome = engine
            .apply(
                &loaded,
                &spec.hub_entry_patch(None, CredentialDelivery::Inline("tok-first")),
                "test 接入",
                Some(&|doc| spec.validate_hub_entry(doc, CredentialMode::Inline)),
                None,
            )
            .unwrap();
        assert!(outcome.written, "应实际写入");

        // 再读回：结构正确、含明文 Bearer。
        let reloaded = engine.load(&path, ConfigFormat::Json).unwrap();
        assert!(reloaded.content.contains("mcpServers"));
        assert!(reloaded.content.contains("Bearer tok-first"));
        assert!(reloaded.content.contains("127.0.0.1"));

        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn has_hub_entry_reflects_connect_state() {
        let spec = spec("cursor").unwrap();
        let empty = ConfigDocument::parse("{}", ConfigFormat::Json).unwrap();
        assert!(!spec.has_hub_entry(&empty));
        assert!(spec.has_hub_entry(&apply_to_empty("cursor", CredentialDelivery::Inline("t"))));
    }

    #[test]
    fn remove_keeps_other_servers() {
        let spec = spec("cursor").unwrap();
        let mut doc = ConfigDocument::parse(
            r#"{"mcpServers":{"mcpane":{"url":"x"},"other":{"url":"y"}}}"#,
            ConfigFormat::Json,
        )
        .unwrap();
        spec.remove_hub_entry_patch().apply(&mut doc).unwrap();
        assert!(doc.get(&["mcpServers", "mcpane"]).is_none());
        assert!(doc.get(&["mcpServers", "other"]).is_some());
    }
}
