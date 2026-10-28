//! Claude Code 安装检测。

use mcpane_hub_protocol::cli_path;

/// 见 [`cli_path::CliInstallation`]。
pub type ClaudeInstallation = cli_path::CliInstallation;

/// 只定位 `claude` 可执行文件，不跑 `--version`。
/// 判断"装没装"用它——启动路径上不该为此 spawn 一个 Node 进程。
pub fn locate_claude() -> Option<std::path::PathBuf> {
    cli_path::locate_cli("claude")
}

/// 检测 `claude` 可执行文件与版本。返回 None 表示未安装。
pub fn detect_claude() -> Option<ClaudeInstallation> {
    cli_path::detect_cli("claude")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsing_handles_common_formats() {
        assert_eq!(
            cli_path::parse_version("2.1.207 (Claude Code)").as_deref(),
            Some("2.1.207")
        );
        assert_eq!(
            cli_path::parse_version("claude 1.0.0"),
            Some("1.0.0".to_owned())
        );
        assert_eq!(cli_path::parse_version("no version here"), None);
    }
}
