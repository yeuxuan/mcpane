//! Codex / Gemini 凭据环境变量冲突检测。
//!
//! 名单逐个变量经源码/实测证实（勿凭直觉扩大范围；cc-switch 按关键字子串
//! 匹配的做法会大量误报，已刻意分道）：
//!
//! - **Claude Code 不需要检测**：`~/.claude/settings.json` 的 `env` 块会**覆盖**
//!   继承来的 shell 变量（实测：shell 设 0、settings 设 1，子进程读到 1）。
//!   用户 rc 里的 `ANTHROPIC_BASE_URL` 盖不掉我们写入的值。
//! - **Codex**：`OPENAI_API_KEY` 不构成冲突——`load_auth()` 主流程不读它。
//!   真正盖过 auth.json 的只有 `CODEX_API_KEY`（需 enable_codex_api_key_env）
//!   与 `CODEX_ACCESS_TOKEN`。
//! - **Gemini**（核实基准 google-gemini/gemini-cli@acae712，2026-07-17）：
//!   `loadEnvironment` 只在 `process.env` **缺失**该 key 时才写入
//!   （settings.ts:722 `if (!Object.hasOwn(process.env, key))`），因此 shell
//!   导出的 `GEMINI_API_KEY` / `GOOGLE_GEMINI_BASE_URL`（contentGenerator.ts:349）
//!   / `GEMINI_MODEL`（config.ts:843，还赢过 settings.json 的 model.name）
//!   恒赢过我们写的 `~/.gemini/.env`。被证伪不检测：`GOOGLE_API_KEY` 只走
//!   Vertex 分支；`GOOGLE_GENAI_USE_VERTEXAI`/`GOOGLE_GENAI_USE_GCA`/
//!   `GOOGLE_APPLICATION_CREDENTIALS` 在 `security.auth.selectedType` 已固定
//!   时被短路（validateNonInterActiveAuth.ts:27），翻不了 auth 分支。
//!   另：`security.auth.enforcedType` 若被设为非 gemini-api-key，gemini 启动
//!   即抛错——单独检测。已知局限：gemini-cli 的 .env 搜索从 cwd 逐级向上、
//!   **命中即停**（settings.ts:567-594），项目目录里的 .env 会整文件 shadow
//!   掉 ~/.gemini/.env；GUI 进程无从得知用户会在哪个目录起 gemini，不扫。
//!
//! 扫描目标是 **shell rc 文件**而非本进程 env：MCPane 是 GUI 进程，环境继承自
//! launchd/explorer，与用户敲 CLI 的那个终端不是同一套。本进程 env 也一并
//! 查（macOS `launchctl setenv` 会灌进 GUI 会话），但 rc 才是主要来源。

use std::path::Path;

/// 会覆盖 auth.json 的 Codex 变量，按优先级从高到低。
/// 附带说明用于拼进用户可见的 warning。
const CODEX_OVERRIDE_VARS: &[(&str, &str)] = &[
    (
        "CODEX_API_KEY",
        "优先级高于 auth.json（需 enable_codex_api_key_env 开启）",
    ),
    ("CODEX_ACCESS_TOKEN", "优先级高于 auth.json"),
];

/// 会盖过 ~/.gemini/.env 受管值的 Gemini 变量（shell 导出恒赢，见模块注释）。
const GEMINI_OVERRIDE_VARS: &[(&str, &str)] = &[
    ("GEMINI_API_KEY", "shell 导出值恒优先于 ~/.gemini/.env"),
    (
        "GOOGLE_GEMINI_BASE_URL",
        "shell 导出值恒优先于 ~/.gemini/.env",
    ),
    (
        "GEMINI_MODEL",
        "shell 导出值恒优先于 ~/.gemini/.env 与 settings.json",
    ),
];

/// 检出 Codex 凭据冲突，返回可直接塞进 `SwitchOutcome.warnings` 的人话提示。
pub fn codex_credential_conflicts() -> Vec<String> {
    override_conflicts(CODEX_OVERRIDE_VARS, "Codex")
}

/// 检出 Gemini 凭据/端点冲突（含 enforcedType 硬冲突）。
pub fn gemini_credential_conflicts() -> Vec<String> {
    let mut findings = override_conflicts(GEMINI_OVERRIDE_VARS, "Gemini CLI");
    findings.extend(gemini_enforced_auth_conflict());
    findings
}

/// 通用扫描：GUI 会话 env + 全部候选 rc + Windows 注册表。
/// 任何 IO 失败都静默跳过——这是锦上添花的提示，不该阻断切换。
fn override_conflicts(vars: &[(&str, &str)], client_name: &str) -> Vec<String> {
    let paths = super::current_paths();
    let mut findings: Vec<String> = Vec::new();

    for (var, note) in vars {
        let mut sources: Vec<String> = Vec::new();
        if std::env::var_os(var).is_some_and(|v| !v.is_empty()) {
            sources.push("当前 GUI 会话环境".to_owned());
        }
        sources.extend(rc_sources_setting(&paths.user_home, var));
        sources.extend(machine_sources_setting(var));
        sources.dedup();

        if !sources.is_empty() {
            findings.push(format!(
                "检测到 {var} 已在 {} 中设置（{note}），{client_name} 将优先使用该值而非 MCPane 写入的值，切换可能不生效。请移除该设置或改由 MCPane 管理。",
                sources.join("、")
            ));
        }
    }
    findings
}

/// `~/.gemini/settings.json` 的 `security.auth.enforcedType` 被设为非
/// gemini-api-key 时，gemini 启动即抛错（validateNonInterActiveAuth.ts:29-35），
/// MCPane 的切换必然失效——这是硬冲突，单独点名。
fn gemini_enforced_auth_conflict() -> Vec<String> {
    enforced_auth_conflict_in(&super::current_paths().user_home)
}

fn enforced_auth_conflict_in(user_home: &Path) -> Vec<String> {
    let settings_path = user_home.join(".gemini").join("settings.json");
    let Ok(Some(content)) = super::store::read_optional(&settings_path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
        return Vec::new();
    };
    match value
        .pointer("/security/auth/enforcedType")
        .and_then(|v| v.as_str())
    {
        Some(enforced) if enforced != "gemini-api-key" => vec![format!(
            "~/.gemini/settings.json 的 security.auth.enforcedType 被设为 \"{enforced}\"，\
             与 MCPane 管理的 gemini-api-key 认证方式冲突，Gemini CLI 会启动报错。\
             请移除该键或改回 gemini-api-key。"
        )],
        _ => Vec::new(),
    }
}

/// 扫所有候选 rc（不依赖当前 `$SHELL`——用户可能换过）以及 fish conf。
fn rc_sources_setting(user_home: &Path, var: &str) -> Vec<String> {
    let mut sources = Vec::new();
    let mut candidates = super::rc::all_conflict_scan_rcs(user_home);
    candidates.push(super::store::fish_conf_path(user_home));
    for rc in candidates {
        let Ok(Some(content)) = super::store::read_optional(&rc) else {
            continue;
        };
        if content.lines().any(|line| line_sets_var(line, var)) {
            sources.push(rc.display().to_string());
        }
    }
    sources
}

#[cfg(windows)]
fn machine_sources_setting(var: &str) -> Vec<String> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(env) = hkcu.open_subkey_with_flags("Environment", KEY_READ) else {
        return Vec::new();
    };
    match env.get_value::<String, _>(var) {
        Ok(value) if !value.is_empty() => vec![r"HKCU\Environment".to_owned()],
        _ => Vec::new(),
    }
}

#[cfg(not(windows))]
fn machine_sources_setting(_var: &str) -> Vec<String> {
    Vec::new()
}

/// 这一行是否在给 `var` 赋值。覆盖 POSIX（`export VAR=` / `VAR=`）与
/// fish（`set -gx VAR value`）两种写法；注释行不算。
/// 故意不匹配 `unset` / 纯引用（`$VAR`）。
fn line_sets_var(line: &str, var: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.starts_with('#') {
        return false;
    }
    // fish: set [-flags] VAR value —— 首个非 flag token 即变量名。
    if let Some(rest) = trimmed.strip_prefix("set ") {
        return rest
            .split_whitespace()
            .find(|token| !token.starts_with('-'))
            .is_some_and(|name| name == var);
    }
    // POSIX: 可选 export 前缀，随后必须紧跟 `VAR=`。
    let body = trimmed
        .strip_prefix("export ")
        .map(str::trim_start)
        .unwrap_or(trimmed);
    body.strip_prefix(var)
        .is_some_and(|rest| rest.starts_with('='))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_assignment_forms_are_detected() {
        assert!(line_sets_var(
            "export CODEX_ACCESS_TOKEN=abc",
            "CODEX_ACCESS_TOKEN"
        ));
        assert!(line_sets_var(
            "CODEX_ACCESS_TOKEN=abc",
            "CODEX_ACCESS_TOKEN"
        ));
        assert!(line_sets_var(
            "  export   CODEX_ACCESS_TOKEN=\"abc\"  ",
            "CODEX_ACCESS_TOKEN"
        ));
    }

    #[test]
    fn fish_set_forms_are_detected() {
        assert!(line_sets_var("set -gx CODEX_API_KEY abc", "CODEX_API_KEY"));
        assert!(line_sets_var("set -x CODEX_API_KEY abc", "CODEX_API_KEY"));
        assert!(line_sets_var("set CODEX_API_KEY abc", "CODEX_API_KEY"));
    }

    #[test]
    fn comments_and_unrelated_lines_are_ignored() {
        assert!(!line_sets_var(
            "# export CODEX_ACCESS_TOKEN=abc",
            "CODEX_ACCESS_TOKEN"
        ));
        assert!(!line_sets_var(
            "echo $CODEX_ACCESS_TOKEN",
            "CODEX_ACCESS_TOKEN"
        ));
        assert!(!line_sets_var(
            "unset CODEX_ACCESS_TOKEN",
            "CODEX_ACCESS_TOKEN"
        ));
    }

    /// 前缀相同的变量不得误报——否则用户会被无中生有的告警骚扰。
    #[test]
    fn prefix_lookalikes_do_not_match() {
        assert!(!line_sets_var(
            "export CODEX_ACCESS_TOKEN_BACKUP=abc",
            "CODEX_ACCESS_TOKEN"
        ));
        assert!(!line_sets_var(
            "export MY_CODEX_API_KEY=abc",
            "CODEX_API_KEY"
        ));
    }

    /// 被证伪的变量不在检测名单里——它们盖不掉我们的写入，
    /// 报了就是误导（cc-switch 正是按关键字子串误报的）。
    #[test]
    fn disproven_vars_are_not_watched() {
        let watched: Vec<&str> = CODEX_OVERRIDE_VARS
            .iter()
            .chain(GEMINI_OVERRIDE_VARS)
            .map(|(v, _)| *v)
            .collect();
        assert!(!watched.contains(&"OPENAI_API_KEY"));
        assert!(!watched.contains(&"ANTHROPIC_BASE_URL"));
        assert!(!watched.contains(&"ANTHROPIC_API_KEY"));
        // Gemini 侧证伪（gemini-cli@acae712）：GOOGLE_API_KEY 只走 Vertex 分支；
        // auth 开关在 selectedType 已固定时被短路，翻不了分支。
        assert!(!watched.contains(&"GOOGLE_API_KEY"));
        assert!(!watched.contains(&"GOOGLE_GENAI_USE_VERTEXAI"));
        assert!(!watched.contains(&"GOOGLE_GENAI_USE_GCA"));
        assert!(!watched.contains(&"GOOGLE_APPLICATION_CREDENTIALS"));
    }

    /// Gemini 三个受管变量都要能从 rc 里翻出来。
    #[test]
    fn gemini_vars_detected_in_rc() {
        let home = temp_home("gemini-rc");
        let _ = std::fs::write(
            home.join(".zshrc"),
            "export GEMINI_API_KEY=sk\nexport GOOGLE_GEMINI_BASE_URL=https://x\nset -gx GEMINI_MODEL m\n",
        );
        for (var, _) in GEMINI_OVERRIDE_VARS {
            assert_eq!(rc_sources_setting(&home, var).len(), 1, "{var} 应被检出");
        }
        // GEMINI 前缀相似变量不误报。
        assert!(rc_sources_setting(&home, "GEMINI_SANDBOX").is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// enforcedType 硬冲突：非 gemini-api-key 报警；等值或缺失不报。
    #[test]
    fn gemini_enforced_type_conflict_detection() {
        let home = temp_home("gemini-enforced");
        let dir = home.join(".gemini");
        let _ = std::fs::create_dir_all(&dir);

        // 缺失 → 不报。
        let _ = std::fs::write(dir.join("settings.json"), r#"{"security":{"auth":{}}}"#);
        assert!(enforced_auth_conflict_in(&home).is_empty());

        // 等值 → 不报。
        let _ = std::fs::write(
            dir.join("settings.json"),
            r#"{"security":{"auth":{"enforcedType":"gemini-api-key"}}}"#,
        );
        assert!(enforced_auth_conflict_in(&home).is_empty());

        // 非 gemini-api-key → 硬冲突报警。
        let _ = std::fs::write(
            dir.join("settings.json"),
            r#"{"security":{"auth":{"enforcedType":"oauth-personal"}}}"#,
        );
        let findings = enforced_auth_conflict_in(&home);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].contains("oauth-personal"), "{findings:?}");

        let _ = std::fs::remove_dir_all(&home);
    }

    /// MCPane 自己注入的 source 行不能被当成冲突。
    #[test]
    fn mcpane_source_line_is_not_a_conflict() {
        let line = r#"[ -f "/x/env.sh" ] && . "/x/env.sh" # mcpane:env"#;
        assert!(!line_sets_var(line, "CODEX_ACCESS_TOKEN"));
        assert!(!line_sets_var(line, "CODEX_API_KEY"));
    }

    fn temp_home(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let home = std::env::temp_dir().join(format!("mcpane-conflict-{tag}-{nanos}"));
        let _ = std::fs::create_dir_all(&home);
        home
    }

    /// 整条 rc 扫描链路：写进 .zshrc 的 export 要被翻出来，并带上出处。
    #[test]
    fn rc_scan_finds_var_and_reports_path() {
        let home = temp_home("zshrc");
        let rc = home.join(".zshrc");
        let _ = std::fs::write(&rc, "export PATH=/usr/bin\nexport CODEX_ACCESS_TOKEN=tok\n");

        let sources = rc_sources_setting(&home, "CODEX_ACCESS_TOKEN");
        assert_eq!(sources.len(), 1, "应恰好检出一处: {sources:?}");
        assert!(sources[0].ends_with(".zshrc"), "出处应指向 rc: {sources:?}");
        assert!(rc_sources_setting(&home, "CODEX_API_KEY").is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// zsh login shell 的 .zprofile/.zshenv/.zlogin（macOS 终端默认 login）与
    /// bash 的 .bash_login 里的受监视变量 export 必须被检出——否则用户把
    /// CODEX_ACCESS_TOKEN 写这些文件里，切换被 auth.json 之外的 shell 值静默覆盖。
    #[test]
    fn rc_scan_covers_zsh_login_files() {
        for name in [".zprofile", ".zshenv", ".zlogin", ".bash_login"] {
            let home = temp_home(&format!("login{name}"));
            let _ = std::fs::write(home.join(name), "export CODEX_ACCESS_TOKEN=tok\n");
            let sources = rc_sources_setting(&home, "CODEX_ACCESS_TOKEN");
            assert_eq!(sources.len(), 1, "{name} 未被扫到: {sources:?}");
            assert!(sources[0].ends_with(name), "出处应指向 {name}: {sources:?}");
            let _ = std::fs::remove_dir_all(&home);
        }
    }

    /// fish 用独立 conf 目录，别漏扫。
    #[test]
    fn rc_scan_covers_fish_conf() {
        let home = temp_home("fish");
        let fish = super::super::store::fish_conf_path(&home);
        if let Some(parent) = fish.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&fish, "set -gx CODEX_API_KEY tok\n");

        let sources = rc_sources_setting(&home, "CODEX_API_KEY");
        assert_eq!(sources.len(), 1, "fish conf 未被扫到: {sources:?}");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// 干净的 home 不该报任何冲突——误报比漏报更伤信任。
    #[test]
    fn clean_home_reports_nothing() {
        let home = temp_home("clean");
        let _ = std::fs::write(home.join(".zshrc"), "export PATH=/usr/bin\n");
        for (var, _) in CODEX_OVERRIDE_VARS {
            assert!(
                rc_sources_setting(&home, var).is_empty(),
                "{var} 在干净环境被误报"
            );
        }
        let _ = std::fs::remove_dir_all(&home);
    }
}
