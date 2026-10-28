//! Provider 状态持久化（P0：JSON 文件；Phase 4 迁 SQLite `provider_profiles`）。

use std::path::PathBuf;

use crate::domain::{PROFILE_SCHEMA_VERSION, ProviderState};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("IO 错误：{0}")]
    Io(#[from] std::io::Error),
    #[error("序列化错误：{0}")]
    Serde(#[from] serde_json::Error),
    #[error("Profile 不存在：{0}")]
    NotFound(String),
}

pub trait ProviderStore: Send + Sync {
    fn load(&self) -> Result<ProviderState, StoreError>;
    fn save(&self, state: &ProviderState) -> Result<(), StoreError>;
}

/// JSON 文件存储（`<root>/providers.json`）。
pub struct JsonProviderStore {
    path: PathBuf,
}

impl JsonProviderStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            path: root.into().join("providers.json"),
        }
    }

    /// 把不可识别的 providers.json 改名留档（`providers.json.corrupt-<unix秒>`），
    /// 返回留档路径。同秒重复留档时追加序号，绝不覆盖已有留档。
    fn quarantine(&self) -> Result<PathBuf, StoreError> {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut archive = self
            .path
            .with_file_name(format!("providers.json.corrupt-{ts}"));
        let mut n = 2u32;
        while archive.exists() {
            archive = self
                .path
                .with_file_name(format!("providers.json.corrupt-{ts}-{n}"));
            n += 1;
        }
        std::fs::rename(&self.path, &archive)?;
        Ok(archive)
    }
}

impl ProviderStore for JsonProviderStore {
    fn load(&self) -> Result<ProviderState, StoreError> {
        if !self.path.exists() {
            return Ok(ProviderState::default());
        }
        let content = std::fs::read_to_string(&self.path)?;
        // v1（裸数组/旧字段）或损坏内容重置为空态（项目未发布，不做迁移），
        // 但原文件必须先改名留档：state 里有 Codex 官方登录备份的唯一引用，
        // "损坏"可能只是半写/位翻转，静默用空态覆盖等于销毁数据。
        // 留档（rename）失败时宁可报错也不返回空态——空态一旦被 save 就覆盖原文件。
        match serde_json::from_str::<ProviderState>(&content) {
            Ok(state) if state.schema_version == PROFILE_SCHEMA_VERSION => Ok(state),
            Ok(state) => {
                let archive = self.quarantine()?;
                tracing::warn!(
                    schema_version = state.schema_version,
                    archive = %archive.display(),
                    "providers.json schema 版本不受支持，原文件已留档，重置为空态"
                );
                Ok(ProviderState::default())
            }
            Err(error) => {
                let archive = self.quarantine()?;
                tracing::warn!(
                    %error,
                    archive = %archive.display(),
                    "providers.json 无法解析（v1 或损坏），原文件已留档，重置为空态"
                );
                Ok(ProviderState::default())
            }
        }
    }

    fn save(&self, state: &ProviderState) -> Result<(), StoreError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(state)?;
        // 原子写：state 里保存着 Codex 官方登录备份的引用，写坏即丢失，
        // 用 temp + fsync + rename 保证要么旧态要么新态。
        let tmp = self.path.with_extension("json.tmp");
        // providers.json 含 desktopGateway.token 明文，创建即 0600（db/快照/备份
        // 都已收紧，此前唯独这份漏了）。旧 tmp 先清：0600 只在创建时生效。
        let _ = std::fs::remove_file(&tmp);
        mcpane_config::permissions::write_owner_only(&tmp, &content)?;
        // 以写权限打开才能 fsync：Windows 的 FlushFileBuffers 要求 GENERIC_WRITE，
        // 只读句柄返回 ERROR_ACCESS_DENIED(5)。详见 mcpane-config 的 RealFs::fsync。
        std::fs::OpenOptions::new()
            .write(true)
            .open(&tmp)?
            .sync_all()?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::domain::{ClientApp, ProviderProfile};

    /// 留档文件列表（load 后应恰有一份，且内容为原文）。
    fn corrupt_archives(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("providers.json.corrupt-"))
            })
            .collect()
    }

    #[test]
    fn v1_array_content_is_reset_but_archived() {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonProviderStore::new(dir.path());
        let original = r#"[{"schemaVersion":1,"id":"x","name":"旧","kind":"anthropic"}]"#;
        std::fs::write(dir.path().join("providers.json"), original).unwrap();
        assert_eq!(store.load().unwrap(), ProviderState::default());
        // 原文件已改名留档，内容原样保留；save 空态不会再覆盖它。
        let archives = corrupt_archives(dir.path());
        assert_eq!(archives.len(), 1);
        assert_eq!(std::fs::read_to_string(&archives[0]).unwrap(), original);
        assert!(!dir.path().join("providers.json").exists());
    }

    #[test]
    fn corrupted_content_is_archived_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonProviderStore::new(dir.path());
        let corrupted = r#"{"schemaVersion":2,"providers":[{"id":"trunc"#; // 半写
        std::fs::write(dir.path().join("providers.json"), corrupted).unwrap();
        assert_eq!(store.load().unwrap(), ProviderState::default());
        // 随后的 save（空态）落的是新文件，损坏原文仍在留档里可人工恢复。
        store.save(&ProviderState::default()).unwrap();
        let archives = corrupt_archives(dir.path());
        assert_eq!(archives.len(), 1);
        assert_eq!(std::fs::read_to_string(&archives[0]).unwrap(), corrupted);
    }

    #[test]
    fn roundtrip_v2_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonProviderStore::new(dir.path());
        let mut state = ProviderState::default();
        state
            .providers
            .push(ProviderProfile::new_relay(ClientApp::Codex, "R"));
        state.current.insert(ClientApp::Codex, "abc".into());
        store.save(&state).unwrap();
        assert_eq!(store.load().unwrap(), state);
    }
}
