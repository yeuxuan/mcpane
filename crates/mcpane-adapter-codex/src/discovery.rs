//! Codex 配置发现。
//!
//! 落盘位置（以 Codex 官方文档为准，核对日期 2026-07-11）：
//! - 用户级：`$CODEX_HOME/config.toml`（默认 `~/.codex/config.toml`）
//! - 项目级：`<project>/.codex/config.toml`（仅受信任项目生效）

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexConfigPaths {
    pub user_config: PathBuf,
    /// 凭据文件（官方 OAuth 登录缓存或 API Key），同在 `$CODEX_HOME` 下。
    pub auth_json: PathBuf,
    pub project_config: Option<PathBuf>,
}

/// 发现配置路径。`codex_home` 遵循 `CODEX_HOME` 语义（默认 `<home>/.codex`）。
pub fn discover_config_paths(
    home: &Path,
    codex_home_override: Option<&Path>,
    project_dir: Option<&Path>,
) -> CodexConfigPaths {
    let codex_home = codex_home_override
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.join(".codex"));
    CodexConfigPaths {
        user_config: codex_home.join("config.toml"),
        auth_json: codex_home.join("auth.json"),
        project_config: project_dir.map(|d| d.join(".codex").join("config.toml")),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn codex_home_override_wins() {
        let paths = discover_config_paths(
            Path::new("/Users/me"),
            Some(Path::new("/custom/codex-home")),
            Some(Path::new("/repo")),
        );
        assert_eq!(
            paths.user_config,
            PathBuf::from("/custom/codex-home/config.toml")
        );
        assert_eq!(
            paths.auth_json,
            PathBuf::from("/custom/codex-home/auth.json")
        );
        assert_eq!(
            paths.project_config.unwrap(),
            PathBuf::from("/repo/.codex/config.toml")
        );
    }
}
