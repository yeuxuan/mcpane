//! envsync：MCPane 代管环境变量持久化（ADR-016）。
//!
//! env 凭据模式一键化的支撑层——接入/轮换时把 Token 自动持久化为
//! 客户端可见的环境变量，用户零手动步骤：
//! - Unix：`$MCPANE_HOME/env.sh`（0600，唯一事实源）+ shell rc 注入一行
//!   带 `# mcpane:env` 标记的 source（CLI 覆盖）；
//! - macOS GUI：`launchctl setenv` 即时生效 + LaunchAgent 登录重放；
//! - Windows：HKCU\Environment 直写 + WM_SETTINGCHANGE 广播；
//! - Linux GUI：`~/.config/environment.d`（注销重登生效）。
//!
//! 失败分级：凭据存储写入失败 = Err（致命，调用方不得继续写客户端配置）；
//! rc 注入 / 会话同步 / LaunchAgent 失败 = 降级进 `warnings`（凭据没丢，
//! 附手动兜底指引）。

pub mod conflict;
mod rc;
mod session;
mod store;

use std::path::{Path, PathBuf};

use session::{AgentState, SessionSync};

/// 编排按 OS 分派（参数化而非 cfg 内联，让分派逻辑本身可在任意宿主上单测）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum HostOs {
    MacOs,
    Linux,
    Windows,
}

fn host_os() -> HostOs {
    if cfg!(target_os = "macos") {
        HostOs::MacOs
    } else if cfg!(windows) {
        HostOs::Windows
    } else {
        HostOs::Linux
    }
}

/// 路径与 shell 上下文：核心函数不读全局 env，全部由此注入（可测）。
struct EnvPaths {
    mcpane_home: PathBuf,
    user_home: PathBuf,
    /// `$SHELL` 的 basename（zsh/bash/fish/…）。
    shell: Option<String>,
}

/// 持久化结果报告：字段直接喂给前端 next_steps 文案。
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvSyncReport {
    /// 凭据落点（env.sh 路径 / `HKCU\Environment`）。
    pub credential_store: String,
    /// 注入或更新过的 rc / fish conf 文件。
    pub rc_files: Vec<String>,
    /// GUI 会话状态：`synced_now` | `synced_at_login` | `not_applicable`。
    pub gui_session: String,
    /// LaunchAgent：`installed` | `updated` | `unchanged` | `failed` | `not_applicable`。
    pub persistence_agent: String,
    /// 降级项的人话解释（可直接拼进 next_steps）。
    pub warnings: Vec<String>,
}

impl EnvSyncReport {
    fn new(credential_store: String) -> Self {
        Self {
            credential_store,
            rc_files: Vec::new(),
            gui_session: "not_applicable".to_owned(),
            persistence_agent: "not_applicable".to_owned(),
            warnings: Vec::new(),
        }
    }

    /// 接入结果视图的收尾指引（env 模式与 inline 对齐：唯一手动步骤=重启）。
    /// `terminal_client`：从终端启动的 CLI（Claude Code / Codex / Gemini CLI），
    /// 环境变量随 shell 启动快照，重启必须发生在新开的终端里。
    pub fn next_steps(&self, client_display: &str, terminal_client: bool) -> Vec<String> {
        let mut steps = vec![format!(
            "Token 已自动发放并保存到 {}（仅本机当前用户可读）",
            self.credential_store
        )];
        if !self.rc_files.is_empty() {
            steps.push(format!(
                "已写入 {}，新打开的终端自动生效；已打开的终端需重新打开",
                self.rc_files.join("、")
            ));
        }
        match self.gui_session.as_str() {
            "synced_now" => {
                steps.push("GUI 应用环境已同步，之后启动的应用即可读取".to_owned());
            }
            "synced_at_login" => {
                steps.push("GUI 客户端（Cursor / VS Code 等）需注销重登后生效".to_owned());
            }
            _ => {}
        }
        steps.extend(self.warnings.iter().cloned());
        steps.push(if terminal_client {
            format!("在新打开的终端中重启 {client_display} 即可使用，无需其他手动步骤")
        } else {
            format!("重启 {client_display} 即可使用，无需其他手动步骤")
        });
        steps
    }
}

/// App 单实例 + 桌面是唯一写入方：进程级锁串行化 persist/remove 已足够，
/// 文件层另有 temp+rename 原子性兜底。
static ENVSYNC_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    ENVSYNC_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn current_paths() -> EnvPaths {
    let user_home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_owned());
    let mcpane_home = mcpane_hub_protocol::mcpane_home();
    // MCPANE_HOME 覆盖值可能是相对路径；rc 注入需要绝对路径。
    let mcpane_home = std::path::absolute(&mcpane_home).unwrap_or(mcpane_home);
    let shell = std::env::var("SHELL").ok().and_then(|s| {
        Path::new(&s)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    });
    EnvPaths {
        mcpane_home,
        user_home: PathBuf::from(user_home),
        shell,
    }
}

fn validate_var(var: &str) -> Result<(), String> {
    let well_formed = var.starts_with("MCPANE_")
        && var
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if well_formed {
        Ok(())
    } else {
        Err(format!("非受管环境变量名：{var}"))
    }
}

/// 持久化一个变量（幂等）。凭据存储失败 => Err；其余降级进 warnings。
pub fn persist_env_var(var: &str, value: &str) -> Result<EnvSyncReport, String> {
    let _guard = lock();
    persist_with(
        &current_paths(),
        &session::PlatformSession,
        host_os(),
        var,
        value,
    )
}

/// 移除一个变量；最后一个受管变量移除时全量清理（rc 标记行 / 派生视图 /
/// LaunchAgent 一并撤除，系统回到接入前状态）。
pub fn remove_env_var(var: &str) -> Result<EnvSyncReport, String> {
    let _guard = lock();
    remove_with(&current_paths(), &session::PlatformSession, host_os(), var)
}

/// App 启动钩子：macOS 重放 launchctl setenv（兜底 LaunchAgent 被删/禁用），
/// 其余平台 no-op。返回警告集（调用方记日志即可）。
pub fn resync_session() -> Vec<String> {
    let _guard = lock();
    resync_with(&current_paths(), &session::PlatformSession, host_os())
}

fn agent_path(user_home: &Path) -> PathBuf {
    user_home.join("Library/LaunchAgents/com.mcpane.env.plist")
}

fn persist_with(
    paths: &EnvPaths,
    sync: &dyn SessionSync,
    os: HostOs,
    var: &str,
    value: &str,
) -> Result<EnvSyncReport, String> {
    validate_var(var)?;
    store::validate_value(value)?;

    if os == HostOs::Windows {
        // 注册表即存储本体：失败=致命，无 env.sh/rc 概念。
        sync.set_var(var, value)?;
        let mut report = EnvSyncReport::new(r"HKCU\Environment".to_owned());
        report.gui_session = "synced_now".to_owned();
        return Ok(report);
    }

    let env_file = store::upsert_var(&paths.mcpane_home, var, value)?;
    let mut report = EnvSyncReport::new(env_file.display().to_string());

    match rc::rc_targets(&paths.user_home, paths.shell.as_deref(), os) {
        rc::RcTargets::Posix(targets) => {
            for target in targets {
                match rc::ensure_source_line(&target, &env_file) {
                    Ok(_) => report.rc_files.push(target.display().to_string()),
                    Err(e) => report.warnings.push(format!(
                        "写入 {} 失败（{e}），可在 shell 配置中手动执行 export {var}=<Token>",
                        target.display()
                    )),
                }
            }
        }
        rc::RcTargets::Fish => {
            let vars = store::list_vars(&paths.mcpane_home)?;
            match store::write_fish_env(&paths.user_home, &vars) {
                Ok(Some(path)) => report.rc_files.push(path.display().to_string()),
                Ok(None) => {}
                Err(e) => report.warnings.push(format!(
                    "写入 fish 配置失败（{e}），可手动执行 set -gx {var} <Token>"
                )),
            }
        }
    }

    match os {
        HostOs::MacOs => {
            report.gui_session = match sync.set_var(var, value) {
                Ok(()) => "synced_now".to_owned(),
                Err(e) => {
                    report
                        .warnings
                        .push(format!("同步 GUI 会话环境失败（{e}），重新登录系统后生效"));
                    "synced_at_login".to_owned()
                }
            };
            report.persistence_agent =
                match sync.install_agent(&env_file, &agent_path(&paths.user_home)) {
                    Ok(AgentState::Installed) => "installed".to_owned(),
                    Ok(AgentState::Updated) => "updated".to_owned(),
                    Ok(AgentState::Unchanged) => "unchanged".to_owned(),
                    Err(e) => {
                        report.warnings.push(format!(
                            "登录代理安装失败（{e}），系统重启后 GUI 客户端需重新连接"
                        ));
                        "failed".to_owned()
                    }
                };
        }
        HostOs::Linux => {
            let vars = store::list_vars(&paths.mcpane_home)?;
            match store::write_environment_d(&paths.user_home, &vars) {
                Ok(_) => report.gui_session = "synced_at_login".to_owned(),
                Err(e) => report.warnings.push(format!(
                    "写入 environment.d 失败（{e}），GUI 客户端需手动设置该环境变量"
                )),
            }
        }
        HostOs::Windows => unreachable!("已在函数开头返回"),
    }

    Ok(report)
}

fn remove_with(
    paths: &EnvPaths,
    sync: &dyn SessionSync,
    os: HostOs,
    var: &str,
) -> Result<EnvSyncReport, String> {
    validate_var(var)?;

    if os == HostOs::Windows {
        sync.unset_var(var)?;
        return Ok(EnvSyncReport::new(r"HKCU\Environment".to_owned()));
    }

    let remaining = store::remove_var(&paths.mcpane_home, var)?;
    let env_file = store::env_file_path(&paths.mcpane_home);
    let mut report = EnvSyncReport::new(env_file.display().to_string());

    if os == HostOs::MacOs
        && let Err(e) = sync.unset_var(var)
    {
        report.warnings.push(format!(
            "清除 GUI 会话环境变量失败（{e}），重新登录系统后失效"
        ));
    }

    if remaining == 0 {
        // 最后一个受管变量：全量清理，系统回到接入前状态。
        for rc_path in rc::all_candidate_rcs(&paths.user_home) {
            if let Err(e) = rc::remove_source_line(&rc_path) {
                report.warnings.push(format!(
                    "清理 {} 中的注入内容失败（{e}），残留内容带存在性判断，不影响使用",
                    rc_path.display()
                ));
            }
        }
        if let Err(e) = store::write_fish_env(&paths.user_home, &[]) {
            report.warnings.push(e);
        }
        if os == HostOs::Linux
            && let Err(e) = store::write_environment_d(&paths.user_home, &[])
        {
            report.warnings.push(e);
        }
        if os == HostOs::MacOs
            && let Err(e) = sync.remove_agent(&agent_path(&paths.user_home))
        {
            report.warnings.push(format!("移除登录重放代理失败（{e}）"));
        }
    } else {
        // 还有其它客户端在用：只重生成派生视图。
        let vars = store::list_vars(&paths.mcpane_home)?;
        if store::fish_conf_path(&paths.user_home).exists()
            && let Err(e) = store::write_fish_env(&paths.user_home, &vars)
        {
            report.warnings.push(e);
        }
        if os == HostOs::Linux
            && let Err(e) = store::write_environment_d(&paths.user_home, &vars)
        {
            report.warnings.push(e);
        }
    }

    Ok(report)
}

fn resync_with(paths: &EnvPaths, sync: &dyn SessionSync, os: HostOs) -> Vec<String> {
    if os != HostOs::MacOs {
        return Vec::new();
    }
    let vars = match store::list_vars(&paths.mcpane_home) {
        Ok(vars) => vars,
        Err(e) => return vec![e],
    };
    let mut warnings = Vec::new();
    for (var, value) in &vars {
        if let Err(e) = sync.set_var(var, value) {
            warnings.push(format!("重放 {var} 到 GUI 会话失败：{e}"));
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::rc::{MARKER, source_line};
    use super::session::render_agent_plist;
    use super::*;
    use std::cell::RefCell;
    use std::fs;

    /// 记录型 stub：断言编排层的调用序列与降级行为，不碰真实系统。
    struct RecordingSync {
        calls: RefCell<Vec<String>>,
        fail_set_var: bool,
        fail_agent: bool,
    }

    impl RecordingSync {
        fn new() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                fail_set_var: false,
                fail_agent: false,
            }
        }
    }

    impl SessionSync for RecordingSync {
        fn set_var(&self, var: &str, value: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("set {var}={value}"));
            if self.fail_set_var {
                Err("launchctl 不可用".to_owned())
            } else {
                Ok(())
            }
        }
        fn unset_var(&self, var: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("unset {var}"));
            Ok(())
        }
        fn install_agent(&self, _env_file: &Path, agent: &Path) -> Result<AgentState, String> {
            self.calls
                .borrow_mut()
                .push(format!("agent {}", agent.display()));
            if self.fail_agent {
                Err("LaunchAgents 目录只读".to_owned())
            } else {
                Ok(AgentState::Installed)
            }
        }
        fn remove_agent(&self, agent: &Path) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("rm-agent {}", agent.display()));
            Ok(())
        }
    }

    /// 带空格的 tempdir：覆盖 macOS 默认 MCPANE_HOME 含空格的真实形态。
    /// 纳秒时间戳在并行测试线程间可能相同，叠加进程内计数器保证唯一。
    fn temp_paths(shell: Option<&str>) -> EnvPaths {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("mcpane envsync {nanos}-{seq}"));
        let paths = EnvPaths {
            mcpane_home: root.join("Application Support/MCPane"),
            user_home: root.join("home"),
            shell: shell.map(str::to_owned),
        };
        fs::create_dir_all(&paths.mcpane_home).unwrap();
        fs::create_dir_all(&paths.user_home).unwrap();
        paths
    }

    fn env_sh(paths: &EnvPaths) -> String {
        fs::read_to_string(paths.mcpane_home.join("env.sh")).unwrap()
    }

    #[test]
    fn persist_writes_env_file_and_injects_zshrc() {
        let paths = temp_paths(Some("zsh"));
        let sync = RecordingSync::new();
        let report =
            persist_with(&paths, &sync, HostOs::MacOs, "MCPANE_CODEX_TOKEN", "tok-1").unwrap();

        let content = env_sh(&paths);
        assert!(
            content.contains("export MCPANE_CODEX_TOKEN='tok-1'"),
            "{content}"
        );
        let zshrc = fs::read_to_string(paths.user_home.join(".zshrc")).unwrap();
        assert!(zshrc.trim_end().ends_with(MARKER), "{zshrc}");
        // 带空格路径必须被引号包裹
        assert!(
            zshrc.contains("\"") && zshrc.contains("Application Support"),
            "{zshrc}"
        );
        assert_eq!(report.gui_session, "synced_now");
        assert_eq!(report.persistence_agent, "installed");
        assert!(report.warnings.is_empty());
        assert_eq!(report.rc_files.len(), 1);
    }

    #[test]
    fn persist_is_idempotent_and_updates_value_in_place() {
        let paths = temp_paths(Some("zsh"));
        let sync = RecordingSync::new();
        persist_with(&paths, &sync, HostOs::MacOs, "MCPANE_CODEX_TOKEN", "tok-1").unwrap();
        let first_env = env_sh(&paths);
        let first_rc = fs::read_to_string(paths.user_home.join(".zshrc")).unwrap();

        // 同值重放：字节级不变
        persist_with(&paths, &sync, HostOs::MacOs, "MCPANE_CODEX_TOKEN", "tok-1").unwrap();
        assert_eq!(env_sh(&paths), first_env);
        assert_eq!(
            fs::read_to_string(paths.user_home.join(".zshrc")).unwrap(),
            first_rc
        );

        // 换值：只有该行变化，且不新增行
        persist_with(&paths, &sync, HostOs::MacOs, "MCPANE_CODEX_TOKEN", "tok-2").unwrap();
        let updated = env_sh(&paths);
        assert!(updated.contains("'tok-2'"));
        assert!(!updated.contains("'tok-1'"));
        assert_eq!(updated.lines().count(), first_env.lines().count());
    }

    #[test]
    fn persist_preserves_user_added_lines_and_multiple_vars() {
        let paths = temp_paths(Some("zsh"));
        let sync = RecordingSync::new();
        persist_with(&paths, &sync, HostOs::MacOs, "MCPANE_CODEX_TOKEN", "a").unwrap();
        // 用户手加一行非受管内容
        let mut content = env_sh(&paths);
        content.push_str("export MY_OWN=1\n");
        fs::write(paths.mcpane_home.join("env.sh"), &content).unwrap();

        persist_with(&paths, &sync, HostOs::MacOs, "MCPANE_CLAUDE_TOKEN", "b").unwrap();
        let content = env_sh(&paths);
        assert!(content.contains("export MY_OWN=1"));
        assert!(content.contains("MCPANE_CODEX_TOKEN='a'"));
        assert!(content.contains("MCPANE_CLAUDE_TOKEN='b'"));
    }

    #[test]
    fn env_file_is_0600() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let paths = temp_paths(Some("zsh"));
            persist_with(
                &paths,
                &RecordingSync::new(),
                HostOs::MacOs,
                "MCPANE_CODEX_TOKEN",
                "t",
            )
            .unwrap();
            let mode = fs::metadata(paths.mcpane_home.join("env.sh"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn token_with_quote_roundtrips() {
        let paths = temp_paths(Some("zsh"));
        let tricky = "ab'cd$ef";
        persist_with(
            &paths,
            &RecordingSync::new(),
            HostOs::MacOs,
            "MCPANE_CODEX_TOKEN",
            tricky,
        )
        .unwrap();
        let vars = store::list_vars(&paths.mcpane_home).unwrap();
        assert_eq!(
            vars,
            vec![("MCPANE_CODEX_TOKEN".to_owned(), tricky.to_owned())]
        );
    }

    #[test]
    fn value_with_newline_is_rejected() {
        let paths = temp_paths(Some("zsh"));
        let err = persist_with(
            &paths,
            &RecordingSync::new(),
            HostOs::MacOs,
            "MCPANE_CODEX_TOKEN",
            "a\nb",
        )
        .unwrap_err();
        assert!(err.contains("拒绝"), "{err}");
        assert!(!paths.mcpane_home.join("env.sh").exists());
    }

    #[test]
    fn non_managed_var_name_is_rejected() {
        let paths = temp_paths(Some("zsh"));
        assert!(persist_with(&paths, &RecordingSync::new(), HostOs::MacOs, "PATH", "x").is_err());
    }

    #[test]
    fn shell_dispatch_targets() {
        let paths = temp_paths(None);
        // bash + macOS：双注入（login shell 不读 .bashrc）
        let sync = RecordingSync::new();
        let report = persist_with(
            &EnvPaths {
                shell: Some("bash".to_owned()),
                ..copy_paths(&paths)
            },
            &sync,
            HostOs::MacOs,
            "MCPANE_CODEX_TOKEN",
            "t",
        )
        .unwrap();
        assert_eq!(report.rc_files.len(), 2);
        assert!(paths.user_home.join(".bashrc").exists());
        assert!(paths.user_home.join(".bash_profile").exists());

        // SHELL 缺失 → .profile
        let report = persist_with(
            &EnvPaths {
                shell: None,
                ..copy_paths(&paths)
            },
            &RecordingSync::new(),
            HostOs::Linux,
            "MCPANE_CODEX_TOKEN",
            "t",
        )
        .unwrap();
        assert!(report.rc_files.iter().any(|f| f.ends_with(".profile")));
    }

    fn copy_paths(paths: &EnvPaths) -> EnvPaths {
        EnvPaths {
            mcpane_home: paths.mcpane_home.clone(),
            user_home: paths.user_home.clone(),
            shell: paths.shell.clone(),
        }
    }

    #[test]
    fn fish_gets_conf_d_file_not_rc_line() {
        let paths = temp_paths(Some("fish"));
        let report = persist_with(
            &paths,
            &RecordingSync::new(),
            HostOs::MacOs,
            "MCPANE_CODEX_TOKEN",
            "to'ken",
        )
        .unwrap();
        let fish =
            fs::read_to_string(paths.user_home.join(".config/fish/conf.d/mcpane.fish")).unwrap();
        assert!(
            fish.contains(r"set -gx MCPANE_CODEX_TOKEN 'to\'ken'"),
            "{fish}"
        );
        assert!(!paths.user_home.join(".zshrc").exists());
        assert!(report.rc_files.iter().any(|f| f.ends_with("mcpane.fish")));
    }

    #[test]
    fn linux_writes_environment_d() {
        let paths = temp_paths(Some("bash"));
        let report = persist_with(
            &paths,
            &RecordingSync::new(),
            HostOs::Linux,
            "MCPANE_CODEX_TOKEN",
            "t",
        )
        .unwrap();
        let conf = fs::read_to_string(paths.user_home.join(".config/environment.d/50-mcpane.conf"))
            .unwrap();
        assert!(conf.contains("MCPANE_CODEX_TOKEN=\"t\""), "{conf}");
        assert_eq!(report.gui_session, "synced_at_login");
        // Linux bash 只注 .bashrc
        assert_eq!(report.rc_files.len(), 1);
        assert!(!paths.user_home.join(".bash_profile").exists());
    }

    #[test]
    fn rc_injection_replaces_line_when_home_moves() {
        let paths = temp_paths(Some("zsh"));
        let sync = RecordingSync::new();
        persist_with(&paths, &sync, HostOs::MacOs, "MCPANE_CODEX_TOKEN", "t").unwrap();

        // 模拟 MCPANE_HOME 迁移：换 home 的 paths 再 persist
        let moved = EnvPaths {
            mcpane_home: paths.mcpane_home.parent().unwrap().join("Moved Home"),
            ..copy_paths(&paths)
        };
        fs::create_dir_all(&moved.mcpane_home).unwrap();
        persist_with(&moved, &sync, HostOs::MacOs, "MCPANE_CODEX_TOKEN", "t").unwrap();

        let zshrc = fs::read_to_string(paths.user_home.join(".zshrc")).unwrap();
        let marker_lines = zshrc.lines().filter(|l| l.ends_with(MARKER)).count();
        assert_eq!(marker_lines, 1, "标记行必须整行替换而非追加：{zshrc}");
        assert!(zshrc.contains("Moved Home"));
    }

    #[test]
    fn rc_without_trailing_newline_gets_one_before_append() {
        let paths = temp_paths(Some("zsh"));
        fs::write(paths.user_home.join(".zshrc"), "alias ll='ls -l'").unwrap();
        persist_with(
            &paths,
            &RecordingSync::new(),
            HostOs::MacOs,
            "MCPANE_CODEX_TOKEN",
            "t",
        )
        .unwrap();
        let zshrc = fs::read_to_string(paths.user_home.join(".zshrc")).unwrap();
        assert!(zshrc.starts_with("alias ll='ls -l'\n["), "{zshrc}");
    }

    #[test]
    fn set_var_failure_degrades_to_warning_but_persists() {
        let paths = temp_paths(Some("zsh"));
        let sync = RecordingSync {
            fail_set_var: true,
            ..RecordingSync::new()
        };
        let report = persist_with(&paths, &sync, HostOs::MacOs, "MCPANE_CODEX_TOKEN", "t").unwrap();
        assert!(paths.mcpane_home.join("env.sh").exists());
        assert_eq!(report.gui_session, "synced_at_login");
        assert!(!report.warnings.is_empty());
    }

    #[test]
    fn agent_failure_degrades_to_warning() {
        let paths = temp_paths(Some("zsh"));
        let sync = RecordingSync {
            fail_agent: true,
            ..RecordingSync::new()
        };
        let report = persist_with(&paths, &sync, HostOs::MacOs, "MCPANE_CODEX_TOKEN", "t").unwrap();
        assert_eq!(report.persistence_agent, "failed");
        assert!(report.warnings.iter().any(|w| w.contains("登录代理")));
    }

    #[test]
    fn remove_last_var_cleans_everything() {
        let paths = temp_paths(Some("zsh"));
        let sync = RecordingSync::new();
        persist_with(&paths, &sync, HostOs::MacOs, "MCPANE_CODEX_TOKEN", "a").unwrap();
        persist_with(&paths, &sync, HostOs::MacOs, "MCPANE_CLAUDE_TOKEN", "b").unwrap();

        // 移除第一个：env.sh 保留、rc 行保留
        remove_with(&paths, &sync, HostOs::MacOs, "MCPANE_CODEX_TOKEN").unwrap();
        assert!(env_sh(&paths).contains("MCPANE_CLAUDE_TOKEN"));
        assert!(
            fs::read_to_string(paths.user_home.join(".zshrc"))
                .unwrap()
                .contains(MARKER)
        );

        // 移除最后一个：全量清理
        remove_with(&paths, &sync, HostOs::MacOs, "MCPANE_CLAUDE_TOKEN").unwrap();
        assert!(!paths.mcpane_home.join("env.sh").exists());
        assert!(
            !fs::read_to_string(paths.user_home.join(".zshrc"))
                .unwrap()
                .contains(MARKER)
        );
        let calls = sync.calls.borrow();
        assert!(calls.iter().any(|c| c.starts_with("rm-agent")), "{calls:?}");
    }

    #[test]
    fn resync_replays_all_vars_on_macos_only() {
        let paths = temp_paths(Some("zsh"));
        let sync = RecordingSync::new();
        persist_with(&paths, &sync, HostOs::MacOs, "MCPANE_CODEX_TOKEN", "a").unwrap();
        persist_with(&paths, &sync, HostOs::MacOs, "MCPANE_CLAUDE_TOKEN", "b").unwrap();
        sync.calls.borrow_mut().clear();

        let warnings = resync_with(&paths, &sync, HostOs::MacOs);
        assert!(warnings.is_empty());
        assert_eq!(sync.calls.borrow().len(), 2);

        assert!(resync_with(&paths, &sync, HostOs::Linux).is_empty());
        assert_eq!(sync.calls.borrow().len(), 2, "Linux 不应有会话重放");
    }

    #[test]
    fn agent_plist_snapshot_quotes_spaced_path() {
        let plist = render_agent_plist(Path::new("/Users/x/Application Support/MCPane/env.sh"));
        // sh 双引号在 XML 里原样保留（无需 XML 转义），路径必须带引号
        assert!(
            plist.contains("\"/Users/x/Application Support/MCPane/env.sh\""),
            "{plist}"
        );
        assert!(plist.contains("<string>com.mcpane.env</string>"));
        assert!(plist.contains("<string>io.mcpane.desktop</string>"));
        assert!(plist.contains("AssociatedBundleIdentifiers"));
        assert!(plist.contains("launchctl setenv"));
    }

    #[test]
    fn source_line_guard_keeps_stale_line_harmless() {
        let line = source_line(Path::new("/tmp/gone/env.sh"));
        assert!(line.starts_with("[ -f "), "{line}");
        assert!(line.ends_with(MARKER));
    }

    // ---------- Windows 分支（ADR-016：注册表即存储本体） ----------

    /// Windows 上凭据只进 `HKCU\Environment`：不写 env.sh、不碰 rc 文件。
    /// 此前这条分支零测试覆盖。
    #[test]
    fn windows_persists_to_registry_only() {
        let paths = temp_paths(None);
        let sync = RecordingSync::new();
        let report = persist_with(
            &paths,
            &sync,
            HostOs::Windows,
            "MCPANE_CODEX_TOKEN",
            "tok-1",
        )
        .unwrap();

        assert_eq!(report.credential_store, r"HKCU\Environment");
        assert_eq!(report.gui_session, "synced_now");
        assert!(report.rc_files.is_empty(), "Windows 不注入 rc 文件");
        assert_eq!(
            sync.calls.borrow().as_slice(),
            ["set MCPANE_CODEX_TOKEN=tok-1"]
        );
        assert!(
            !store::env_file_path(&paths.mcpane_home).exists(),
            "Windows 不得写出 env.sh"
        );
    }

    /// 注册表是存储本体，写失败即致命——不能像 macOS 的 launchctl 那样降级成警告。
    #[test]
    fn windows_registry_failure_is_fatal() {
        let paths = temp_paths(None);
        let mut sync = RecordingSync::new();
        sync.fail_set_var = true;
        assert!(
            persist_with(&paths, &sync, HostOs::Windows, "MCPANE_CODEX_TOKEN", "tok").is_err(),
            "写注册表失败必须整体失败"
        );
        assert!(
            !store::env_file_path(&paths.mcpane_home).exists(),
            "失败路径也不该留下 env.sh"
        );
    }

    /// 撤销同样只走注册表。
    #[test]
    fn windows_removes_from_registry_only() {
        let paths = temp_paths(None);
        let sync = RecordingSync::new();
        let report = remove_with(&paths, &sync, HostOs::Windows, "MCPANE_CODEX_TOKEN").unwrap();

        assert_eq!(report.credential_store, r"HKCU\Environment");
        assert!(report.rc_files.is_empty());
        assert_eq!(sync.calls.borrow().as_slice(), ["unset MCPANE_CODEX_TOKEN"]);
    }
}
