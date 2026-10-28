//! Claude Code 配置发现与 MCP scope 识别。
//!
//! 落盘位置（以 Claude Code 官方文档为准，核对日期 2026-07-11）：
//! - User scope：`~/.claude.json` 顶层 `mcpServers`
//! - Project scope：`<project>/.mcp.json` 顶层 `mcpServers`（入库共享）
//! - Local scope：`~/.claude.json` 中 `projects.<abs-path>.mcpServers`
//! - 用户设置（Provider env 等）：`~/.claude/settings.json`

use std::path::{Path, PathBuf};

/// MCP Server 配置的作用域。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpScope {
    User,
    Project,
    Local,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeConfigPaths {
    /// `~/.claude.json`（User/Local scope MCP + 应用状态）。
    pub user_config: PathBuf,
    /// `~/.claude/settings.json`（Provider env 等用户设置）。
    pub user_settings: PathBuf,
    /// `<project>/.mcp.json`（Project scope MCP；仅当提供项目目录时）。
    pub project_mcp: Option<PathBuf>,
}

/// `CLAUDE_CONFIG_DIR`：Claude Code 官方支持的配置目录覆盖 env（语义对齐
/// Codex 侧的 `CODEX_HOME`）。设了它，`~/.claude` 整体换成该目录，
/// 且 `.claude.json` 一并移入（`$dir/.claude.json`，不再在 $HOME 侧根）。
pub fn config_dir_override() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Claude 配置目录：env 覆盖优先，缺省 `~/.claude`。
/// （插件 config.json、用量 projects 目录等所有目录内文件都应由此推导。）
pub fn claude_config_dir(home: &Path) -> PathBuf {
    config_dir_override().unwrap_or_else(|| home.join(".claude"))
}

/// 发现配置路径。`home` 与 `project_dir` 可注入（测试/多 Target）。
pub fn discover_config_paths(home: &Path, project_dir: Option<&Path>) -> ClaudeConfigPaths {
    discover_config_paths_with(home, project_dir, config_dir_override().as_deref())
}

/// 纯函数版（env 注入为参数，测试不碰进程环境）。
pub fn discover_config_paths_with(
    home: &Path,
    project_dir: Option<&Path>,
    config_dir: Option<&Path>,
) -> ClaudeConfigPaths {
    let (user_config, user_settings) = match config_dir {
        // 覆盖目录：.claude.json 移入目录内（与 Claude Code 实际行为一致）。
        Some(dir) => (dir.join(".claude.json"), dir.join("settings.json")),
        None => (
            home.join(".claude.json"),
            home.join(".claude").join("settings.json"),
        ),
    };
    ClaudeConfigPaths {
        user_config,
        user_settings,
        project_mcp: project_dir.map(|d| d.join(".mcp.json")),
    }
}

impl ClaudeConfigPaths {
    /// 某 scope 的 MCP 配置落在哪个文件、哪个 JSON 前缀路径下。
    pub fn mcp_location(
        &self,
        scope: &McpScope,
        project_dir: Option<&Path>,
    ) -> Option<(PathBuf, Vec<String>)> {
        match scope {
            McpScope::User => Some((self.user_config.clone(), vec!["mcpServers".into()])),
            McpScope::Project => self
                .project_mcp
                .clone()
                .map(|p| (p, vec!["mcpServers".into()])),
            McpScope::Local => {
                let project = project_dir?;
                Some((
                    self.user_config.clone(),
                    vec![
                        "projects".into(),
                        project.to_string_lossy().into_owned(),
                        "mcpServers".into(),
                    ],
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn config_dir_override_moves_both_files_into_dir() {
        // 覆盖目录下 .claude.json 在目录**内**（不是 $HOME 侧根）——认错位置
        // 会让 MCP User scope 写进 Claude Code 不读的文件。
        let paths = discover_config_paths_with(
            Path::new("/Users/me"),
            None,
            Some(Path::new("/custom/claude-cfg")),
        );
        assert_eq!(
            paths.user_config,
            PathBuf::from("/custom/claude-cfg/.claude.json")
        );
        assert_eq!(
            paths.user_settings,
            PathBuf::from("/custom/claude-cfg/settings.json")
        );
        // 未覆盖：维持经典布局。
        let default_paths = discover_config_paths_with(Path::new("/Users/me"), None, None);
        assert_eq!(
            default_paths.user_config,
            PathBuf::from("/Users/me/.claude.json")
        );
        assert_eq!(
            default_paths.user_settings,
            PathBuf::from("/Users/me/.claude/settings.json")
        );
    }

    #[test]
    fn scope_locations_are_distinct() {
        let paths = discover_config_paths(Path::new("/Users/me"), Some(Path::new("/repo")));
        let (user_file, user_prefix) = paths.mcp_location(&McpScope::User, None).unwrap();
        assert_eq!(user_file, PathBuf::from("/Users/me/.claude.json"));
        assert_eq!(user_prefix, vec!["mcpServers"]);

        let (proj_file, _) = paths
            .mcp_location(&McpScope::Project, Some(Path::new("/repo")))
            .unwrap();
        assert_eq!(proj_file, PathBuf::from("/repo/.mcp.json"));

        let (local_file, local_prefix) = paths
            .mcp_location(&McpScope::Local, Some(Path::new("/repo")))
            .unwrap();
        assert_eq!(local_file, PathBuf::from("/Users/me/.claude.json"));
        assert_eq!(local_prefix, vec!["projects", "/repo", "mcpServers"]);
    }
}
