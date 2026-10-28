//! MCPane 数据云同步（对齐 cc-switch 的全量快照协议，安全模型收紧）。
//!
//! 协议：**全量快照覆盖**，upload 覆盖远端、download 覆盖本地，无增量
//! 无合并；并发上传由「基线预检 + manifest 条件写」乐观锁裁决。artifact：
//! - `mcpane-<sha12>.db`：业务数据快照（`SYNC_LOCAL_TABLES` 的日志/用量/
//!   本机设置数据已剔除，见 mcpane-storage::backup；`export_snapshot` 的
//!   `redact_downstream_credentials=true` 还会剥离 `mcp_servers` 的
//!   `bearer`/`env`/`headers`——下游 Server 的明文 Bearer Token / API Key，
//!   新设备下载后需重新填写，与 Provider Key 的既有体验一致）；
//! - `providers-<sha12>.json`：Provider 状态（只含 SecretReference 引用；
//!   上传前 `engine::redact_providers_json` 还会摘除 `desktopGateway`、
//!   profile 的 `headers`/`extraSettings`、以及 `appliedFragments`/各
//!   `*CommonSnippet` 等可能含明文的配置片段）；
//! - `manifest.json`（唯一固定名）：格式/版本/db 迁移版本/各 artifact
//!   摘要，最后条件写——**内容寻址命名保证并发落败方绝不污染胜者的
//!   artifact**，manifest 换名即原子切换；旧 artifact 在成功上传后清理。
//!
//! 与 cc-switch 的关键差异（安全红线）：
//! - **任何明文凭据都不上云**——API Key 在系统密钥库、不在 db/providers.json；
//!   下游 Server 的 bearer/env/headers 与本地代理 token 在导出/上传前剥离
//!   （上两条 artifact 说明）；远端连接凭据（WebDAV 密码 / S3 SK）也存密钥库，
//!   由命令层物化后注入；
//! - 下载导入是数据级（ATTACH + 列交集），本机日志/用量/设置保留。

pub mod engine;
pub mod manifest;
pub mod s3;
pub mod webdav;

pub use engine::{RemoteInfo, SyncEngine, SyncOutcome};
pub use manifest::{ArtifactMeta, Manifest, compute_snapshot_id, sha256_hex};
pub use s3::{S3Config, S3Store};
pub use webdav::{WebDavConfig, WebDavStore, is_jianguoyun};

/// 单个 artifact 上限。下载按 Content-Length 预检 + 全量落内存后复核
/// （非流式——桌面单用户场景 512MB 封顶可接受；真流式校验列为可选后续）。
pub const MAX_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;
/// manifest 上限。
pub const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("网络错误：{0}")]
    Network(String),
    #[error("远端响应异常：{0}")]
    Remote(String),
    #[error("数据校验失败：{0}")]
    Integrity(String),
    #[error("版本不兼容：{0}")]
    Incompatible(String),
    #[error("本地数据错误：{0}")]
    Local(String),
    #[error("云端快照已被其他设备更新，请先下载合并或强制覆盖")]
    Conflict(ConflictInfo),
}

/// 冲突详情（前端确认对话框展示对方设备/时间用；字段尽力而为）。
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictInfo {
    pub device_name: Option<String>,
    pub created_at: Option<i64>,
    pub snapshot_id: Option<String>,
}

impl From<mcpane_storage::StorageError> for SyncError {
    fn from(e: mcpane_storage::StorageError) -> Self {
        Self::Local(e.to_string())
    }
}

/// 远端对象（etag 供条件写乐观锁；服务器可能不提供）。
#[derive(Debug, Clone)]
pub struct RemoteObject {
    pub data: Vec<u8>,
    pub etag: Option<String>,
}

/// 写入前置条件（并发冲突防护的竞态窗口防线）。
#[derive(Debug, Clone)]
pub enum PutCondition {
    /// 无条件覆盖。
    None,
    /// 仅当远端 etag 未变（`If-Match`），否则 412 → Conflict。
    IfMatch(String),
    /// 仅当远端不存在（`If-None-Match: *`），否则 412 → Conflict。
    IfAbsent,
}

/// 远端对象存取抽象（WebDAV / S3 各自实现）。key 为相对 remote_root 的
/// 简单文件名（协议层保证不含路径分隔符之外的层级）。
#[allow(async_fn_in_trait)]
pub trait RemoteStore {
    /// 探测连通性与凭据（不产生副作用）。
    async fn test(&self) -> Result<(), SyncError>;
    /// 确保远端目录存在（幂等）。
    async fn ensure_dirs(&self) -> Result<(), SyncError>;
    /// 读取对象；不存在返回 Ok(None)。`max_bytes` 超限即错。
    async fn get(&self, key: &str, max_bytes: u64) -> Result<Option<RemoteObject>, SyncError>;
    /// 写入对象。条件不满足返回 `SyncError::Conflict`；服务器不支持
    /// 条件写（501）时实现方应降级为无条件写（主防线在引擎层预检）。
    async fn put(&self, key: &str, body: Vec<u8>, condition: PutCondition)
    -> Result<(), SyncError>;
    /// 删除对象（旧 artifact 清理用）；对象不存在视为成功。
    async fn delete(&self, key: &str) -> Result<(), SyncError>;
}

/// 错误信息里的 URL 脱敏（去 query，防 SigV4 预签名参数等泄漏进日志）。
pub(crate) fn redact_url(url: &str) -> String {
    match url.split_once('?') {
        Some((base, _)) => format!("{base}?…"),
        None => url.to_owned(),
    }
}
