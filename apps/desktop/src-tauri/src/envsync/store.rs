//! 凭据存储的纯文件层：`env.sh`（唯一事实源）与它的两个派生视图
//! `mcpane.fish` / `environment.d`（每次全量重生成）。
//!
//! `env.sh` 是 Unix 上 Token 明文的唯一长期落点（Hub 只存哈希，ADR-003）：
//! 0600 权限 + 同目录 temp+rename 原子写；逐行幂等替换，非 MCPANE 行原样
//! 保留（容忍用户手加内容）。

use std::path::{Path, PathBuf};

const ENV_FILE_NAME: &str = "env.sh";
const HEADER: &str = "# Managed by MCPane — MCPANE_* 行会在接入/轮换时被自动改写";
const FISH_HEADER: &str = "# Managed by MCPane — 本文件整体由 MCPane 生成，请勿手改";

pub(super) fn env_file_path(mcpane_home: &Path) -> PathBuf {
    mcpane_home.join(ENV_FILE_NAME)
}

pub(super) fn fish_conf_path(user_home: &Path) -> PathBuf {
    user_home.join(".config/fish/conf.d/mcpane.fish")
}

pub(super) fn environment_d_path(user_home: &Path) -> PathBuf {
    user_home.join(".config/environment.d/50-mcpane.conf")
}

/// POSIX 单引号转义：内嵌 `'` 写作 `'\''`。
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// fish 的单引号规则与 POSIX 不同：引号内支持 `\'` 与 `\\` 转义。
fn fish_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', r"\\").replace('\'', r"\'"))
}

/// 变量值必须能安全落入引号（Token 不可能合法包含换行/NUL，含则直接拒写）。
pub(super) fn validate_value(value: &str) -> Result<(), String> {
    if value.contains('\n') || value.contains('\r') || value.contains('\0') {
        return Err("凭据值包含换行或控制字符，拒绝写入环境变量存储".to_owned());
    }
    Ok(())
}

/// 读文件；不存在返回 None，其余 IO 错误上抛。
pub(super) fn read_optional(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("读取 {} 失败：{e}", path.display())),
    }
}

/// 同目录 temp + rename 原子写；Unix 下写前落 0600。
fn write_atomic_secret(path: &Path, content: &str) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} 缺少父目录", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("创建 {} 失败：{e}", dir.display()))?;
    let tmp = dir.join(format!(".{ENV_FILE_NAME}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, content).map_err(|e| format!("写入 {} 失败：{e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("设置 {} 权限失败：{e}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("落盘 {} 失败：{e}", path.display()))?;
    Ok(())
}

/// 幂等写入/更新一个变量，返回 env.sh 路径。
pub(super) fn upsert_var(mcpane_home: &Path, var: &str, value: &str) -> Result<PathBuf, String> {
    validate_value(value)?;
    let path = env_file_path(mcpane_home);
    let mut lines: Vec<String> = match read_optional(&path)? {
        Some(content) => content.lines().map(str::to_owned).collect(),
        None => vec![HEADER.to_owned()],
    };
    let prefix = format!("export {var}=");
    let line = format!("export {var}={}", shell_single_quote(value));
    if let Some(slot) = lines
        .iter_mut()
        .find(|l| l.trim_start().starts_with(&prefix))
    {
        *slot = line;
    } else {
        lines.push(line);
    }
    write_atomic_secret(&path, &(lines.join("\n") + "\n"))?;
    Ok(path)
}

/// 移除一个变量，返回剩余受管（`export MCPANE_*`）变量数；
/// 归零时删除 env.sh 本体（调用方据此触发全量清理）。
pub(super) fn remove_var(mcpane_home: &Path, var: &str) -> Result<usize, String> {
    let path = env_file_path(mcpane_home);
    let Some(content) = read_optional(&path)? else {
        return Ok(0);
    };
    let prefix = format!("export {var}=");
    let lines: Vec<&str> = content
        .lines()
        .filter(|l| !l.trim_start().starts_with(&prefix))
        .collect();
    let remaining = lines
        .iter()
        .filter(|l| l.trim_start().starts_with("export MCPANE_"))
        .count();
    if remaining == 0 {
        if let Err(e) = std::fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return Err(format!("删除 {} 失败：{e}", path.display()));
        }
    } else {
        write_atomic_secret(&path, &(lines.join("\n") + "\n"))?;
    }
    Ok(remaining)
}

/// `shell_single_quote` 的逆：解析 `'abc'` / `'ab'\''cd'` 形态。
fn parse_single_quoted(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut rest = raw.trim();
    loop {
        rest = rest.strip_prefix('\'')?;
        let end = rest.find('\'')?;
        out.push_str(&rest[..end]);
        rest = &rest[end + 1..];
        if rest.is_empty() {
            return Some(out);
        }
        rest = rest.strip_prefix("\\'")?;
        out.push('\'');
    }
}

/// 列出 env.sh 中全部受管变量（供派生视图重生成与会话重放）。
pub(super) fn list_vars(mcpane_home: &Path) -> Result<Vec<(String, String)>, String> {
    let Some(content) = read_optional(&env_file_path(mcpane_home))? else {
        return Ok(Vec::new());
    };
    let mut vars = Vec::new();
    for line in content.lines() {
        let Some(assign) = line.trim_start().strip_prefix("export MCPANE_") else {
            continue;
        };
        let Some((name_rest, raw_value)) = assign.split_once('=') else {
            continue;
        };
        if let Some(value) = parse_single_quoted(raw_value) {
            vars.push((format!("MCPANE_{name_rest}"), value));
        }
    }
    Ok(vars)
}

/// 全量重生成 fish 视图；无变量时删除文件。返回写入的路径（删除时 None）。
pub(super) fn write_fish_env(
    user_home: &Path,
    vars: &[(String, String)],
) -> Result<Option<PathBuf>, String> {
    let path = fish_conf_path(user_home);
    if vars.is_empty() {
        remove_if_exists(&path)?;
        return Ok(None);
    }
    let mut content = format!("{FISH_HEADER}\n");
    for (var, value) in vars {
        content.push_str(&format!("set -gx {var} {}\n", fish_single_quote(value)));
    }
    write_atomic_secret(&path, &content)?;
    Ok(Some(path))
}

/// 全量重生成 systemd user session 视图（Linux GUI，登录时生效）。
pub(super) fn write_environment_d(
    user_home: &Path,
    vars: &[(String, String)],
) -> Result<Option<PathBuf>, String> {
    let path = environment_d_path(user_home);
    if vars.is_empty() {
        remove_if_exists(&path)?;
        return Ok(None);
    }
    let mut content = String::from("# Managed by MCPane\n");
    for (var, value) in vars {
        let escaped = value.replace('\\', r"\\").replace('"', "\\\"");
        content.push_str(&format!("{var}=\"{escaped}\"\n"));
    }
    write_atomic_secret(&path, &content)?;
    Ok(Some(path))
}

fn remove_if_exists(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("删除 {} 失败：{e}", path.display())),
    }
}
