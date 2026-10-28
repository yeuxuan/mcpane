//! Hermes 配置发现：`~/.hermes/config.yaml`。
//!
//! Windows 上 cc-switch 会退到 `%LOCALAPPDATA%\hermes`，这里同样处理。

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HermesConfigPaths {
    /// `~/.hermes/config.yaml`（custom_providers 数组所在）。
    pub config: PathBuf,
}

pub fn discover_config_paths(home: &Path) -> HermesConfigPaths {
    #[cfg(target_os = "windows")]
    let dir = std::env::var_os("LOCALAPPDATA")
        .map(|v| PathBuf::from(v).join("hermes"))
        .unwrap_or_else(|| home.join(".hermes"));
    #[cfg(not(target_os = "windows"))]
    let dir = home.join(".hermes");
    HermesConfigPaths {
        config: dir.join("config.yaml"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_lives_in_hermes_dir() {
        let paths = discover_config_paths(Path::new("/home/u"));
        assert!(paths.config.ends_with("config.yaml"), "{paths:?}");
    }
}
