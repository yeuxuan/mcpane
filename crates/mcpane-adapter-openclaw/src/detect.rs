//! OpenClaw 安装检测。

use mcpane_hub_protocol::cli_path;

/// 见 [`cli_path::CliInstallation`]。
pub type OpenClawInstallation = cli_path::CliInstallation;

/// 只定位 `openclaw` 可执行文件，不跑 `--version`（判断"装没装"用它）。
pub fn locate_openclaw() -> Option<std::path::PathBuf> {
    cli_path::locate_cli("openclaw")
}

/// 检测 `openclaw` 可执行文件与版本。返回 None 表示未安装。
pub fn detect_openclaw() -> Option<OpenClawInstallation> {
    cli_path::detect_cli("openclaw")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_semver_from_version_output() {
        assert_eq!(
            cli_path::parse_version("openclaw 1.2.3"),
            Some("1.2.3".to_owned())
        );
        assert_eq!(cli_path::parse_version("no version here"), None);
    }
}
