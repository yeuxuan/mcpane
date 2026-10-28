//! Gemini CLI 安装检测。

use mcpane_hub_protocol::cli_path;

/// 见 [`cli_path::CliInstallation`]。
pub type GeminiInstallation = cli_path::CliInstallation;

/// 只定位 `gemini` 可执行文件，不跑 `--version`（判断"装没装"用它）。
///
/// 刻意不看 `~/.gemini` 目录：该目录会被 Antigravity 等其他 Google 工具创建，
/// 拿它当安装依据会把"没装 Gemini CLI"误判成已装。
pub fn locate_gemini() -> Option<std::path::PathBuf> {
    cli_path::locate_cli("gemini")
}

/// 检测 `gemini` 可执行文件与版本。返回 None 表示未安装。
pub fn detect_gemini() -> Option<GeminiInstallation> {
    cli_path::detect_cli("gemini")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsing_handles_common_formats() {
        assert_eq!(cli_path::parse_version("0.9.4").as_deref(), Some("0.9.4"));
        assert_eq!(
            cli_path::parse_version("gemini-cli 1.2.3\n").as_deref(),
            Some("1.2.3")
        );
        assert_eq!(cli_path::parse_version("no version here"), None);
    }
}
