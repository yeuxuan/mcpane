//! OpenClaw 配置发现：`~/.openclaw/openclaw.json`（内容是 JSON5）。

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenClawConfigPaths {
    /// `~/.openclaw/openclaw.json`（models.providers 表所在）。
    pub config: PathBuf,
}

pub fn discover_config_paths(home: &Path) -> OpenClawConfigPaths {
    OpenClawConfigPaths {
        config: home.join(".openclaw").join("openclaw.json"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_is_rooted_at_dot_openclaw() {
        assert_eq!(
            discover_config_paths(Path::new("/home/u")).config,
            PathBuf::from("/home/u/.openclaw/openclaw.json")
        );
    }
}
