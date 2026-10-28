//! GUI 进程 PATH 补齐：Dock/Finder 启动的 App 通常只有系统路径，
//! Homebrew / nvm / npm 全局 bin 全找不到——Hub sidecar 与 stdio 下游
//! （`npx`/`uvx`）以及客户端安装探测都会踩坑。
//!
//! 不改进程自身环境（`set_var` 在 edition 2024 需 unsafe，本 workspace 禁用）；
//! 调用方把 [`augment_path`] 的结果写入子进程 / sidecar 的 `PATH` 即可。

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

/// 常见包管理器的 bin 目录清单（含可能尚不存在的路径；调用方按需过滤）。
pub fn common_cli_bin_dirs() -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    // Windows：`pnpm setup` 默认把 shim 放到 `%LOCALAPPDATA%\pnpm`，
    // 不是 macOS 的 `~/Library/pnpm`，也不是 `%APPDATA%\npm`。
    let windows_pnpm = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("AppData/Local"))
        .join("pnpm");
    let mut dirs: Vec<PathBuf> = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        home.join(".local/bin"),
        home.join(".bun/bin"),
        home.join(".volta/bin"),
        home.join(".npm-global/bin"),
        home.join("Library/pnpm"),
        windows_pnpm,
        // 原生安装排在 npm 全局之前：更新过的原生装法不该被 `%APPDATA%\npm`
        // 里那个过期 shim 遮住（"更新了但还显示旧版本"）。
        home.join("AppData/Local/Programs/claude"), // winget Anthropic.ClaudeCode
        home.join("AppData/Local/Programs/OpenAI/Codex/bin"), // Codex 独立安装器
        home.join("AppData/Roaming/npm"),           // Windows npm 全局
    ];
    // nvm / fnm 每个 Node 版本一个 bin 目录，逐个展开（目录数是版本数，很小）。
    for versions in [
        home.join(".nvm/versions/node"),
        home.join(".fnm/node-versions"),
    ] {
        let Ok(entries) = std::fs::read_dir(&versions) else {
            continue;
        };
        for entry in entries.flatten() {
            // fnm 的布局多一层 `installation`。
            dirs.push(entry.path().join("bin"));
            dirs.push(entry.path().join("installation/bin"));
        }
    }
    // Hermes 是 pip 装的：用户级安装落在 Python 版本目录下，各版本一个脚本目录。
    for (base, bin) in [
        (home.join("Library/Python"), "bin"),
        (home.join("AppData/Roaming/Python"), "Scripts"),
    ] {
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue;
        };
        for entry in entries.flatten() {
            dirs.push(entry.path().join(bin));
        }
    }
    dirs
}

/// 把**已存在**的常见 bin 目录前置进 PATH，已在 PATH 中的不重复。
///
/// `current` 为 `None` 时等同于空 PATH，只返回补齐段。
pub fn augment_path(current: Option<&OsStr>) -> OsString {
    let existing: Vec<PathBuf> = current
        .map(|path| std::env::split_paths(path).collect())
        .unwrap_or_default();
    let mut prepended = Vec::new();
    for dir in common_cli_bin_dirs() {
        if !dir.is_dir() {
            continue;
        }
        if existing.iter().any(|p| p == &dir) || prepended.iter().any(|p: &PathBuf| p == &dir) {
            continue;
        }
        prepended.push(dir);
    }
    let merged: Vec<PathBuf> = prepended.into_iter().chain(existing).collect();
    std::env::join_paths(&merged).unwrap_or_else(|_| {
        // join 失败（路径含 NUL）极少见：回退到原始 PATH，别把进程弄瘫。
        current.map(OsString::from).unwrap_or_default()
    })
}

/// 当前进程 PATH 补齐后的值（给 sidecar / 子进程 `.env("PATH", …)` 用）。
///
/// 基线是 [`effective_search_paths`] 而非裸进程 PATH：自更新重启的进程丢了
/// 用户 PATH 时，node 所在目录也一起丢了，npm 装的 CLI 会以 127 起不来。
pub fn augmented_path_from_env() -> OsString {
    let process_path = std::env::var_os("PATH");
    let effective = std::env::join_paths(effective_search_paths()).ok();
    augment_path(effective.as_deref().or(process_path.as_deref()))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// 有效搜索目录：进程 PATH 在前，Windows 上再并入注册表里的用户/机器 PATH。
///
/// 应用内自更新后重启的进程只继承机器 PATH、丢掉用户 PATH，装在用户 PATH 上的
/// CLI 于是显示"未安装"，非得从开始菜单完整重启才回来；注册表里那两个 Path
/// 值才是一个新登录 shell 看到的东西。进程段保持在前，运行时覆盖照样生效。
pub fn effective_search_paths() -> Vec<PathBuf> {
    let process = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&process).collect();
    dirs.extend(registry_path_dirs());
    dedup_paths(dirs)
}

#[cfg(windows)]
fn registry_path_dirs() -> Vec<PathBuf> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    // 用户在前、机器在后，与 Windows 自己拼 PATH 的顺序一致。
    [
        (HKEY_CURRENT_USER, r"Environment"),
        (
            HKEY_LOCAL_MACHINE,
            r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
        ),
    ]
    .iter()
    .filter_map(|(root, sub)| {
        RegKey::predef(*root)
            .open_subkey(sub)
            .ok()?
            .get_value::<String, _>("Path")
            .ok()
    })
    // 注册表的 Path 是 REG_EXPAND_SZ，取回来的字符串不展开 %VAR%。
    .flat_map(|raw| split_windows_path(&expand_env_vars(&raw)))
    .collect()
}

#[cfg(not(windows))]
fn registry_path_dirs() -> Vec<PathBuf> {
    Vec::new()
}

/// 切分 Windows 风格的 `;` 分隔 PATH（去空段、剥两端引号）。
// 别处平台只有测试调用：纯字符串逻辑不该只能在 Windows 上验证。
#[cfg_attr(not(windows), allow(dead_code))]
fn split_windows_path(raw: &str) -> Vec<PathBuf> {
    raw.split(';')
        .map(|segment| segment.trim().trim_matches('"'))
        .filter(|segment| !segment.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// 展开 `%VAR%` 引用；变量未定义时原样保留，不吞字符。
#[cfg_attr(not(windows), allow(dead_code))]
fn expand_env_vars(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(open) = rest.find('%') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('%') else {
            // 落单的 `%`：后面没有配对符号，整段原样带走。
            out.push('%');
            out.push_str(after);
            return out;
        };
        let name = &after[..close];
        let value = (!name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '(' || c == ')'))
        .then(|| std::env::var(name).ok())
        .flatten();
        match value {
            Some(value) => out.push_str(&value),
            None => {
                out.push('%');
                out.push_str(name);
                out.push('%');
            }
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// 保序去重（Windows 路径大小写不敏感，按小写比对）。
fn dedup_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    paths
        .into_iter()
        .filter(|path| {
            let key = if cfg!(windows) {
                path.to_string_lossy().to_ascii_lowercase()
            } else {
                path.to_string_lossy().into_owned()
            };
            !key.is_empty() && seen.insert(key)
        })
        .collect()
}

/// 在常见 bin 目录中找可执行文件（不查进程 PATH）。
fn find_in_common_cli_bin_dirs(name: &str) -> Option<PathBuf> {
    common_cli_bin_dirs()
        .iter()
        .find_map(|dir| executable_in(dir, name))
}

/// 常见 bin 目录中的全部命中（顺序即查找顺序）。
fn find_all_in_common_cli_bin_dirs(name: &str) -> Vec<PathBuf> {
    common_cli_bin_dirs()
        .iter()
        .filter_map(|dir| executable_in(dir, name))
        .collect()
}

/// Windows 上可执行文件的探测次序（PATHEXT 里真正相关的那几个）。
///
/// 次序照抄 PATHEXT 的默认值（`.COM;.EXE;.BAT;.CMD;...`）去掉不相关的那几个，
/// 同名多份时挑中的才是 cmd.exe 会执行的那份——自己排一版就是在猜。
///
/// 这份次序是共享的：hub 启动 stdio 下游时也要把 `npx` 补成 `npx.cmd`，
/// 两边各存一份就会漂移——本地环境面板误报「跑不起来」正是这么来的，
/// 下游那边早已按扩展名优先解析，这边却还先试无扩展名。
pub const WINDOWS_EXECUTABLE_EXTS: &[&str] = &["exe", "bat", "cmd"];

fn executable_in(dir: &Path, name: &str) -> Option<PathBuf> {
    if cfg!(windows) && is_app_execution_alias_dir(dir) {
        return None;
    }
    executable_candidates(dir, name, cfg!(windows))
        .into_iter()
        .find(|path| is_real_executable(path))
}

/// 目录内的候选可执行文件，按**该平台实际会执行到的那份**排序。
///
/// Windows 上带扩展名的必须排在无扩展名之前：npm 全局安装一次留下三个文件
/// （`codex` 是给 Git Bash 的 Unix sh 脚本、`codex.cmd`、`codex.ps1`），而
/// CreateProcess 起不动那个 sh 脚本——挑中它就会把好好的 CLI 报成「跑不起来」。
/// 扩展名之间照 [`WINDOWS_EXECUTABLE_EXTS`] 的次序试，报出来的才是用户在终端里
/// 真正跑到的那份。无扩展名留作兜底，免得真是无扩展名 PE 时变成"未安装"。
///
/// `windows` 由调用方传入而非直接读 `cfg!`：纯路径拼接逻辑不该只能在 Windows
/// 上验证（同 [`split_windows_path`]）。
fn executable_candidates(dir: &Path, name: &str, windows: bool) -> Vec<PathBuf> {
    if !windows {
        return vec![dir.join(name)];
    }
    WINDOWS_EXECUTABLE_EXTS
        .iter()
        .map(|ext| dir.join(format!("{name}.{ext}")))
        .chain(std::iter::once(dir.join(name)))
        .collect()
}

/// 跨平台 which（避免引入额外依赖；Windows 上尝试 PATHEXT 常见扩展）。
pub fn which(name: &str) -> Option<PathBuf> {
    which_in(effective_search_paths().into_iter(), name)
}

/// [`which`] 的可测内核：搜索目录由外部注入（改进程 PATH 需 unsafe，本工作区禁用）。
fn which_in(dirs: impl Iterator<Item = PathBuf>, name: &str) -> Option<PathBuf> {
    dirs.into_iter().find_map(|dir| executable_in(&dir, name))
}

/// 该 CLI 能找到的**全部**安装位置（PATH 命中在前，常见 bin 目录兜底，已去重）。
///
/// 供"装了好几份"的冲突诊断用：诊断必须与 [`resolve_cli`] 看同一组位置，
/// 只扫常见目录会漏掉 PATH 上那份——而那份往往正是实际被调用的。
pub fn find_all_installations(name: &str) -> Vec<PathBuf> {
    let mut hits: Vec<PathBuf> = effective_search_paths()
        .iter()
        .filter_map(|dir| executable_in(dir, name))
        .collect();
    hits.extend(find_all_in_common_cli_bin_dirs(name));
    dedup_paths(hits)
}

/// CLI 可执行文件的绝对路径：**进程 PATH 优先，落空再查常见 bin 目录**。
///
/// 只查 PATH 是不够的：GUI 进程（Dock/Finder 启动）不走登录 shell，PATH 通常
/// 只有系统路径，nvm/volta/bun/homebrew 装的 CLI 一个都找不到——安装探测因此
/// 早有旁路，版本探测却没有，于是出现"已安装 v?"。
pub fn resolve_cli(name: &str) -> Option<PathBuf> {
    which(name).or_else(|| find_in_common_cli_bin_dirs(name))
}

/// 单个 CLI 客户端的安装检测结果（`crates/mcpane-adapter-*` 的 `XInstallation`
/// 都是它的别名——7 个客户端的探测逻辑完全一致，差异只在 CLI 名字）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliInstallation {
    pub binary_path: PathBuf,
    /// `--version` 输出解析出的版本号。
    pub version: Option<String>,
    /// `--version` 是否跑通（退出码 0）。
    ///
    /// 与 `version` 分开是有意的：**解析不出版本号 ≠ 跑不起来**。早先本地环境
    /// 面板拿"有没有版本号"当判据，遇到没见过的输出格式就报红「装了但跑不
    /// 起来」——CLI 其实好好的。
    pub ran_ok: bool,
    /// `ran_ok == false` 时的失败原因（跑通时为 `None`）。
    pub failure: Option<ProbeFailure>,
}

/// 只定位 `name` 对应的可执行文件，不跑 `--version`。
/// 判断"装没装"用它——启动路径上不该为此多 spawn 一个进程。
pub fn locate_cli(name: &str) -> Option<PathBuf> {
    resolve_cli(name)
}

/// 检测 `name` 对应的可执行文件与版本。返回 `None` 表示未安装。
pub fn detect_cli(name: &str) -> Option<CliInstallation> {
    let binary_path = resolve_cli(name)?;
    let probe = probe_cli_version(&binary_path);
    Some(CliInstallation {
        binary_path,
        version: probe.version,
        ran_ok: probe.ran_ok,
        failure: probe.failure,
    })
}

/// `<cli> --version` 的探测结果。
pub struct CliVersion {
    /// 命令跑通（退出码 0）。解析不出版本号 ≠ 跑不起来，故与 `version` 分开。
    pub ran_ok: bool,
    /// 解析出的语义化版本号。
    pub version: Option<String>,
    /// 没跑通时的原因（跑通时为 `None`）。
    pub failure: Option<ProbeFailure>,
}

/// `--version` 探测失败的原因。
///
/// 只报一个 `ran_ok: false` 是把诊断当结论：界面除了说"跑不起来"再没别的可说，
/// 用户不知道哪坏了、也不知道下一步做什么。三种失败的补救动作完全不同，
/// 所以分开上报，由界面翻译成各自的下一步。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ProbeFailure {
    /// 进程根本没起来：Windows 上多是挑中了系统执行不了的文件，Unix 上多是缺执行位。
    Spawn { detail: String },
    /// 到点仍未退出，已被击杀。
    Timeout { seconds: u64 },
    /// 跑起来了但非零退出，`stderr` 为末几行原文（可能为空）。
    Exit { code: Option<i32>, stderr: String },
}

/// 诊断里保留的 stderr 行数。够定位原因，又不至于把整屏堆栈搬进面板。
const DIAGNOSTIC_TAIL_LINES: usize = 4;

/// 单次 `--version` 探测的时间预算。客户端是逐个串行探的，任一挂住（npm shim
/// 抢锁、脚本等 stdin、nvm shim 指向已删除的 node）就会冻住整轮检测；到点即杀、
/// 该客户端按探测失败降级，其余照常。
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// 等子进程退出的轮询间隔。
const PROBE_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// 进程退出后排空 stdout 的预算。数据此时已经写完，读完只是一瞬；等不到就是
/// 管道被它留下的后台进程占着，再等也不会来。
const PIPE_DRAIN_GRACE: Duration = Duration::from_secs(1);

/// 跑一次 `<bin> --version` 并解析版本号。
///
/// 子进程 PATH 必须补齐：npm 装的 CLI 多是 Node 脚本（`codex` 即是），瘦 PATH
/// 下会以 `env: node: No such file or directory` 退出 127——那不是"装坏了"，
/// 只是我们没把 node 所在目录传下去。
pub fn probe_cli_version(bin: &Path) -> CliVersion {
    probe_cli_version_within(bin, VERSION_PROBE_TIMEOUT)
}

fn probe_cli_version_within(bin: &Path, timeout: Duration) -> CliVersion {
    let mut command = std::process::Command::new(bin);
    command
        .arg("--version")
        .env("PATH", augmented_path_from_env())
        // stdin 置空：等输入的 CLI 会一直挂着，直到我们超时把它杀掉。
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    suppress_console_window(&mut command);

    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return CliVersion {
                ran_ok: false,
                version: None,
                failure: Some(ProbeFailure::Spawn {
                    detail: err.to_string(),
                }),
            };
        }
    };
    let Some((status, stdout, stderr)) = wait_capped(child, timeout) else {
        return CliVersion {
            ran_ok: false,
            version: None,
            failure: Some(ProbeFailure::Timeout {
                seconds: timeout.as_secs(),
            }),
        };
    };
    if !status.success() {
        return CliVersion {
            ran_ok: false,
            version: None,
            failure: Some(ProbeFailure::Exit {
                code: status.code(),
                // 报错通常写在 stderr，但也有 CLI 只往 stdout 写；两个都空就留空，
                // 界面那时只报退出码——瞎猜一句原因比不说更误导。
                stderr: tail_lines(&diagnostic_text(&stderr, &stdout), DIAGNOSTIC_TAIL_LINES),
            }),
        };
    }
    CliVersion {
        ran_ok: true,
        version: parse_version(&mcpane_config::process_output::decode(&stdout)),
        failure: None,
    }
}

/// 诊断正文：优先 stderr，为空则退到 stdout。
fn diagnostic_text(stderr: &[u8], stdout: &[u8]) -> String {
    let err = mcpane_config::process_output::decode(stderr);
    if err.trim().is_empty() {
        mcpane_config::process_output::decode(stdout)
    } else {
        err
    }
}

/// 取文本末尾 `n` 行（已去掉首尾空白）。
fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.trim().lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// 等子进程结束并收走 stdout / stderr；超时或等待出错返回 `None`。
fn wait_capped(
    mut child: std::process::Child,
    timeout: Duration,
) -> Option<(std::process::ExitStatus, Vec<u8>, Vec<u8>)> {
    // 两个管道各起一个读线程：任一管道写满都会把子进程堵在 write 上，
    // 边等边读才不会把"输出略多"变成假超时。
    let stdout = spawn_reader(child.stdout.take());
    let stderr = spawn_reader(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(PROBE_POLL_INTERVAL),
            // 超时（或 try_wait 出错）：击杀后直接放手，读线程留给它自己了结。
            _ => {
                terminate_probe(&mut child);
                let _ = child.wait();
                return None;
            }
        }
    };
    // 进程退了不等于管道到了 EOF：它 spawn 的后台进程可能还攥着写端，死等就是
    // 把"子进程挂死"原样换成"我们挂死"。收不到就当没有输出，退出码照样有效。
    // 两个管道共享同一份宽限期，别因为多收一路 stderr 就把最坏耗时翻倍。
    let deadline = Instant::now() + PIPE_DRAIN_GRACE;
    Some((status, drain(stdout, deadline), drain(stderr, deadline)))
}

/// 在 `deadline` 前收走读线程的结果，收不到就当没有输出。
fn drain(rx: Option<std::sync::mpsc::Receiver<Vec<u8>>>, deadline: Instant) -> Vec<u8> {
    rx.and_then(|rx| {
        rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .ok()
    })
    .unwrap_or_default()
}

/// 起线程把管道读干，结果经 channel 回传——`JoinHandle::join` 没有带超时的版本。
fn spawn_reader<R>(pipe: Option<R>) -> Option<std::sync::mpsc::Receiver<Vec<u8>>>
where
    R: std::io::Read + Send + 'static,
{
    let mut pipe = pipe?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    Some(rx)
}

/// 终止超时的探测进程。
///
/// Windows 走 `taskkill /T`：CLI 多是 `.cmd` shim 再拉起真正的进程，只杀 shim
/// 会留下孤儿。Unix 只杀直接子进程——整组击杀要 libc + unsafe（本工作区禁用
/// unsafe），而 `--version` 极少产生长命孙进程，读线程也已经放手了。
fn terminate_probe(child: &mut std::process::Child) {
    #[cfg(windows)]
    {
        let mut kill = std::process::Command::new("taskkill");
        kill.args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        suppress_console_window(&mut kill);
        let _ = kill.status();
    }
    let _ = child.kill();
}

/// 从版本输出提取语义化版本号（如 "2.1.207 (Claude Code)" → "2.1.207"）。
pub fn parse_version(output: &str) -> Option<String> {
    output
        .split_whitespace()
        // 常见输出里版本号可能带 v 前缀（`hermes v1.2.3`）或跟着逗号
        // （click 风格的 `hermes, version 1.2.3`），都要能认出来。
        .map(|token| {
            token
                .trim_start_matches(['v', 'V'])
                .trim_matches(|c: char| !c.is_ascii_alphanumeric())
        })
        .find(|token| {
            let mut parts = token.split('.');
            parts.clone().count() >= 2
                && parts.all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        })
        .map(str::to_owned)
}

/// 存在且非空。用于排除 Windows 的 App Execution Alias 垫片：
/// `%LOCALAPPDATA%\Microsoft\WindowsApps` 默认在 PATH 里，其中的 Store 别名
/// （系统自带 `python.exe` 等）是 0 字节文件，`is_file()` 却认它，选中后执行
/// 弹出的是 Store 安装页。真实可执行文件不可能是 0 字节。
fn is_real_executable(path: &Path) -> bool {
    path.metadata().is_ok_and(|m| m.is_file() && m.len() > 0)
}

/// 整个 WindowsApps 目录直接跳过：别名是 reparse point，未必都是 0 字节，
/// 但没有一个是我们能 `--version` 探测的真实 CLI。
fn is_app_execution_alias_dir(dir: &Path) -> bool {
    let normalized = dir
        .to_string_lossy()
        .to_ascii_lowercase()
        .replace('\\', "/");
    normalized
        .trim_end_matches('/')
        .ends_with("microsoft/windowsapps")
}

/// Windows 上抑制控制台窗口。桌面端是 GUI 进程（无控制台），再 spawn 控制台
/// 子程序时系统会新建一个窗口——逐个探测客户端就会连闪好几个黑框。
fn suppress_console_window(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW，std 直接支持，无需引入 windows crate。
        command.creation_flags(0x0800_0000);
    }
    #[cfg(not(windows))]
    let _ = command;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn augment_path_keeps_existing_and_skips_missing_dirs() {
        let original = std::env::join_paths([Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
        let out = augment_path(Some(&original));
        let parts: Vec<PathBuf> = std::env::split_paths(&out).collect();
        assert!(parts.iter().any(|p| p == Path::new("/usr/bin")));
        assert!(parts.iter().any(|p| p == Path::new("/bin")));
        for dir in common_cli_bin_dirs() {
            if !dir.is_dir() {
                assert!(
                    !parts.contains(&dir),
                    "不存在的目录不该进 PATH: {}",
                    dir.display()
                );
            }
        }
    }

    #[test]
    fn common_cli_bin_dirs_includes_windows_pnpm_shim_dir() {
        let Some(home) = home_dir() else {
            return;
        };
        let expected = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Local"))
            .join("pnpm");
        let dirs = common_cli_bin_dirs();
        assert!(
            dirs.iter().any(|p| p == &expected),
            "GUI PATH 补齐必须含 pnpm setup 的默认 shim 目录 {expected:?}，实际 {dirs:?}"
        );
    }

    #[test]
    fn find_missing_cli_does_not_panic() {
        assert!(find_in_common_cli_bin_dirs("definitely-not-a-real-cli-42").is_none());
        assert!(resolve_cli("definitely-not-a-real-cli-42").is_none());
        assert!(find_all_installations("definitely-not-a-real-cli-42").is_empty());
    }

    /// 冲突诊断列的位置必须包含实际会被调用的那份，且不重复。
    #[test]
    fn all_installations_contain_the_resolved_one() {
        // `sh` 在 PATH 上必然存在，且常见 bin 目录里可能另有一份。
        let Some(resolved) = resolve_cli("sh") else {
            return;
        };
        let all = find_all_installations("sh");
        assert!(all.contains(&resolved), "漏掉实际调用的那份: {all:?}");
        let mut deduped = all.clone();
        deduped.dedup();
        assert_eq!(deduped.len(), all.len(), "同一路径不该列两次");
    }

    #[test]
    fn version_parsing_handles_common_formats() {
        assert_eq!(
            parse_version("2.1.207 (Claude Code)").as_deref(),
            Some("2.1.207")
        );
        assert_eq!(parse_version("claude 1.0.0"), Some("1.0.0".to_owned()));
        assert_eq!(parse_version("hermes v1.2.3"), Some("1.2.3".to_owned()));
        assert_eq!(parse_version("no version here"), None);
    }

    /// Windows 的 App Execution Alias 垫片是 0 字节，`is_file()` 却认它——
    /// 选中它之后执行会弹 Microsoft Store 页而不是运行程序。
    #[test]
    fn zero_byte_app_execution_alias_is_not_executable() {
        let dir = tempfile::tempdir().unwrap();

        let stub = dir.path().join("python.exe");
        std::fs::write(&stub, b"").unwrap();
        assert!(
            stub.is_file(),
            "前提：0 字节垫片对 is_file() 就是返回 true，所以才需要额外判大小"
        );
        assert!(
            !is_real_executable(&stub),
            "0 字节的 Store 别名垫片不得被当成可执行文件"
        );

        let real = dir.path().join("node.exe");
        std::fs::write(&real, b"MZ").unwrap();
        assert!(is_real_executable(&real), "非空可执行文件必须被认出");

        assert!(!is_real_executable(&dir.path().join("absent.exe")));
        assert!(!is_real_executable(dir.path()), "目录不是可执行文件");
    }

    /// 版本探测必须给子进程补齐 PATH：npm 装的 CLI 是 Node 脚本，
    /// GUI 进程那份瘦 PATH 下会以「node 找不到」退出 127。
    #[test]
    fn version_probe_passes_augmented_path_to_child() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-cli");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&script, "#!/bin/sh\necho \"fake 9.8.7\"\n").unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            let probe = probe_cli_version(&script);
            assert!(probe.ran_ok);
            assert_eq!(probe.version.as_deref(), Some("9.8.7"));
        }
        #[cfg(not(unix))]
        let _ = script;
    }

    /// 挂死的 `--version` 不得拖住整轮探测：到点击杀，按探测失败降级。
    #[cfg(unix)]
    #[test]
    fn hung_version_probe_is_killed_at_deadline() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("hung-cli");
        std::fs::write(&script, "#!/bin/sh\nsleep 60\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let started = Instant::now();
        let probe = probe_cli_version_within(&script, Duration::from_millis(300));
        let elapsed = started.elapsed();

        assert!(!probe.ran_ok, "超时的探测不能报成跑通");
        assert!(probe.version.is_none());
        assert!(
            elapsed < Duration::from_secs(10),
            "必须在预算附近返回，实际耗时 {elapsed:?}"
        );
    }

    /// 子进程退了、它留下的后台进程还攥着 stdout：不能因此卡住，
    /// 退出码照样有效（拿不到版本号只是降级，不是挂死）。
    #[cfg(unix)]
    #[test]
    fn leaked_pipe_after_exit_does_not_block() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("leaky-cli");
        // `sleep 30 &` 继承 stdout：父进程 echo 完就退，管道却一直不 EOF。
        std::fs::write(&script, "#!/bin/sh\nsleep 30 &\necho \"leaky 1.2.3\"\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let started = Instant::now();
        let probe = probe_cli_version_within(&script, Duration::from_secs(30));
        let elapsed = started.elapsed();

        assert!(probe.ran_ok, "进程正常退出，退出码应照常采信");
        assert!(
            elapsed < Duration::from_secs(10),
            "不得等到孙进程放开管道，实际耗时 {elapsed:?}"
        );
    }

    /// 读 stdin 的 CLI 同样不能把探测挂住（stdin 置空 → 立刻 EOF）。
    #[cfg(unix)]
    #[test]
    fn version_probe_closes_stdin() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("stdin-cli");
        std::fs::write(
            &script,
            "#!/bin/sh\ncat > /dev/null\necho \"stdin 1.0.0\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let probe = probe_cli_version_within(&script, Duration::from_secs(5));
        assert!(probe.ran_ok, "stdin 应立刻 EOF，命令要能正常退出");
        assert_eq!(probe.version.as_deref(), Some("1.0.0"));
    }

    #[test]
    fn env_vars_expand_and_survive_unknown_names() {
        // PATH 在测试进程里必然存在（改环境变量需 unsafe，只能用现成的）。
        let path = std::env::var("PATH").unwrap();
        assert_eq!(expand_env_vars("%PATH%"), path);
        assert_eq!(expand_env_vars("a;%PATH%;b"), format!("a;{path};b"));
        assert_eq!(
            expand_env_vars(r"C:\bin;%MCPANE_UNSET_VAR_42%\x"),
            r"C:\bin;%MCPANE_UNSET_VAR_42%\x",
            "未定义的变量原样保留，不得吞掉路径段"
        );
        assert_eq!(
            expand_env_vars("100% done"),
            "100% done",
            "落单的 % 不得吞掉后面的字符"
        );
    }

    #[test]
    fn windows_path_string_splits_and_dedups() {
        let dirs = split_windows_path(r#"C:\a; ;"C:\b" ;C:\c;"#);
        assert_eq!(
            dirs,
            vec![
                PathBuf::from(r"C:\a"),
                PathBuf::from(r"C:\b"),
                PathBuf::from(r"C:\c"),
            ]
        );

        let deduped = dedup_paths(vec![
            PathBuf::from("/usr/bin"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/usr/bin"),
        ]);
        assert_eq!(
            deduped,
            vec![PathBuf::from("/usr/bin"), PathBuf::from("/usr/local/bin")],
            "保序去重，先出现的胜出"
        );
    }

    /// 进程 PATH 里的目录必须原样出现在有效搜索路径里（注册表段只做补充）。
    #[test]
    fn effective_search_paths_cover_process_path() {
        let process: Vec<PathBuf> =
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
        let effective = effective_search_paths();
        for dir in process {
            if dir.as_os_str().is_empty() {
                continue;
            }
            assert!(
                effective.contains(&dir),
                "进程 PATH 段丢失: {}",
                dir.display()
            );
        }
    }

    /// WindowsApps 里全是 Store 别名，不能当作 CLI 的安装位置。
    #[test]
    fn windows_apps_alias_dir_is_skipped() {
        assert!(is_app_execution_alias_dir(Path::new(
            r"C:\Users\me\AppData\Local\Microsoft\WindowsApps"
        )));
        assert!(is_app_execution_alias_dir(Path::new(
            "C:/Users/me/AppData/Local/Microsoft/WindowsApps/"
        )));
        assert!(!is_app_execution_alias_dir(Path::new(
            r"C:\Users\me\AppData\Roaming\npm"
        )));
    }

    /// Windows 上带扩展名的必须排在无扩展名之前：npm 全局装一次会同时留下
    /// `codex`（Git Bash 用的 Unix sh 脚本）和 `codex.cmd`，挑中前者会被
    /// CreateProcess 拒掉，界面就把好好的 CLI 报成「跑不起来」。
    #[test]
    fn windows_candidates_try_extensions_before_bare_name() {
        let dir = Path::new(r"C:\tools");
        assert_eq!(
            executable_candidates(dir, "codex", true),
            vec![
                dir.join("codex.exe"),
                dir.join("codex.bat"),
                dir.join("codex.cmd"),
                dir.join("codex"),
            ]
        );
    }

    /// 无扩展名只是降级，不是删除：真是无扩展名 PE 时不该变成"未检测到"。
    #[test]
    fn windows_candidates_still_end_with_bare_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("codex"), b"MZ").unwrap();
        assert_eq!(
            executable_candidates(dir.path(), "codex", true)
                .into_iter()
                .find(|p| is_real_executable(p)),
            Some(dir.path().join("codex"))
        );
    }

    /// 同目录两份都在时选 `.cmd`，不选那个跑不起来的 Unix shim。
    #[test]
    fn windows_candidates_prefer_cmd_over_npm_unix_shim() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("codex"), b"#!/bin/sh\n").unwrap();
        std::fs::write(dir.path().join("codex.cmd"), b"@echo off\r\n").unwrap();
        assert_eq!(
            executable_candidates(dir.path(), "codex", true)
                .into_iter()
                .find(|p| is_real_executable(p)),
            Some(dir.path().join("codex.cmd"))
        );
    }

    /// 非 Windows 没有扩展名一说，只认裸名——别在 Unix 上凭空找 `.exe`。
    #[test]
    fn non_windows_candidates_are_bare_name_only() {
        assert_eq!(
            executable_candidates(Path::new("/usr/local/bin"), "codex", false),
            vec![PathBuf::from("/usr/local/bin/codex")]
        );
    }

    /// 起不来要报 Spawn，不能和"跑了但报错"混为一谈——两者的补救动作不同。
    #[test]
    fn missing_binary_reports_spawn_failure() {
        let dir = tempfile::tempdir().unwrap();
        let probe = probe_cli_version(&dir.path().join("nope"));
        assert!(!probe.ran_ok);
        assert!(matches!(probe.failure, Some(ProbeFailure::Spawn { .. })));
    }

    /// 非零退出要带上退出码与 stderr 原文，界面才有话可说。
    #[cfg(unix)]
    #[test]
    fn failing_binary_reports_exit_code_and_stderr() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("broken");
        std::fs::write(
            &script,
            "#!/bin/sh\necho 'boom: node not found' >&2\nexit 3\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let probe = probe_cli_version(&script);
        assert!(!probe.ran_ok);
        let Some(ProbeFailure::Exit { code, stderr }) = probe.failure else {
            panic!("非零退出应报 Exit");
        };
        assert_eq!(code, Some(3));
        assert_eq!(stderr, "boom: node not found");
    }

    /// 跑通就没有 failure，版本照常解析。
    #[cfg(unix)]
    #[test]
    fn working_binary_has_no_failure() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fine");
        std::fs::write(&script, "#!/bin/sh\necho 'codex-cli 1.2.3'\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let probe = probe_cli_version(&script);
        assert!(probe.ran_ok);
        assert!(probe.failure.is_none());
        assert_eq!(probe.version.as_deref(), Some("1.2.3"));
    }

    /// 诊断只留末几行：整屏堆栈搬进面板等于没诊断。
    #[test]
    fn tail_lines_keeps_only_the_last_lines() {
        assert_eq!(tail_lines("\na\nb\nc\nd\ne\n\n", 3), "c\nd\ne");
        assert_eq!(tail_lines("only", 4), "only");
        assert_eq!(tail_lines("   ", 4), "");
    }
}
