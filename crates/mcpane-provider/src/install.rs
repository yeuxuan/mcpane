//! 客户端安装探测：判断某个 app 在本机"装没装"。
//!
//! 两个用途，都要求判定**保守而准确**：
//! - 首启只为装了的客户端播种官方项（[`crate::ProviderService::seed_officials`]），
//!   没装的客户端不该凭空多出一条"官方订阅"；
//! - 供应商页对没装的客户端给安装引导，而不是显示一个切不动的官方卡片。
//!
//! 两级判定，逐级放宽：
//! 1. 可执行文件（`locate_*`，只查文件不跑 `--version`——判定"装没装"不值得在
//!    启动路径上 spawn 三个 Node 进程）。查的是 PATH **加**常见包管理器 bin 目录：
//!    GUI 进程的 PATH 通常只有系统路径（从 Finder / Dock 启动不走登录 shell），
//!    只认 PATH 会把 nvm/volta/bun 装的客户端说成没装；
//! 2. 该客户端**自己**的用户配置痕迹。选文件时刻意避开会被同厂其他工具创建的路径：
//!    `~/.gemini/settings.json` 会被 Antigravity 建出来，拿它判定就会把没装说成装了，
//!    所以 Gemini 只认 `oauth_creds.json` / `.env`。
//!
//! Claude Desktop 没有 CLI 入口，只看应用本体的安装位置（不是配置目录——那个
//! 目录 MCP 接入路径也会创建）。

use std::path::{Path, PathBuf};

use crate::domain::ClientApp;

/// 第 2 级判定用的"用过的痕迹"（相对 home），一律取各 adapter 认的那份配置文件：
/// - 只列**该客户端自己写**的，混入别家会建的（`~/.gemini/settings.json` 之于
///   Antigravity）就会把没装说成装了；
/// - 只列**文件**，不列数据目录。`~/.openclaw/agents` 这类会话目录卸载后仍在盘上，
///   拿它判定就是"卸载了还显示已安装"。
const CLAUDE_TRACES: &[&str] = &[".claude.json", ".claude/settings.json"];
const CODEX_TRACES: &[&str] = &[".codex/auth.json", ".codex/config.toml"];
const GEMINI_TRACES: &[&str] = &[".gemini/oauth_creds.json", ".gemini/.env"];
const GROK_TRACES: &[&str] = &[".grok/config.toml", ".grok/user-settings.json"];
const OPENCODE_TRACES: &[&str] = &[".config/opencode/opencode.json"];
const OPENCLAW_TRACES: &[&str] = &[".openclaw/openclaw.json"];
const HERMES_TRACES: &[&str] = &[".hermes/config.yaml"];

/// 该客户端是否已安装。配置目录被用户覆盖时用 [`is_installed_with_dir`]。
pub fn is_installed(app: ClientApp) -> bool {
    is_installed_with_dir(app, None)
}

/// 同上，但痕迹判定额外看一眼用户指定的配置目录。
pub fn is_installed_with_dir(app: ClientApp, config_dir: Option<&Path>) -> bool {
    let traces = |list: &[&str]| {
        any_trace_file(list) || config_dir.is_some_and(|dir| any_trace_file_in(dir, list))
    };
    match app {
        ClientApp::Claude => {
            mcpane_adapter_claude::locate_claude().is_some() || traces(CLAUDE_TRACES)
        }
        ClientApp::Codex => mcpane_adapter_codex::locate_codex().is_some() || traces(CODEX_TRACES),
        ClientApp::Gemini => {
            mcpane_adapter_gemini::locate_gemini().is_some() || traces(GEMINI_TRACES)
        }
        ClientApp::Grok => mcpane_adapter_grok::locate_grok().is_some() || traces(GROK_TRACES),
        ClientApp::OpenCode => {
            mcpane_adapter_opencode::locate_opencode().is_some() || traces(OPENCODE_TRACES)
        }
        ClientApp::OpenClaw => {
            mcpane_adapter_openclaw::locate_openclaw().is_some() || traces(OPENCLAW_TRACES)
        }
        ClientApp::Hermes => {
            mcpane_adapter_hermes::locate_hermes().is_some() || traces(HERMES_TRACES)
        }
        ClientApp::ClaudeDesktop => claude_desktop_installed(),
    }
}

/// 单个客户端的本地环境实况（设置页「本地环境」面板用）。
///
/// 与 [`is_installed`] 的区别：这里会跑一次 `--version`，慢（Node 启动）但信息全。
/// 只在用户打开设置页时按需调用，不进启动路径。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientEnv {
    pub app: ClientApp,
    pub installed: bool,
    /// `<cli> --version` 解析出的版本号；GUI 应用与解析失败为 None。
    pub version: Option<String>,
    /// 可执行文件 / 应用本体的绝对路径（"装在哪"比"装没装"更能定位问题）。
    pub path: Option<String>,
    /// 装了但 `--version` 跑不起来（npm 漏装平台二进制等半损坏状态）。
    /// 对齐 cc-switch 的 `installed_but_broken`：这类问题不提示的话，
    /// 用户只会看到客户端"莫名其妙用不了"。
    ///
    /// **只有真跑过 `--version` 才会为 false**：靠常见 bin 目录或配置痕迹认出来
    /// 的安装没跑过命令，一律按"能跑"处理——报错误的红比不报更糟。
    pub runnable: bool,
    /// `runnable == false` 时的失败原因，供界面给出对症的下一步（`runnable` 时为 None）。
    /// 只报一个布尔量等于把诊断当结论——界面除了说"跑不起来"就无话可说了。
    pub problem: Option<mcpane_hub_protocol::cli_path::ProbeFailure>,
    /// 同一个 CLI 的**其他**安装位置（不含 `path` 那份）。
    ///
    /// 场景：装了两份（比如 brew 一份、npm 全局一份）时，
    /// 用户升级的常常不是实际被调用的那份，然后困惑"明明更新了怎么没变"。
    /// 我们不猜哪份对，只把事实摆出来——`path` 是会被用到的那份，这里是其余的。
    pub conflicts: Vec<String>,
}

/// 全部客户端的本地环境（顺序同 [`ClientApp::ALL`]）。
pub fn client_envs() -> Vec<ClientEnv> {
    ClientApp::ALL.into_iter().map(client_env).collect()
}

/// 同上，但带上用户设置的配置目录覆盖（app → 目录）。
pub fn client_envs_with_dirs(
    dirs: &std::collections::BTreeMap<ClientApp, String>,
) -> Vec<ClientEnv> {
    // 逐个客户端要跑一次 `--version`（Node 启动几百毫秒，卡住的那个要耗满 10 秒
    // 预算）。串行会把这些时间叠起来——七个客户端最坏冻结一分多钟；并行后整轮
    // 只等最慢的一个。线程七条、命短，不值得引线程池。
    std::thread::scope(|scope| {
        let handles: Vec<_> = ClientApp::ALL
            .into_iter()
            .map(|app| scope.spawn(move || client_env_with_dir(app, dirs.get(&app).map(Path::new))))
            .collect();
        handles
            .into_iter()
            // panic 照原样往外抛，与串行时的行为一致。
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|e| std::panic::resume_unwind(e))
            })
            .collect()
    })
}

/// 单个客户端的本地环境。
pub fn client_env(app: ClientApp) -> ClientEnv {
    client_env_with_dir(app, None)
}

/// 同上，但痕迹判定看用户指定的配置目录。
pub fn client_env_with_dir(app: ClientApp, config_dir: Option<&Path>) -> ClientEnv {
    if app == ClientApp::ClaudeDesktop {
        let path = claude_desktop_path();
        return ClientEnv {
            app,
            installed: path.is_some(),
            version: None, // GUI 应用没有 --version，版本留空好过瞎猜
            runnable: true,
            problem: None,         // 没跑过命令，无从谈失败
            conflicts: Vec::new(), // GUI 应用只有一个安装位置，无从冲突
            path: path.map(|p| p.display().to_string()),
        };
    }
    // 七家的探测流程完全一致（各 adapter 的 detect_* 都是 cli_path::detect_cli 的
    // 别名），差异只在 CLI 名字；分支只负责选出这两样，解构统一在下面做。
    let (cli, detected) = match app {
        ClientApp::Claude => ("claude", mcpane_adapter_claude::detect_claude()),
        ClientApp::Codex => ("codex", mcpane_adapter_codex::detect_codex()),
        ClientApp::Gemini => ("gemini", mcpane_adapter_gemini::detect_gemini()),
        ClientApp::Grok => ("grok", mcpane_adapter_grok::detect_grok()),
        ClientApp::OpenCode => ("opencode", mcpane_adapter_opencode::detect_opencode()),
        ClientApp::OpenClaw => ("openclaw", mcpane_adapter_openclaw::detect_openclaw()),
        ClientApp::Hermes => ("hermes", mcpane_adapter_hermes::detect_hermes()),
        ClientApp::ClaudeDesktop => unreachable!("已在上面提前返回"),
    };
    let path = detected.as_ref().map(|i| i.binary_path.clone());
    let ran_ok = detected.as_ref().is_some_and(|i| i.ran_ok);
    let failure = detected.as_ref().and_then(|i| i.failure.clone());
    let version = detected.and_then(|i| i.version);
    // `ran_ok` 只在 detect **真的跑过** `--version` 时才有判定力。只靠配置痕迹
    // 认出来的安装没跑过命令，此时对可运行性一无所知——不能因为"没跑过"就报红。
    let probed = path.is_some();
    // 其余安装位置：把会被实际调用的那份（path）排掉，剩下的就是"还有几份"。
    let conflicts: Vec<String> = find_all_installations(cli)
        .into_iter()
        .filter(|candidate| path.as_ref() != Some(candidate))
        .map(|candidate| candidate.display().to_string())
        .collect();
    // 判据是 `--version` **跑没跑通（退出码）**，不是"解析出版本号"——
    // 没见过的输出格式只该让版本显示为未知，不该报「装了但跑不起来」。
    let runnable = !probed || ran_ok;
    ClientEnv {
        app,
        conflicts,
        installed: path.is_some() || is_installed_with_dir(app, config_dir),
        runnable,
        // 与 runnable 同源：没探测过就没有原因可说，别把"没跑过"渲染成故障。
        problem: if runnable { None } else { failure },
        version,
        path: path.map(|p| p.display().to_string()),
    }
}

/// 全部客户端的安装态（顺序同 [`ClientApp::ALL`]）。
pub fn installed_apps() -> Vec<ClientApp> {
    ClientApp::ALL
        .into_iter()
        .filter(|app| is_installed(*app))
        .collect()
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// 相对 home 的任一路径存在。
/// 任一痕迹文件存在。**`is_file` 而非 `exists`**：同名目录不算数（见 [`CLAUDE_TRACES`]）。
fn any_trace_file(relative: &[&str]) -> bool {
    let Some(home) = home_dir() else {
        return false;
    };
    relative.iter().any(|rel| home.join(rel).is_file())
}

/// 覆盖目录下的任一同名文件存在（痕迹判定的第二落点）。
///
/// 用户把配置搬到别处后，相对 home 的痕迹当然找不到——不看覆盖目录的话就会
/// 出现"设了目录反而说没装"。相对路径只取**文件名**：覆盖目录本身就相当于
/// `~/.claude` 那一层。
fn any_trace_file_in(dir: &Path, relative: &[&str]) -> bool {
    relative.iter().any(|rel| {
        Path::new(rel)
            .file_name()
            .is_some_and(|name| dir.join(name).is_file())
    })
}

/// 全部安装位置（诊断"装了好几份"用；顺序即查找顺序）。
fn find_all_installations(name: &str) -> Vec<PathBuf> {
    mcpane_hub_protocol::cli_path::find_all_installations(name)
}

fn claude_desktop_installed() -> bool {
    crate::claude_desktop::app_present()
}

/// Claude Desktop：找应用本体，不看用户数据目录——`~/Library/Application Support/Claude`
/// 由 MCP 接入等路径也可能创建，会误报成已安装。
fn claude_desktop_path() -> Option<PathBuf> {
    crate::claude_desktop::app_install_path()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// 没跑过 `--version` 的判定路径（旁路 bin 目录 / 配置痕迹）不该报"跑不起来"。
    /// 本机不一定装齐所有客户端，所以断言的是这条不变量而非具体某家的状态。
    #[test]
    fn unprobed_installs_are_never_reported_broken() {
        for env in client_envs() {
            if env.installed && env.version.is_none() && env.path.is_none() {
                assert!(
                    env.runnable,
                    "{:?} 是靠配置痕迹认出来的，没跑过命令就不能说它跑不起来",
                    env.app
                );
            }
        }
    }

    /// 并行探测不得打乱顺序：设置页「本地环境」按 `ClientApp::ALL` 的次序渲染。
    #[test]
    fn client_envs_keep_declaration_order() {
        let envs = client_envs_with_dirs(&std::collections::BTreeMap::new());
        let apps: Vec<ClientApp> = envs.iter().map(|env| env.app).collect();
        assert_eq!(apps, ClientApp::ALL.to_vec());
    }

    #[test]
    fn installed_apps_is_subset_of_all() {
        for app in installed_apps() {
            assert!(ClientApp::ALL.contains(&app));
        }
    }

    /// 回退判定只能认客户端自己写的文件：`~/.gemini/settings.json` / `config`
    /// 会被 Antigravity 创建，一旦纳入判定，没装 Gemini CLI 的机器就会被判成已装。
    #[test]
    fn gemini_traces_exclude_files_written_by_other_google_tools() {
        assert!(!GEMINI_TRACES.contains(&".gemini/settings.json"));
        assert!(!GEMINI_TRACES.contains(&".gemini/config"));
        assert!(GEMINI_TRACES.contains(&".gemini/oauth_creds.json"));
    }

    /// 痕迹路径必须落在各自客户端的目录里，不能互相串（复制粘贴回归）。
    #[test]
    fn traces_stay_within_their_own_client_dirs() {
        assert!(CLAUDE_TRACES.iter().all(|p| p.starts_with(".claude")));
        assert!(CODEX_TRACES.iter().all(|p| p.starts_with(".codex/")));
        assert!(GEMINI_TRACES.iter().all(|p| p.starts_with(".gemini/")));
        assert!(GROK_TRACES.iter().all(|p| p.starts_with(".grok/")));
        // OpenCode 走 XDG，痕迹不在 ~/.opencode（那是错的位置）。
        assert!(OPENCODE_TRACES.iter().all(|p| !p.starts_with(".opencode")));
    }

    /// 卸载残留的数据目录不算痕迹，否则卸载后会一直显示"已安装"。
    #[test]
    fn traces_exclude_leftover_data_dirs() {
        assert!(!OPENCLAW_TRACES.contains(&".openclaw/agents"));
        assert!(!OPENCODE_TRACES.contains(&".local/share/opencode"));
        assert!(!HERMES_TRACES.contains(&".hermes/memories"));
    }

    /// 清单之外再兜一层：判定本身只认文件。
    #[test]
    fn trace_lookup_ignores_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("openclaw.json")).unwrap();
        assert!(
            !any_trace_file_in(dir.path(), &[".openclaw/openclaw.json"]),
            "同名目录不是配置文件，不该判成已安装"
        );
    }
}
