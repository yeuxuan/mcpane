//! Grok CLI 配置发现。
//!
//! 落盘位置（按 Grok CLI 实测，核对日期 2026-07-25）：
//! `~/.grok/config.toml` —— 模型表与默认模型指针都在这一个文件里。
//! Grok CLI 没有 `GROK_HOME` 之类的目录覆盖环境变量，目录固定 `~/.grok`。

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokConfigPaths {
    /// `~/.grok/config.toml`（模型表 + 默认模型指针）。
    pub config: PathBuf,
}

/// 发现配置路径。`home` 可注入（测试/多 Target）。
pub fn discover_config_paths(home: &Path) -> GrokConfigPaths {
    GrokConfigPaths {
        config: home.join(".grok").join("config.toml"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_is_rooted_at_dot_grok() {
        let paths = discover_config_paths(Path::new("/home/u"));
        assert_eq!(paths.config, PathBuf::from("/home/u/.grok/config.toml"));
    }
}
