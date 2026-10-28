//! 同步引擎：本地快照构建 → 上传；远端校验 → 下载 → 导入。
//!
//! upload：导出 db 快照（剔本机表）→ 读 providers.json → 算摘要建 manifest
//! → 先传 artifacts、manifest 最后传（best-effort 一致性：manifest 在即
//! 快照完整）。
//! download：取 manifest → 兼容校验 → 逐 artifact 下载 + size/sha256 双验
//! → 先备份本地 → 导入 db（保留本机表）→ 写 providers.json。

use std::path::{Path, PathBuf};

use mcpane_storage::SqlitePool;

use crate::manifest::{
    ArtifactMeta, Manifest, ROLE_DB, ROLE_PROVIDERS, compute_snapshot_id, device_name, sha256_hex,
    verify_artifact,
};
use crate::{
    ConflictInfo, MAX_ARTIFACT_BYTES, MAX_MANIFEST_BYTES, PutCondition, RemoteStore, SyncError,
};

/// 本机 providers 文件名（远端 artifact 是内容寻址名，见 artifact_name）。
pub const ARTIFACT_PROVIDERS: &str = "providers.json";
pub const ARTIFACT_MANIFEST: &str = "manifest.json";

/// 内容寻址 artifact 名：并发上传不同内容绝不同名，落败方无法污染胜者。
fn artifact_name(role: &str, sha256: &str) -> String {
    let short = &sha256[..12.min(sha256.len())];
    match role {
        ROLE_DB => format!("mcpane-{short}.db"),
        _ => format!("providers-{short}.json"),
    }
}

/// 云同步导出前脱敏 providers.json：摘除 `desktopGateway` 整个键（P0 第二处，
/// 与 db 快照的 `mcp_servers.bearer/env/headers` 同一条红线——`DesktopGateway::token`
/// 是明文存储的本地网关 loopback 鉴权 token）。
///
/// 只删键、不置 null：`token` 字段是非 `Option` 的 `String`，若写成
/// `{"desktopGateway":{"token":null,...}}`，下载方反序列化 `DesktopGateway`
/// 会因类型不匹配失败，进而把整份 `ProviderState` 判成损坏、隔离归档、
/// 丢掉其余本该正常同步的数据——删键则该字段在下载方缺失，
/// `ProviderState.desktop_gateway: Option<DesktopGateway>` 的 `#[serde(default)]`
/// 落 None，与"这台设备还没有本地网关"完全等价，需要时按需生成新 token。
///
/// 解析失败（旧格式/损坏）不是本函数的职责：原样返回，交给上传/下载两端
/// 已有的校验去处理（本函数只做"如果能解析出这个键，就摘掉它"）。
fn redact_profile_object(profile: &mut serde_json::Map<String, serde_json::Value>) {
    profile.remove("headers");
    profile.remove("extraSettings");
    profile.remove("extra_settings");
    // 路由请求体覆盖与请求头同理：部分厂商把鉴权参数放在请求体里。
    profile.remove("routeBodyOverride");
}

fn redact_providers_json(bytes: &[u8]) -> Vec<u8> {
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return bytes.to_vec();
    };
    let Some(obj) = value.as_object_mut() else {
        return bytes.to_vec();
    };
    obj.remove("desktopGateway");
    // 上次切换写入片段与通用配置片段可能含明文 Key，整键摘除（下载方 #[serde(default)] 落空）。
    obj.remove("appliedFragments");
    obj.remove("officialOverrides");
    let snippet_keys: Vec<String> = obj
        .keys()
        .filter(|key| key.ends_with("CommonSnippet"))
        .cloned()
        .collect();
    for key in snippet_keys {
        obj.remove(&key);
    }
    if let Some(providers) = obj.get_mut("providers").and_then(|v| v.as_array_mut()) {
        for profile in providers {
            let Some(p) = profile.as_object_mut() else {
                continue;
            };
            redact_profile_object(p);
        }
    }
    serde_json::to_vec(&value).unwrap_or_else(|_| bytes.to_vec())
}

/// 一次同步的结果（状态展示用）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncOutcome {
    pub snapshot_id: String,
    /// 本次传输的总字节数（上传或下载）。
    pub total_bytes: u64,
    pub device_name: String,
    pub created_at: i64,
}

/// 远端快照信息（上传/下载前的确认对话框用）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteInfo {
    pub exists: bool,
    pub device_name: Option<String>,
    pub created_at: Option<i64>,
    pub snapshot_id: Option<String>,
    /// 与本机的兼容性问题（None = 兼容）。
    pub incompatible: Option<String>,
}

pub struct SyncEngine<'a> {
    pub pool: &'a SqlitePool,
    /// MCPane 数据目录（providers.json 所在）。
    pub mcpane_home: &'a Path,
    /// 临时工作目录（db 快照落这里，用后删除）。
    pub work_dir: PathBuf,
}

impl<'a> SyncEngine<'a> {
    pub fn new(pool: &'a SqlitePool, mcpane_home: &'a Path) -> Self {
        Self {
            pool,
            mcpane_home,
            work_dir: std::env::temp_dir().join("mcpane-sync"),
        }
    }

    async fn local_db_version(&self) -> Result<i64, SyncError> {
        Ok(mcpane_storage::db_migration_version(self.pool).await?)
    }

    /// 查询远端快照信息（不做任何写入）。
    pub async fn remote_info(&self, store: &impl RemoteStore) -> Result<RemoteInfo, SyncError> {
        let Some(raw) = store.get(ARTIFACT_MANIFEST, MAX_MANIFEST_BYTES).await? else {
            return Ok(RemoteInfo {
                exists: false,
                device_name: None,
                created_at: None,
                snapshot_id: None,
                incompatible: None,
            });
        };
        let manifest: Manifest = serde_json::from_slice(&raw.data)
            .map_err(|e| SyncError::Remote(format!("manifest 解析失败：{e}")))?;
        let incompatible = manifest
            .validate_compat(self.local_db_version().await?)
            .err()
            .map(|e| e.to_string());
        Ok(RemoteInfo {
            exists: true,
            device_name: Some(manifest.device_name),
            created_at: Some(manifest.created_at),
            snapshot_id: Some(manifest.snapshot_id),
            incompatible,
        })
    }

    /// 上传本地快照，覆盖远端。
    ///
    /// 并发冲突防护（两道防线，`force` 同时豁免）：
    /// 1. 预检：远端 manifest 的 snapshot_id ≠ `last_synced_snapshot_id`
    ///    （本机上次成功上传/下载记录的基线）→ 其他设备动过 → Conflict；
    /// 2. 条件写：manifest PUT 带 If-Match（拿到 etag 时）/ If-None-Match: *
    ///    （远端不存在时），412 → Conflict，堵住预检到落盘之间的竞态窗口。
    pub async fn upload(
        &self,
        store: &impl RemoteStore,
        last_synced_snapshot_id: Option<&str>,
        force: bool,
    ) -> Result<SyncOutcome, SyncError> {
        store.ensure_dirs().await?;

        // (0) 冲突预检 + 记录 etag（条件写用）。
        let remote = store.get(ARTIFACT_MANIFEST, MAX_MANIFEST_BYTES).await?;
        let remote_exists = remote.is_some();
        let remote_etag = remote.as_ref().and_then(|o| o.etag.clone());
        // manifest 解析失败（远端损坏）不算冲突：覆盖即修复。
        let remote_manifest: Option<Manifest> = remote
            .as_ref()
            .and_then(|o| serde_json::from_slice(&o.data).ok());
        if !force
            && let Some(manifest) = &remote_manifest
            && last_synced_snapshot_id != Some(manifest.snapshot_id.as_str())
        {
            return Err(SyncError::Conflict(ConflictInfo {
                device_name: Some(manifest.device_name.clone()),
                created_at: Some(manifest.created_at),
                snapshot_id: Some(manifest.snapshot_id.clone()),
            }));
        }

        // (a) 构建本地 artifacts。
        std::fs::create_dir_all(&self.work_dir)
            .map_err(|e| SyncError::Local(format!("创建工作目录失败：{e}")))?;
        let db_snapshot = self.work_dir.join(format!("upload-{}.db", unique_suffix()));
        // redact_downstream_credentials=true：这是离开设备的那份副本，
        // mcp_servers 的下游 bearer/env/headers 必须剥离（P0）。
        //
        // 清理无条件先做、再判结果（download 侧同款）：导出或读取任一步失败时
        // 用 `?` 直接返回会把这份快照永久留在 OS 临时目录，没有任何地方回收它。
        let exported = mcpane_storage::export_snapshot(
            self.pool,
            &db_snapshot,
            &mcpane_storage::SYNC_LOCAL_TABLES,
            true,
        )
        .await;
        let read_result = match exported {
            Ok(()) => std::fs::read(&db_snapshot)
                .map_err(|e| SyncError::Local(format!("读取快照失败：{e}"))),
            Err(error) => Err(error.into()),
        };
        let _ = std::fs::remove_file(&db_snapshot);
        let db_bytes = read_result?;

        let providers_bytes = match std::fs::read(self.mcpane_home.join(ARTIFACT_PROVIDERS)) {
            Ok(bytes) => bytes,
            // 只有「从未配置过 Provider」才允许以空对象顶替，下载侧同样按可缺省
            // 处理。其余错误（权限、IO）必须上抛：这是全量快照覆盖协议，一次
            // 读取抖动就会把空 providers 传成新快照，其他设备下次 download 会用
            // 它覆盖掉本地正常的供应商配置——静默的跨设备数据丢失。
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => b"{}".to_vec(),
            Err(error) => {
                return Err(SyncError::Local(format!("读取 providers 失败：{error}")));
            }
        };
        // P0 第二处：desktopGateway.token 是本地代理的 loopback 鉴权 token，
        // 明文存在 providers.json 里（`mcpane-provider` 的 `DesktopGateway::token`），
        // 同一条"任何明文凭据都不上云"的红线。它本就只对"这台机器自己的本地
        // 代理"有意义，不需要跨设备携带——整个键一并摘除（而非把 token 置
        // null：`DesktopGateway::token` 是非 Option 的 String，null 会在下载
        // 方解析 providers.json 时把整份 ProviderState 判成"损坏"进隔离区，
        // 反而丢了其余正常数据）。缺键时 `desktop_gateway: Option<_>` 的
        // `#[serde(default)]` 自然落 None，下载方按需生成新网关 token。
        let providers_bytes = redact_providers_json(&providers_bytes);

        let db_sha = sha256_hex(&db_bytes);
        let providers_sha = sha256_hex(&providers_bytes);
        let db_name = artifact_name(ROLE_DB, &db_sha);
        let providers_name = artifact_name(ROLE_PROVIDERS, &providers_sha);
        let artifacts = vec![
            ArtifactMeta {
                role: ROLE_DB.into(),
                name: db_name.clone(),
                size: db_bytes.len() as u64,
                sha256: db_sha,
            },
            ArtifactMeta {
                role: ROLE_PROVIDERS.into(),
                name: providers_name.clone(),
                size: providers_bytes.len() as u64,
                sha256: providers_sha,
            },
        ];
        let snapshot_id = compute_snapshot_id(&artifacts);
        let created_at = now_secs();
        let manifest = Manifest {
            format: crate::manifest::PROTOCOL_FORMAT.into(),
            version: crate::manifest::PROTOCOL_VERSION,
            db_migration_version: self.local_db_version().await?,
            snapshot_id: snapshot_id.clone(),
            device_name: device_name(),
            created_at,
            artifacts,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|e| SyncError::Local(format!("manifest 序列化失败：{e}")))?;

        // (b) 先传数据、manifest 最后传（带条件写，堵竞态窗口）。
        // artifact 名内容寻址：无条件写安全（同名必同内容，幂等覆盖）。
        let total_bytes = (db_bytes.len() + providers_bytes.len() + manifest_bytes.len()) as u64;
        store.put(&db_name, db_bytes, PutCondition::None).await?;
        store
            .put(&providers_name, providers_bytes, PutCondition::None)
            .await?;
        let condition = if force {
            PutCondition::None
        } else {
            match &remote_etag {
                // 弱 etag（W/）不能用于 If-Match 强比较，退回无条件。
                Some(etag) if !etag.starts_with("W/") => PutCondition::IfMatch(etag.clone()),
                _ if !remote_exists => PutCondition::IfAbsent,
                _ => PutCondition::None,
            }
        };
        if let Err(err) = store
            .put(ARTIFACT_MANIFEST, manifest_bytes, condition)
            .await
        {
            if let SyncError::Conflict(_) = &err {
                // 竞态窗口被别的设备抢先：补取对方 manifest 丰富冲突详情。
                let info = match store.get(ARTIFACT_MANIFEST, MAX_MANIFEST_BYTES).await {
                    Ok(Some(obj)) => serde_json::from_slice::<Manifest>(&obj.data)
                        .map(|m| ConflictInfo {
                            device_name: Some(m.device_name),
                            created_at: Some(m.created_at),
                            snapshot_id: Some(m.snapshot_id),
                        })
                        .unwrap_or_default(),
                    _ => ConflictInfo::default(),
                };
                return Err(SyncError::Conflict(info));
            }
            return Err(err);
        }

        // (c) manifest 已切换：清理旧快照的 artifact（内容未变则同名跳过）。
        // best-effort——失败只留下孤儿文件，不影响一致性。
        if let Some(old) = &remote_manifest {
            for stale in &old.artifacts {
                if stale.name != db_name && stale.name != providers_name {
                    let _ = store.delete(&stale.name).await;
                }
            }
        }

        Ok(SyncOutcome {
            snapshot_id,
            total_bytes,
            device_name: manifest.device_name,
            created_at,
        })
    }

    /// 下载远端快照并导入本地（覆盖业务数据，保留本机日志/用量/设置）。
    /// 导入前自动做一次本地兜底备份。
    pub async fn download(&self, store: &impl RemoteStore) -> Result<SyncOutcome, SyncError> {
        let raw = store
            .get(ARTIFACT_MANIFEST, MAX_MANIFEST_BYTES)
            .await?
            .ok_or_else(|| SyncError::Remote("远端没有可下载的快照".into()))?;
        let manifest: Manifest = serde_json::from_slice(&raw.data)
            .map_err(|e| SyncError::Remote(format!("manifest 解析失败：{e}")))?;
        manifest.validate_compat(self.local_db_version().await?)?;

        let db_meta = manifest
            .artifact(ROLE_DB)
            .ok_or_else(|| SyncError::Remote("远端快照缺少数据库 artifact".into()))?;
        let providers_meta = manifest
            .artifact(ROLE_PROVIDERS)
            .ok_or_else(|| SyncError::Remote("远端快照缺少 providers artifact".into()))?;

        let db_bytes = store
            .get(&db_meta.name, MAX_ARTIFACT_BYTES)
            .await?
            .ok_or_else(|| SyncError::Remote("数据库 artifact 缺失".into()))?
            .data;
        verify_artifact(db_meta, &db_bytes)?;
        let providers_bytes = store
            .get(&providers_meta.name, MAX_ARTIFACT_BYTES)
            .await?
            .ok_or_else(|| SyncError::Remote("providers artifact 缺失".into()))?
            .data;
        verify_artifact(providers_meta, &providers_bytes)?;

        // 覆盖前兜底备份（含 providers.json 的 db 外数据由调用方文件级处理）。
        mcpane_storage::backup_now(self.pool, self.mcpane_home).await?;

        // providers.json 先落临时文件再导 db：此前顺序相反，db 导入成功后
        // providers 写失败会留下「db 已是远端、providers 还是本地」的半态；
        // 现在写不动就在改任何东西之前整体失败。
        let target = self.mcpane_home.join(ARTIFACT_PROVIDERS);
        let tmp = target.with_extension("json.mcpane-tmp");
        // 含 desktopGateway.token 明文：临时文件与 .bak 都收紧到 0600。
        let _ = std::fs::remove_file(&tmp);
        // 整段任一步失败都要带走 tmp——收紧权限本身就可能是失败的那一步，
        // 把一份含明文 token 的文件以默认权限留在盘上，正是这里要防的事。
        // （下方 import 失败时的清理已有，缺的是这几步。）
        let prepared = (|| -> Result<(), SyncError> {
            std::fs::write(&tmp, &providers_bytes)
                .map_err(|e| SyncError::Local(format!("写入 providers 失败：{e}")))?;
            mcpane_config::permissions::harden_to_owner_only(&tmp)
                .map_err(|e| SyncError::Local(format!("收紧 providers 临时文件权限失败：{e}")))?;
            // 旧文件留 .bak——导入出岔子时唯一的手工找回点，拷不动必须上抛而不是吞掉。
            if target.exists() {
                let bak = target.with_extension("json.bak");
                std::fs::copy(&target, &bak)
                    .map_err(|e| SyncError::Local(format!("备份原 providers 失败：{e}")))?;
                mcpane_config::permissions::harden_to_owner_only(&bak)
                    .map_err(|e| SyncError::Local(format!("收紧 providers 备份权限失败：{e}")))?;
            }
            Ok(())
        })();
        if let Err(error) = prepared {
            let _ = std::fs::remove_file(&tmp);
            return Err(error);
        }

        // 导入 db（保留本机表）。
        std::fs::create_dir_all(&self.work_dir)
            .map_err(|e| SyncError::Local(format!("创建工作目录失败：{e}")))?;
        let db_path = self
            .work_dir
            .join(format!("download-{}.db", unique_suffix()));
        std::fs::write(&db_path, &db_bytes)
            .map_err(|e| SyncError::Local(format!("落盘快照失败：{e}")))?;
        // 这条路径不经过 export_snapshot（直接落远端字节），单独收紧权限——
        // 远端快照即使已按 P0 修复剥离了下游凭据，仍含其余业务数据，且旧版本
        // 上传的存量快照可能未剥离，同一份威胁模型不能只顾上传侧不顾下载侧。
        mcpane_storage::harden_db_permissions(&db_path);
        let import_result = mcpane_storage::import_snapshot(
            self.pool,
            &db_path,
            &mcpane_storage::SYNC_LOCAL_TABLES,
        )
        .await;
        let _ = std::fs::remove_file(&db_path);
        if let Err(error) = import_result {
            // db 没动成，providers 也别动：清掉已备好的临时文件保持原状。
            let _ = std::fs::remove_file(&tmp);
            return Err(error.into());
        }

        // 原子替换。rename 失败的概率远小于 write（同目录、同文件系统），
        // 真失败时 .bak 与 tmp 都在，错误信息足以让用户手工收拾。
        std::fs::rename(&tmp, &target)
            .map_err(|e| SyncError::Local(format!("替换 providers 失败：{e}")))?;

        Ok(SyncOutcome {
            snapshot_id: manifest.snapshot_id,
            total_bytes: (db_bytes.len() + providers_bytes.len()) as u64,
            device_name: manifest.device_name,
            created_at: manifest.created_at,
        })
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 工作目录临时文件的唯一后缀：pid + 进程内计数（并行测试/并发同步不串号）。
fn unique_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// 内存远端：单测覆盖 upload→download 全链路 + etag 条件写语义。
    /// etag 为每 key 单调递增的版本号（模拟真实服务器行为）。
    /// `weak_etags` 模拟只回弱 etag 的服务器：弱 etag 不可用于 If-Match
    /// 强比较，带 IfMatch 一律 412（引擎应过滤掉 W/ 前缀退回无条件）。
    #[derive(Default)]
    struct MemStore {
        objects: Mutex<HashMap<String, (Vec<u8>, u64)>>,
        version: std::sync::atomic::AtomicU64,
        weak_etags: bool,
    }

    impl MemStore {
        fn next_version(&self) -> u64 {
            self.version
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1
        }
        fn etag_for(&self, version: u64) -> String {
            if self.weak_etags {
                format!("W/\"v{version}\"")
            } else {
                format!("\"v{version}\"")
            }
        }
        fn keys(&self) -> Vec<String> {
            let mut keys: Vec<String> = self.objects.lock().unwrap().keys().cloned().collect();
            keys.sort();
            keys
        }
    }

    impl RemoteStore for MemStore {
        async fn test(&self) -> Result<(), SyncError> {
            Ok(())
        }
        async fn ensure_dirs(&self) -> Result<(), SyncError> {
            Ok(())
        }
        async fn get(
            &self,
            key: &str,
            max_bytes: u64,
        ) -> Result<Option<crate::RemoteObject>, SyncError> {
            let objects = self.objects.lock().unwrap();
            match objects.get(key) {
                Some((data, _)) if data.len() as u64 > max_bytes => {
                    Err(SyncError::Integrity(format!("{key} 超过大小上限")))
                }
                Some((data, version)) => Ok(Some(crate::RemoteObject {
                    data: data.clone(),
                    etag: Some(self.etag_for(*version)),
                })),
                None => Ok(None),
            }
        }
        async fn put(
            &self,
            key: &str,
            body: Vec<u8>,
            condition: PutCondition,
        ) -> Result<(), SyncError> {
            let version = self.next_version();
            let mut objects = self.objects.lock().unwrap();
            let current_etag = objects.get(key).map(|(_, v)| self.etag_for(*v));
            match &condition {
                PutCondition::IfMatch(_) if self.weak_etags => {
                    // RFC 7232：If-Match 必须强比较，弱 etag 永不匹配。
                    return Err(SyncError::Conflict(crate::ConflictInfo::default()));
                }
                PutCondition::IfMatch(expected) if current_etag.as_deref() != Some(expected) => {
                    return Err(SyncError::Conflict(crate::ConflictInfo::default()));
                }
                PutCondition::IfAbsent if current_etag.is_some() => {
                    return Err(SyncError::Conflict(crate::ConflictInfo::default()));
                }
                _ => {}
            }
            objects.insert(key.to_owned(), (body, version));
            Ok(())
        }
        async fn delete(&self, key: &str) -> Result<(), SyncError> {
            self.objects.lock().unwrap().remove(key);
            Ok(())
        }
    }

    #[tokio::test]
    async fn upload_then_download_roundtrip_across_devices() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemStore::default();

        // 设备 A：有一个 server + providers.json。
        let home_a = dir.path().join("home-a");
        std::fs::create_dir_all(&home_a).unwrap();
        std::fs::write(home_a.join("providers.json"), br#"{"schemaVersion":2}"#).unwrap();
        let pool_a = mcpane_storage::open_database(&dir.path().join("a.db"))
            .await
            .unwrap();
        let servers_a = mcpane_storage::ServerRepo::new(pool_a.clone());
        servers_a.upsert(test_def("from-device-a")).await.unwrap();
        let engine_a = SyncEngine::new(&pool_a, &home_a);
        let up = engine_a.upload(&store, None, false).await.unwrap();
        assert!(!up.snapshot_id.is_empty());

        // remote_info 对设备 B 可见且兼容。
        let home_b = dir.path().join("home-b");
        std::fs::create_dir_all(&home_b).unwrap();
        let pool_b = mcpane_storage::open_database(&dir.path().join("b.db"))
            .await
            .unwrap();
        let engine_b = SyncEngine::new(&pool_b, &home_b);
        let info = engine_b.remote_info(&store).await.unwrap();
        assert!(info.exists);
        assert!(info.incompatible.is_none());
        assert_eq!(info.snapshot_id.as_deref(), Some(up.snapshot_id.as_str()));

        // 设备 B 下载：业务数据到位、providers.json 落盘、自动兜底备份存在。
        let down = engine_b.download(&store).await.unwrap();
        assert_eq!(down.snapshot_id, up.snapshot_id);
        let names: Vec<String> = mcpane_storage::ServerRepo::new(pool_b.clone())
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|d| d.slug)
            .collect();
        assert_eq!(names, vec!["from-device-a".to_owned()]);
        assert_eq!(
            std::fs::read_to_string(home_b.join("providers.json")).unwrap(),
            r#"{"schemaVersion":2}"#
        );
        assert!(
            !mcpane_storage::list_backups(&home_b).is_empty(),
            "下载前必须兜底备份"
        );
    }

    /// P0 第二处回归防线：providers.json 的 `desktopGateway.token`（本地代理
    /// loopback 鉴权 token，明文存储）同样不得随 `upload()` 的云同步快照外泄——
    /// 与 mcp_servers 的 bearer/env/headers 同一条红线（`mcpane-storage` 的
    /// 哨兵测试覆盖前者，这条覆盖后者）。脱敏要精确：其余字段（如
    /// schemaVersion）必须原样保留，不能把整个 providers.json 判废。
    #[tokio::test]
    async fn upload_strips_desktop_gateway_token_from_providers_json() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemStore::default();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("providers.json"),
            br#"{"schemaVersion":2,"desktopGateway":{"token":"SENTINEL-GATEWAY-TOKEN-9f3a","listenPort":24871}}"#,
        )
        .unwrap();
        let pool = mcpane_storage::open_database(&dir.path().join("a.db"))
            .await
            .unwrap();
        let engine = SyncEngine::new(&pool, &home);
        engine.upload(&store, None, false).await.unwrap();

        // providers artifact 应已上传（find 命中即 Some）。
        let providers_key = store
            .keys()
            .into_iter()
            .find(|k| k.starts_with("providers-"))
            .unwrap();
        let uploaded = store
            .objects
            .lock()
            .unwrap()
            .get(&providers_key)
            .unwrap()
            .0
            .clone();
        let text = String::from_utf8(uploaded).unwrap();
        assert!(
            !text.contains("SENTINEL-GATEWAY-TOKEN-9f3a"),
            "同步快照的 providers.json 含 desktopGateway.token 明文，违反 mcpane-sync/src/lib.rs:13 声明的红线"
        );
        assert!(
            !text.contains("desktopGateway"),
            "desktopGateway 应整键摘除，而不是留一个空壳"
        );
        assert!(
            text.contains("\"schemaVersion\":2"),
            "脱敏必须是外科手术式的，其余字段要原样保留：{text}"
        );
    }

    /// providers 的 headers / extraSettings / routeBodyOverride 同样不得随云同步外泄。
    #[tokio::test]
    async fn upload_strips_provider_headers_and_extra_settings() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemStore::default();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("providers.json"),
            br#"{"schemaVersion":2,"providers":[{"id":"r1","name":"Relay","app":"opencode","category":"relay","headers":{"Authorization":"SENTINEL-HEADER-KEY"},"extraSettings":{"env":{"API_KEY":"SENTINEL-EXTRA-KEY"}},"routeBodyOverride":{"api_key":"SENTINEL-BODY-KEY"},"apiKeyRef":"ref-only"}]}"#,
        )
        .unwrap();
        let pool = mcpane_storage::open_database(&dir.path().join("a.db"))
            .await
            .unwrap();
        let engine = SyncEngine::new(&pool, &home);
        engine.upload(&store, None, false).await.unwrap();

        let providers_key = store
            .keys()
            .into_iter()
            .find(|k| k.starts_with("providers-"))
            .unwrap();
        let uploaded = store
            .objects
            .lock()
            .unwrap()
            .get(&providers_key)
            .unwrap()
            .0
            .clone();
        let text = String::from_utf8(uploaded).unwrap();
        assert!(!text.contains("SENTINEL-HEADER-KEY"));
        assert!(!text.contains("SENTINEL-EXTRA-KEY"));
        assert!(!text.contains("SENTINEL-BODY-KEY"));
        assert!(text.contains("ref-only"));
    }

    /// appliedFragments / commonSnippet 同样不得随云同步外泄。
    #[tokio::test]
    async fn upload_strips_applied_fragments_and_common_snippets() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemStore::default();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("providers.json"),
            br#"{"schemaVersion":2,"appliedFragments":{"claude":{"apiKey":"SENTINEL-APPLIED"}},"claudeCommonSnippet":{"env":{"TOKEN":"SENTINEL-COMMON"}},"providers":[]}"#,
        )
        .unwrap();
        let pool = mcpane_storage::open_database(&dir.path().join("a.db"))
            .await
            .unwrap();
        let engine = SyncEngine::new(&pool, &home);
        engine.upload(&store, None, false).await.unwrap();

        let providers_key = store
            .keys()
            .into_iter()
            .find(|k| k.starts_with("providers-"))
            .unwrap();
        let text = String::from_utf8(
            store
                .objects
                .lock()
                .unwrap()
                .get(&providers_key)
                .unwrap()
                .0
                .clone(),
        )
        .unwrap();
        assert!(!text.contains("SENTINEL-APPLIED"));
        assert!(!text.contains("SENTINEL-COMMON"));
        assert!(!text.contains("appliedFragments"));
        assert!(!text.contains("claudeCommonSnippet"));
    }

    /// 回归：providers.json 存在但读不动（权限、被占、路径被目录顶掉）时必须
    /// 整体失败，不得以空对象顶替上传。这是全量快照覆盖协议——空 providers
    /// 一旦成为新快照，其他设备下次 download 就用它覆盖掉本地正常的供应商配置。
    /// 只有「文件不存在」（从未配置过 Provider）才允许回落到空对象。
    #[tokio::test]
    async fn upload_fails_instead_of_uploading_empty_providers_when_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemStore::default();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        // 用目录顶掉 providers.json：read 必失败，且错误不是 NotFound（跨平台成立）。
        std::fs::create_dir_all(home.join("providers.json")).unwrap();
        let pool = mcpane_storage::open_database(&dir.path().join("a.db"))
            .await
            .unwrap();
        let engine = SyncEngine::new(&pool, &home);

        assert!(
            engine.upload(&store, None, false).await.is_err(),
            "providers 读不动时 upload 必须整体失败，不得以空对象顶替"
        );
        assert!(
            !store.keys().iter().any(|k| k.starts_with("providers-")),
            "失败的 upload 不得把空 providers 留在远端"
        );
    }

    #[tokio::test]
    async fn download_rejects_tampered_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemStore::default();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let pool = mcpane_storage::open_database(&dir.path().join("a.db"))
            .await
            .unwrap();
        let engine = SyncEngine::new(&pool, &home);
        engine.upload(&store, None, false).await.unwrap();

        // 篡改远端 db artifact（内容寻址名，从对象表按后缀定位）。
        {
            let mut objects = store.objects.lock().unwrap();
            let db_key = objects
                .keys()
                .find(|k| k.ends_with(".db"))
                .cloned()
                .unwrap();
            objects
                .entry(db_key)
                .and_modify(|(data, _)| data.push(0xFF));
        }
        let err = engine.download(&store).await.unwrap_err();
        assert!(matches!(err, SyncError::Integrity(_)), "{err}");
    }

    #[tokio::test]
    async fn download_without_remote_snapshot_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemStore::default();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let pool = mcpane_storage::open_database(&dir.path().join("a.db"))
            .await
            .unwrap();
        let engine = SyncEngine::new(&pool, &home);
        let info = engine.remote_info(&store).await.unwrap();
        assert!(!info.exists);
        assert!(engine.download(&store).await.is_err());
    }

    /// 冲突防护：基线不匹配拒绝、下载后基线对齐放行、force 豁免。
    #[tokio::test]
    async fn upload_conflict_detection_and_force() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemStore::default();

        // 设备 A 首次上传（远端空，IfAbsent 放行）。
        let home_a = dir.path().join("home-a");
        std::fs::create_dir_all(&home_a).unwrap();
        let pool_a = mcpane_storage::open_database(&dir.path().join("a.db"))
            .await
            .unwrap();
        let engine_a = SyncEngine::new(&pool_a, &home_a);
        let up_a = engine_a.upload(&store, None, false).await.unwrap();

        // 设备 B 未下载直接上传（基线 None ≠ 远端）→ Conflict，且带对方信息。
        let home_b = dir.path().join("home-b");
        std::fs::create_dir_all(&home_b).unwrap();
        let pool_b = mcpane_storage::open_database(&dir.path().join("b.db"))
            .await
            .unwrap();
        let engine_b = SyncEngine::new(&pool_b, &home_b);
        let err = engine_b.upload(&store, None, false).await.unwrap_err();
        match err {
            SyncError::Conflict(info) => {
                assert_eq!(info.snapshot_id.as_deref(), Some(up_a.snapshot_id.as_str()));
                assert!(info.device_name.is_some());
            }
            other => panic!("应为 Conflict，得到：{other}"),
        }

        // B 下载后基线对齐 → 上传放行。B 先写入自有数据，确保其快照
        // 与 A 的必然不同（否则 snapshot_id 相同，A 的过期基线检不出来）。
        let down = engine_b.download(&store).await.unwrap();
        mcpane_storage::ServerRepo::new(pool_b.clone())
            .upsert(test_def("from-device-b"))
            .await
            .unwrap();
        engine_b
            .upload(&store, Some(down.snapshot_id.as_str()), false)
            .await
            .unwrap();

        // A 的基线已过期（远端被 B 覆盖）→ Conflict；force 豁免。
        let stale = engine_a
            .upload(&store, Some(up_a.snapshot_id.as_str()), false)
            .await;
        assert!(matches!(stale, Err(SyncError::Conflict(_))));
        engine_a
            .upload(&store, Some(up_a.snapshot_id.as_str()), true)
            .await
            .unwrap();

        // 成功上传后旧快照 artifact 已清理：远端只剩当前 db/providers/manifest。
        assert_eq!(
            store.keys().len(),
            3,
            "旧 artifact 应被 GC：{:?}",
            store.keys()
        );
    }

    /// 弱 etag 服务器：引擎须过滤 W/ 前缀退回无条件写（否则 If-Match
    /// 强比较永不匹配，上传永远失败）。
    #[tokio::test]
    async fn weak_etag_server_falls_back_to_unconditional() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemStore {
            weak_etags: true,
            ..Default::default()
        };
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let pool = mcpane_storage::open_database(&dir.path().join("a.db"))
            .await
            .unwrap();
        let engine = SyncEngine::new(&pool, &home);
        let up1 = engine.upload(&store, None, false).await.unwrap();
        // 基线对齐的二次上传：远端存在 + 弱 etag → 无条件写放行。
        engine
            .upload(&store, Some(up1.snapshot_id.as_str()), false)
            .await
            .unwrap();
    }

    /// 竞态窗口防线（必修回归用例）：预检通过后 manifest 被并发赢家改写
    /// → 条件写 412 → Conflict 且携带对方详情；关键断言：**落败方已上传
    /// 的 artifact 是孤儿、不污染胜者快照**，远端仍可完整下载。
    #[tokio::test]
    async fn losing_racer_does_not_corrupt_remote() {
        use std::sync::atomic::{AtomicBool, Ordering};

        /// 包装远端：第一次条件写 manifest 前抢先无条件改写一次（模拟并发赢家）。
        struct RaceStore<'a> {
            inner: &'a MemStore,
            fired: AtomicBool,
        }
        impl RemoteStore for RaceStore<'_> {
            async fn test(&self) -> Result<(), SyncError> {
                self.inner.test().await
            }
            async fn ensure_dirs(&self) -> Result<(), SyncError> {
                self.inner.ensure_dirs().await
            }
            async fn get(
                &self,
                key: &str,
                max_bytes: u64,
            ) -> Result<Option<crate::RemoteObject>, SyncError> {
                self.inner.get(key, max_bytes).await
            }
            async fn put(
                &self,
                key: &str,
                body: Vec<u8>,
                condition: PutCondition,
            ) -> Result<(), SyncError> {
                if key == ARTIFACT_MANIFEST
                    && matches!(condition, PutCondition::IfMatch(_))
                    && !self.fired.swap(true, Ordering::SeqCst)
                {
                    // 并发赢家：同内容重写 manifest（etag 版本 +1）。
                    let current =
                        self.inner.get(key, u64::MAX).await?.ok_or_else(|| {
                            SyncError::Remote("竞态注入前 manifest 应存在".into())
                        })?;
                    self.inner
                        .put(key, current.data, PutCondition::None)
                        .await?;
                }
                self.inner.put(key, body, condition).await
            }
            async fn delete(&self, key: &str) -> Result<(), SyncError> {
                self.inner.delete(key).await
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let store = MemStore::default();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let pool = mcpane_storage::open_database(&dir.path().join("a.db"))
            .await
            .unwrap();
        let engine = SyncEngine::new(&pool, &home);
        let up1 = engine.upload(&store, None, false).await.unwrap();

        // 本地写入新数据后重传：预检通过（基线=远端），条件写被抢先 → 412。
        mcpane_storage::ServerRepo::new(pool.clone())
            .upsert(test_def("racer-local"))
            .await
            .unwrap();
        let racing = RaceStore {
            inner: &store,
            fired: AtomicBool::new(false),
        };
        let err = engine
            .upload(&racing, Some(up1.snapshot_id.as_str()), false)
            .await
            .unwrap_err();
        match err {
            SyncError::Conflict(info) => {
                // 412 后补取的冲突详情来自赢家 manifest。
                assert_eq!(info.snapshot_id.as_deref(), Some(up1.snapshot_id.as_str()));
            }
            other => panic!("应为 Conflict，得到：{other}"),
        }

        // 核心断言：远端仍指向赢家快照且可完整下载（落败方 artifact 未污染）。
        let home_c = dir.path().join("home-c");
        std::fs::create_dir_all(&home_c).unwrap();
        let pool_c = mcpane_storage::open_database(&dir.path().join("c.db"))
            .await
            .unwrap();
        let down = SyncEngine::new(&pool_c, &home_c)
            .download(&store)
            .await
            .unwrap();
        assert_eq!(down.snapshot_id, up1.snapshot_id);
        let names: Vec<String> = mcpane_storage::ServerRepo::new(pool_c.clone())
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|d| d.slug)
            .collect();
        assert!(
            !names.contains(&"racer-local".to_owned()),
            "落败方的数据不得出现在远端快照里：{names:?}"
        );
    }

    /// 条件写竞态防线：预检通过后 manifest 被抢先改写 → put 412 → Conflict。
    #[tokio::test]
    async fn manifest_put_race_is_conflict() {
        let store = MemStore::default();
        // 直接在 store 层验证条件语义（引擎内的竞态窗口无法确定性触发）。
        store
            .put("manifest.json", b"v1".to_vec(), PutCondition::IfAbsent)
            .await
            .unwrap();
        // 二次 IfAbsent → 冲突；旧 etag IfMatch → 冲突；新 etag 放行。
        let err = store
            .put("manifest.json", b"v2".to_vec(), PutCondition::IfAbsent)
            .await
            .unwrap_err();
        assert!(matches!(err, SyncError::Conflict(_)));
        let etag = store
            .get("manifest.json", 1024)
            .await
            .unwrap()
            .unwrap()
            .etag
            .unwrap();
        let err = store
            .put(
                "manifest.json",
                b"v2".to_vec(),
                PutCondition::IfMatch("\"v0\"".into()),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SyncError::Conflict(_)));
        store
            .put("manifest.json", b"v2".to_vec(), PutCondition::IfMatch(etag))
            .await
            .unwrap();
    }

    fn test_def(slug: &str) -> mcpane_storage::McpServerDef {
        mcpane_storage::McpServerDef {
            id: String::new(),
            slug: slug.to_owned(),
            name: slug.to_owned(),
            transport: mcpane_storage::ServerTransport::Stdio,
            command: Some("echo".into()),
            args: vec![],
            env: Default::default(),
            cwd: None,
            url: None,
            bearer: None,
            bearer_env: None,
            headers: Default::default(),
            runtime_policy: "shared".into(),
            enabled: true,
        }
    }
}
