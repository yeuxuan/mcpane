//! OpenCode 配置发现。
//!
//! 落盘位置（按 OpenCode 官方文档与实测，核对日期 2026-07-25）：
//! `$XDG_CONFIG_HOME/opencode/opencode.json`，未设 XDG 时退到 `~/.config/opencode/`。
//! 注意不是 `~/.opencode`——OpenCode 走 XDG 规范，放错地方它读不到。

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeConfigPaths {
    /// `opencode.json`（provider 表 + mcp 表都在这一份里）。
    pub config: PathBuf,
}

/// 发现配置路径。`home` 可注入（测试/多 Target）。
/// `XDG_CONFIG_HOME` 优先——用户改过它的话，`~/.config` 那份是死文件。
pub fn discover_config_paths(home: &Path) -> OpenCodeConfigPaths {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    OpenCodeConfigPaths {
        config: dir.join("opencode").join("opencode.json"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_to_dot_config_under_home() {
        // 该测试不设 XDG_CONFIG_HOME 时才有意义；设了就走那条分支（下面的断言二选一）。
        let paths = discover_config_paths(Path::new("/home/u"));
        assert!(
            paths.config.ends_with("opencode/opencode.json"),
            "{paths:?}"
        );
        if std::env::var_os("XDG_CONFIG_HOME").is_none() {
            assert_eq!(
                paths.config,
                PathBuf::from("/home/u/.config/opencode/opencode.json")
            );
        }
    }
}
