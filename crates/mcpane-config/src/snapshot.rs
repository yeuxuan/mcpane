//! 配置快照：写入前备份原文件，回滚前备份当前文件。
//!
//! P0 使用文件系统存储（`<root>/config-snapshots/`）；Phase 4 迁到 SQLite
//! 时保持本 trait 不变。快照内容支持保存前脱敏（§3.3）。

use std::path::{Path, PathBuf};

use crate::document::content_hash;
use crate::error::ConfigError;

/// 以仅属主可读写（unix 0600）创建并写入文件，消除「先按 umask 写入、
/// 再 chmod」之间的世界可读窗口。快照留存写前全文，可能含用户自有 secret
///（脱敏只覆盖 MCPane 管理字段），绝不能以 0644 堆在 config-snapshots/ 里。
/// 快照不经 FsOps 抽象，直接调共享实现（写前全文可能含用户 secret）。
fn write_private(path: &Path, content: &str) -> std::io::Result<()> {
    crate::permissions::write_owner_only(path, content)
}

/// 快照元信息。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotMeta {
    pub id: String,
    /// 被快照文件的原路径。
    pub source_path: PathBuf,
    /// 快照内容哈希（脱敏后）。
    pub content_hash: String,
    pub created_unix_secs: u64,
    /// 快照文件落盘位置。
    pub stored_at: PathBuf,
}

pub trait SnapshotStore: Send + Sync {
    /// 保存一份快照，返回元信息。`redact` 在落盘前应用（如剔除已知 Secret 字段）。
    fn save(
        &self,
        source_path: &Path,
        content: &str,
        redact: Option<&dyn Fn(&str) -> String>,
    ) -> Result<SnapshotMeta, ConfigError>;

    /// 读取快照内容。
    fn load(&self, meta: &SnapshotMeta) -> Result<String, ConfigError>;
}

/// 保留的快照上限（内容 + meta 成对计数）。超限时按时间戳淘汰最旧的。
const MAX_SNAPSHOTS: usize = 100;

/// 文件系统快照存储。
pub struct FsSnapshotStore {
    root: PathBuf,
}

impl FsSnapshotStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn dir(&self) -> PathBuf {
        self.root.join("config-snapshots")
    }

    /// 淘汰最旧快照，保留最近 `MAX_SNAPSHOTS` 份。失败只记日志不阻塞写入。
    fn prune(&self, dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        // 快照文件名以创建秒级时间戳开头（`{created}-{id}-{stem}`），meta 文件不计数。
        let mut snapshots: Vec<(u64, PathBuf)> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && !p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with(".meta.json"))
            })
            .filter_map(|p| {
                let created = p
                    .file_name()?
                    .to_str()?
                    .split('-')
                    .next()?
                    .parse::<u64>()
                    .ok()?;
                Some((created, p))
            })
            .collect();
        if snapshots.len() <= MAX_SNAPSHOTS {
            return;
        }
        snapshots.sort_by_key(|(created, _)| *created);
        for (_, path) in snapshots
            .iter()
            .take(snapshots.len().saturating_sub(MAX_SNAPSHOTS))
        {
            if let Err(e) = std::fs::remove_file(path) {
                tracing::warn!(path = %path.display(), error = %e, "清理过期快照失败");
            }
            let _ = std::fs::remove_file(path.with_extension("meta.json"));
        }
    }
}

impl SnapshotStore for FsSnapshotStore {
    fn save(
        &self,
        source_path: &Path,
        content: &str,
        redact: Option<&dyn Fn(&str) -> String>,
    ) -> Result<SnapshotMeta, ConfigError> {
        let stored_content = match redact {
            Some(f) => f(content),
            None => content.to_owned(),
        };
        let id = uuid::Uuid::new_v4().simple().to_string();
        let created = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let file_stem = source_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config");
        let dir = self.dir();
        std::fs::create_dir_all(&dir).map_err(|e| ConfigError::io("创建快照目录", e))?;
        let stored_at = dir.join(format!("{created}-{id}-{file_stem}"));
        write_private(&stored_at, &stored_content).map_err(|e| ConfigError::io("写入快照", e))?;

        // 快照元信息旁路存储，便于回滚列表枚举。
        let meta = SnapshotMeta {
            id,
            source_path: source_path.to_path_buf(),
            content_hash: content_hash(&stored_content),
            created_unix_secs: created,
            stored_at: stored_at.clone(),
        };
        let meta_path = stored_at.with_extension("meta.json");
        let meta_json = serde_json::to_string_pretty(&meta)
            .map_err(|e| ConfigError::Snapshot(e.to_string()))?;
        // 与快照本体同一份 write_private：meta 挨着 0600 的快照内容躺在同一个
        // 目录里，若自己仍走默认 umask 落盘，保护水位不一致——且 meta 里的
        // source_path/content_hash 也不该比它描述的快照本体更公开。
        write_private(&meta_path, &meta_json).map_err(|e| ConfigError::io("写入快照元信息", e))?;
        self.prune(&dir);
        Ok(meta)
    }

    fn load(&self, meta: &SnapshotMeta) -> Result<String, ConfigError> {
        std::fs::read_to_string(&meta.stored_at).map_err(|e| ConfigError::io("读取快照", e))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn snapshot_roundtrip_with_redaction() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsSnapshotStore::new(dir.path());
        let secret_content = r#"{"apiKey": "sk-super-secret"}"#;
        let meta = store
            .save(
                Path::new("/tmp/example.json"),
                secret_content,
                Some(&|s: &str| s.replace("sk-super-secret", "[REDACTED]")),
            )
            .unwrap();
        let loaded = store.load(&meta).unwrap();
        assert!(!loaded.contains("sk-super-secret"), "快照必须脱敏");
        assert!(loaded.contains("[REDACTED]"));
    }

    /// 快照内容与紧挨着它的 `.meta.json` 必须同等收紧为 0600：meta 此前走
    /// `std::fs::write` 默认 umask 落盘，与同目录快照本体的保护水位不一致。
    #[cfg(unix)]
    #[test]
    fn meta_json_is_hardened_like_the_snapshot_it_describes() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let store = FsSnapshotStore::new(dir.path());
        let meta = store
            .save(Path::new("/tmp/example.json"), "{}", None)
            .unwrap();
        let meta_path = meta.stored_at.with_extension("meta.json");

        let snapshot_mode = std::fs::metadata(&meta.stored_at)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let meta_mode = std::fs::metadata(&meta_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(snapshot_mode, 0o600, "快照本体应为 0600");
        assert_eq!(meta_mode, 0o600, "meta.json 应与快照本体同等收紧为 0600");
    }
}
