//! Gemini CLI 配置发现。
//!
//! 落盘位置（以 Gemini CLI 官方文档与实测为准，核对日期 2026-07-20）：
//! - `~/.gemini/.env`：Provider env（API Key / base_url / model）；
//! - `~/.gemini/settings.json`：CLI 设置（auth 类型、mcpServers 等）；
//! - `~/.gemini/oauth_creds.json`：官方 OAuth 凭据（CLI 自管，MCPane 不写入）。
//!
//! Gemini CLI 没有 `GEMINI_HOME` 之类的目录覆盖环境变量，目录固定 `~/.gemini`。

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiConfigPaths {
    /// `~/.gemini/.env`（Provider 受管键）。
    pub env_file: PathBuf,
    /// `~/.gemini/settings.json`（auth 类型 + 附加配置片段目标）。
    pub settings: PathBuf,
    /// `~/.gemini/oauth_creds.json`（只读参考；切换流程不触碰）。
    pub oauth_creds: PathBuf,
}

/// 发现配置路径。`home` 可注入（测试/多 Target）。
pub fn discover_config_paths(home: &Path) -> GeminiConfigPaths {
    let dir = home.join(".gemini");
    GeminiConfigPaths {
        env_file: dir.join(".env"),
        settings: dir.join("settings.json"),
        oauth_creds: dir.join("oauth_creds.json"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_rooted_at_dot_gemini() {
        let paths = discover_config_paths(Path::new("/home/u"));
        assert_eq!(paths.env_file, PathBuf::from("/home/u/.gemini/.env"));
        assert_eq!(
            paths.settings,
            PathBuf::from("/home/u/.gemini/settings.json")
        );
        assert_eq!(
            paths.oauth_creds,
            PathBuf::from("/home/u/.gemini/oauth_creds.json")
        );
    }
}
