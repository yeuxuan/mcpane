//! Hermes 安装检测。

use mcpane_hub_protocol::cli_path;

/// 见 [`cli_path::CliInstallation`]。
pub type HermesInstallation = cli_path::CliInstallation;

/// 只定位 `hermes` 可执行文件，不跑 `--version`（判断"装没装"用它）。
pub fn locate_hermes() -> Option<std::path::PathBuf> {
    cli_path::locate_cli("hermes")
}

/// 检测 `hermes` 可执行文件与版本。返回 None 表示未安装。
pub fn detect_hermes() -> Option<HermesInstallation> {
    cli_path::detect_cli("hermes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_semver_from_version_output() {
        assert_eq!(
            cli_path::parse_version("hermes 1.2.3"),
            Some("1.2.3".to_owned())
        );
        // Hermes 是 Python 包，输出可能带 v 前缀或走 click 的 "name, version X" 格式。
        // 解析不出版本会被本地环境面板误报成"装了但跑不起来"，所以这里放宽。
        assert_eq!(
            cli_path::parse_version("hermes v1.2.3"),
            Some("1.2.3".to_owned())
        );
        assert_eq!(
            cli_path::parse_version("hermes, version 1.2.3"),
            Some("1.2.3".to_owned())
        );
        assert_eq!(cli_path::parse_version("no version here"), None);
        assert_eq!(cli_path::parse_version("build abc.def"), None);
    }
}
