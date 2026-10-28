//! 同步 manifest：远端快照的自描述元数据。

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const PROTOCOL_FORMAT: &str = "mcpane-sync";
pub const PROTOCOL_VERSION: u32 = 1;

/// artifact 角色（下载侧按角色定位；名称是内容寻址的，不可硬编码）。
pub const ROLE_DB: &str = "db";
pub const ROLE_PROVIDERS: &str = "providers";

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactMeta {
    /// "db" | "providers"。
    pub role: String,
    /// 内容寻址文件名（含内容摘要前缀，如 `mcpane-3fa2….db`）：
    /// 并发上传时不同内容绝不同名互覆，manifest 条件写是唯一提交点。
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// 恒 "mcpane-sync"（拒绝解析异构远端目录）。
    pub format: String,
    /// 协议版本（结构性变更时递增）。
    pub version: u32,
    /// 快照里 mcpane.db 的迁移版本；下载侧要求 <= 本机（老应用拒新快照）。
    pub db_migration_version: i64,
    /// 各 artifact 摘要拼接的稳定哈希（状态展示/变更判断用）。
    pub snapshot_id: String,
    /// 上传设备名（hostname，仅展示）。
    pub device_name: String,
    /// 上传时刻（Unix 秒）。
    pub created_at: i64,
    pub artifacts: Vec<ArtifactMeta>,
}

impl Manifest {
    /// 校验远端 manifest 与本机的兼容性。
    pub fn validate_compat(&self, local_db_version: i64) -> Result<(), crate::SyncError> {
        if self.format != PROTOCOL_FORMAT {
            return Err(crate::SyncError::Incompatible(format!(
                "远端数据格式不是 MCPane 同步目录（format={}）",
                self.format
            )));
        }
        if self.version != PROTOCOL_VERSION {
            return Err(crate::SyncError::Incompatible(format!(
                "远端协议版本 v{} 与本机 v{PROTOCOL_VERSION} 不一致",
                self.version
            )));
        }
        if self.db_migration_version > local_db_version {
            return Err(crate::SyncError::Incompatible(format!(
                "远端数据由更新版本的 MCPane 上传（db v{} > 本机 v{local_db_version}），请先升级应用",
                self.db_migration_version
            )));
        }
        Ok(())
    }

    /// 按角色取 artifact 元数据。
    pub fn artifact(&self, role: &str) -> Option<&ArtifactMeta> {
        self.artifacts.iter().find(|a| a.role == role)
    }
}

pub fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// 快照身份：各 artifact 的 `name:sha256` 按名称排序拼接后取 sha256。
pub fn compute_snapshot_id(artifacts: &[ArtifactMeta]) -> String {
    let ordered: BTreeMap<&str, &str> = artifacts
        .iter()
        .map(|a| (a.name.as_str(), a.sha256.as_str()))
        .collect();
    let joined = ordered
        .iter()
        .map(|(name, hash)| format!("{name}:{hash}"))
        .collect::<Vec<_>>()
        .join("\n");
    sha256_hex(joined.as_bytes())
}

/// 校验下载内容与 manifest 声明一致（size + sha256 双验）。
pub fn verify_artifact(meta: &ArtifactMeta, data: &[u8]) -> Result<(), crate::SyncError> {
    if data.len() as u64 != meta.size {
        return Err(crate::SyncError::Integrity(format!(
            "{} 大小不符：期望 {}，实际 {}",
            meta.name,
            meta.size,
            data.len()
        )));
    }
    let actual = sha256_hex(data);
    if actual != meta.sha256 {
        return Err(crate::SyncError::Integrity(format!(
            "{} 摘要不符（内容损坏或被篡改）",
            meta.name
        )));
    }
    Ok(())
}

/// 设备名：hostname，去控制字符、截 64 字符；取不到用 "unknown"。
pub fn device_name() -> String {
    let raw = env_host_name()
        .or_else(host_name_command)
        .unwrap_or_default();
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect();
    if cleaned.is_empty() {
        "unknown".to_owned()
    } else {
        cleaned
    }
}

/// Windows 没有 `HOSTNAME`，用的是 `COMPUTERNAME`（且它一定存在，于是不必再去
/// spawn 进程）。类 Unix 上 `HOSTNAME` 常是未导出的 shell 变量，取不到属正常，
/// 走下面的命令兜底。
fn env_host_name() -> Option<String> {
    let key = if cfg!(windows) {
        "COMPUTERNAME"
    } else {
        "HOSTNAME"
    };
    std::env::var(key).ok().filter(|s| !s.trim().is_empty())
}

/// 这里不做系统代码页解码（adapter 的 `--version` 探测做了）：Windows 上
/// `COMPUTERNAME` 必然存在，上面那条就返回了，走不到这里；真走到也只是设备名
/// 显示成 `unknown`，不影响同步本身。
fn host_name_command() -> Option<String> {
    let mut command = std::process::Command::new("hostname");
    // 云同步会在后台定时跑，不加这个标志每次都在 GUI 进程下闪一个黑框。
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn meta(name: &str, data: &[u8]) -> ArtifactMeta {
        ArtifactMeta {
            role: "db".to_owned(),
            name: name.to_owned(),
            size: data.len() as u64,
            sha256: sha256_hex(data),
        }
    }

    #[test]
    fn snapshot_id_is_order_independent_and_content_sensitive() {
        let a = meta("mcpane.db", b"db-bytes");
        let b = meta("providers.json", b"{}");
        let id1 = compute_snapshot_id(&[a.clone(), b.clone()]);
        let id2 = compute_snapshot_id(&[b.clone(), a]);
        assert_eq!(id1, id2, "顺序无关");
        let a2 = meta("mcpane.db", b"different");
        assert_ne!(id1, compute_snapshot_id(&[a2, b]), "内容敏感");
    }

    #[test]
    fn verify_artifact_rejects_size_and_hash_mismatch() {
        let m = meta("x", b"hello");
        assert!(verify_artifact(&m, b"hello").is_ok());
        assert!(verify_artifact(&m, b"hell").is_err());
        assert!(verify_artifact(&m, b"hellO").is_err());
    }

    #[test]
    fn compat_validation_gates() {
        let mut m = Manifest {
            format: PROTOCOL_FORMAT.into(),
            version: PROTOCOL_VERSION,
            db_migration_version: 13,
            snapshot_id: "x".into(),
            device_name: "dev".into(),
            created_at: 0,
            artifacts: vec![],
        };
        assert!(m.validate_compat(13).is_ok());
        assert!(m.validate_compat(20).is_ok(), "老快照可导入新应用");
        assert!(m.validate_compat(12).is_err(), "新快照拒绝导入老应用");
        m.format = "other".into();
        assert!(m.validate_compat(13).is_err());
    }
}
