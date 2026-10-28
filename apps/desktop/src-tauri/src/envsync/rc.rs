//! shell rc 注入层：按 `$SHELL` 决定目标 rc，追加/更新一行带标记的
//! `source`，让 CLI 客户端（Claude Code / Codex）的新终端自动加载 env.sh。
//!
//! 注入行固定追加在文件末尾——这决定了同名变量的覆盖语义：env.sh 覆盖
//! 用户在前面手写的 export；用户写在标记行之后则用户赢（视为显式覆盖）。
//! 写 rc 用 `fs::write`（跟随符号链接）而非 rename：dotfile 管理器
//! （stow/chezmoi）的 rc 常是软链，rename 会把软链替换成普通文件。

use std::path::{Path, PathBuf};

use super::HostOs;
use super::store::read_optional;

pub(super) const MARKER: &str = "# mcpane:env";

/// 需要注入的 rc 家族；fish 不兼容 POSIX `export`，走独立 conf.d 文件。
pub(super) enum RcTargets {
    Posix(Vec<PathBuf>),
    Fish,
}

pub(super) fn rc_targets(user_home: &Path, shell: Option<&str>, os: HostOs) -> RcTargets {
    match shell.unwrap_or_default() {
        "zsh" => RcTargets::Posix(vec![user_home.join(".zshrc")]),
        "bash" => {
            let mut targets = vec![user_home.join(".bashrc")];
            // macOS 终端默认开 login shell，login bash 不读 .bashrc。
            if os == HostOs::MacOs {
                targets.push(user_home.join(".bash_profile"));
            }
            RcTargets::Posix(targets)
        }
        "fish" => RcTargets::Fish,
        _ => RcTargets::Posix(vec![user_home.join(".profile")]),
    }
}

/// 清理时要扫的全部候选 rc（不依赖当前 `$SHELL`——用户可能换过 shell）。
/// 只含 MCPane 会注入的 rc 家族（见 `rc_targets`）——清理即删这些文件里的注入行。
pub(super) fn all_candidate_rcs(user_home: &Path) -> Vec<PathBuf> {
    [".zshrc", ".bashrc", ".bash_profile", ".profile"]
        .iter()
        .map(|name| user_home.join(name))
        .collect()
}

/// 冲突检测要扫的 rc 全集：注入候选 + 用户可能手写 `export` 的其余 shell 启动文件。
/// 比 `all_candidate_rcs` 更广——macOS 终端默认 login shell，zsh login 会读
/// `.zshenv`（所有 shell 都读）/`.zprofile`/`.zlogin`，bash login 会读 `.bash_login`；
/// 用户把 `ANTHROPIC_*` 写这些文件里同样会覆盖切换结果，漏扫即静默失效。
pub(super) fn all_conflict_scan_rcs(user_home: &Path) -> Vec<PathBuf> {
    let mut rcs = all_candidate_rcs(user_home);
    for name in [".zshenv", ".zprofile", ".zlogin", ".bash_login"] {
        rcs.push(user_home.join(name));
    }
    rcs
}

/// 双引号包裹路径（MCPANE_HOME 的 macOS 默认值含空格），转义 shell 特殊字符。
fn double_quote(path: &Path) -> String {
    let escaped = path
        .display()
        .to_string()
        .replace('\\', r"\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`");
    format!("\"{escaped}\"")
}

/// 生成注入行：`[ -f ... ]` 守卫让 env.sh 被删后残留行无害空转。
pub(super) fn source_line(env_file: &Path) -> String {
    let quoted = double_quote(env_file);
    format!("[ -f {quoted} ] && . {quoted} {MARKER}")
}

pub(super) enum Injection {
    Added,
    Updated,
    Unchanged,
}

/// 幂等注入：有标记行且内容一致→跳过；不一致（MCPANE_HOME 变了）→整行替换；
/// 无→末尾追加。rc 不存在则创建。
pub(super) fn ensure_source_line(rc_path: &Path, env_file: &Path) -> Result<Injection, String> {
    let desired = source_line(env_file);
    let content = read_optional(rc_path)?.unwrap_or_default();
    let mut lines: Vec<String> = content.lines().map(str::to_owned).collect();
    let injection = if let Some(pos) = lines.iter().position(|l| l.trim_end().ends_with(MARKER)) {
        if lines[pos] == desired {
            return Ok(Injection::Unchanged);
        }
        lines[pos] = desired;
        Injection::Updated
    } else {
        lines.push(desired);
        Injection::Added
    };
    std::fs::write(rc_path, lines.join("\n") + "\n")
        .map_err(|e| format!("写入 {} 失败：{e}", rc_path.display()))?;
    Ok(injection)
}

/// 移除标记行；返回是否发生了改写。
pub(super) fn remove_source_line(rc_path: &Path) -> Result<bool, String> {
    let Some(content) = read_optional(rc_path)? else {
        return Ok(false);
    };
    let lines: Vec<&str> = content
        .lines()
        .filter(|l| !l.trim_end().ends_with(MARKER))
        .collect();
    if lines.len() == content.lines().count() {
        return Ok(false);
    }
    let body = if lines.is_empty() {
        String::new()
    } else {
        lines.join("\n") + "\n"
    };
    std::fs::write(rc_path, body).map_err(|e| format!("写入 {} 失败：{e}", rc_path.display()))?;
    Ok(true)
}
