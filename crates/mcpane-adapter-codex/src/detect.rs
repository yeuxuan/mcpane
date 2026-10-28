//! Codex 安装检测。

use mcpane_hub_protocol::cli_path;

/// 见 [`cli_path::CliInstallation`]。
pub type CodexInstallation = cli_path::CliInstallation;

/// 只定位 `codex` 可执行文件，不跑 `--version`（判断"装没装"用它）。
pub fn locate_codex() -> Option<std::path::PathBuf> {
    cli_path::locate_cli("codex")
}

/// 检测 `codex` 可执行文件与版本。返回 None 表示未安装。
pub fn detect_codex() -> Option<CodexInstallation> {
    cli_path::detect_cli("codex")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsing() {
        assert_eq!(
            cli_path::parse_version("codex-cli 0.133.0").as_deref(),
            Some("0.133.0")
        );
        assert_eq!(cli_path::parse_version("garbage"), None);
    }
}
