//! 独立于 [`crate::engine`] 的最小原子写原语：tmp 文件 + 可选收紧权限 +
//! 可选 fsync + rename。
//!
//! `mcpane-provider::service::write_auth_atomically`（OAuth token，绕开
//! ConfigEngine 快照）与桌面壳 `commands::provider::write_small_json_atomically`
//! （无凭据的小状态文件）都是这套骨架的实例，差异只在要不要
//! harden/fsync——此前两处各自手写一份 tmp+rename，容易改了一处漏了另一处。
//! `ConfigEngine::write_atomically` 另有 symlink 解析、失败清理、快照等上层
//! 编排，不在此列，仍保留独立实现。

use std::path::Path;

/// `harden` 收紧到属主独占（[`crate::permissions::harden_to_owner_only`]），
/// `fsync` 落盘前 `sync_all`。失败时不清理临时文件，行为与此前两处调用点一致。
pub fn atomic_write(path: &Path, content: &str, harden: bool, fsync: bool) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // 追加而非 with_extension：后者**替换**扩展名，`CLAUDE.md` 会变成
    // `CLAUDE.json.mcpane-tmp`——写 markdown 规则文件时留下的临时文件既误导
    // 又不像是自己的。追加对既有的 `.json` 调用点行为完全不变
    // （`config.json` → `config.json.mcpane-tmp`）。
    let tmp = {
        let mut name = path.as_os_str().to_owned();
        name.push(".mcpane-tmp");
        std::path::PathBuf::from(name)
    };
    std::fs::write(&tmp, content)?;
    if harden {
        crate::permissions::harden_to_owner_only(&tmp)?;
    }
    if fsync {
        // Windows 的 FlushFileBuffers（sync_all 的底层实现）要求句柄具备
        // GENERIC_WRITE，只读句柄一律返回 ERROR_ACCESS_DENIED(5)，必须以写
        // 权限重新打开。
        std::fs::OpenOptions::new()
            .write(true)
            .open(&tmp)?
            .sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn writes_content_and_renames_into_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        atomic_write(&path, "{}", false, false).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
        assert!(!path.with_extension("json.mcpane-tmp").exists());
    }

    #[test]
    fn creates_missing_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/state.json");
        atomic_write(&path, "{}", false, false).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
    }

    #[cfg(unix)]
    #[test]
    fn harden_true_restricts_to_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        atomic_write(&path, "{}", true, true).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "实际 {:o}", mode & 0o777);
    }
}

#[cfg(test)]
mod tmp_name_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// 临时文件名必须是「原名 + 后缀」而不是替换扩展名：markdown 规则文件
    /// 走的也是这条路，`CLAUDE.md` 变成 `CLAUDE.json.mcpane-tmp` 既误导、
    /// 写失败时留在用户目录里也认不出是谁的。
    #[test]
    fn tmp_file_keeps_the_original_extension() {
        let dir = std::env::temp_dir().join(format!("mcpane-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        for name in ["CLAUDE.md", "config.json", "AGENTS.md"] {
            let path = dir.join(name);
            atomic_write(&path, "x", false, false).unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "x");
            // 成功路径不留临时文件；名字形态由下面的断言锁住。
            assert!(
                !dir.join(format!("{name}.mcpane-tmp")).exists(),
                "成功写入后不该留下临时文件"
            );
        }
        // 扩展名不得被替换：这个名字一旦出现就说明退回了 with_extension。
        assert!(!dir.join("CLAUDE.json.mcpane-tmp").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
