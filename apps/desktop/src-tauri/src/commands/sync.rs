//! 数据备份与云同步命令（Settings 页数据层）。
//!
//! - 本地备份：mcpane-storage::backup（VACUUM INTO 快照 + 数据级恢复）；
//! - 云同步：mcpane-sync（WebDAV / S3，全量快照协议）；
//! - 凭据红线：WebDAV 密码 / S3 SecretKey 只进系统密钥库，配置表只存引用；
//!   前端永不回传明文（只回传 has* 布尔）。
//! - 恢复/下载导入是数据级（不替换 db 文件），完成后通知 Hub reload +
//!   刷新托盘（providers.json 可能已变）。

use mcpane_secrets::{KeyringSecretStore, Secret, SecretReference, SecretStore};
use mcpane_sync::{RemoteStore, S3Config, S3Store, SyncEngine, WebDavConfig, WebDavStore};

use super::mcpane_home;

/// 全局同步互斥：手动上传/下载/自动同步串行（并发同步会互相覆盖工作产物）。
static SYNC_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 自动同步暂停标志：检出并发冲突后置位（避免每个防抖周期反复对着同一
/// 冲突重试刷错），任一次成功同步或用户改配置后恢复。
static AUTO_PAUSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

// ---------- 配置模型（存 app_settings.sync_config，JSON） ----------

/// 同步方式的三个合法值。字符串而非 enum：这个字段直接进 app_settings 的 JSON，
/// 老库里存着什么都可能读回来，反序列化必须不挑食——挑食就是启动即报错。
pub const PROVIDER_OFF: &str = "off";

/// 把任何读回来的值收敛成三选一，未知一律当「关闭」。
///
/// **必须在出口处调用**：`SyncSettings` 是 `#[derive(Default)]` 的，全新安装时
/// `provider` 是空串而不是 "off"；老库里也可能留着空串。空串在前端既不等于
/// "off"（卡片于是半开）、又不匹配分段器的任何一项（于是一个都不高亮）——
/// 界面变成"展开着但既没字段也没选中项"，用户根本读不出当前是什么状态。
/// 此前只有 auto_upload 一处顺手 `is_empty()` 兜了一下，等于把毛病留在源头。
fn normalized_provider(raw: &str) -> &'static str {
    match raw {
        "webdav" => "webdav",
        "s3" => "s3",
        _ => PROVIDER_OFF,
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SyncSettings {
    /// "off" | "webdav" | "s3"；读回来一律经 [`normalized_provider`] 收敛。
    pub provider: String,
    pub auto_sync: bool,
    pub webdav: WebDavForm,
    pub s3: S3Form,
    /// server-owned：命令层维护，set 时以存量为准。
    pub status: SyncStatus,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WebDavForm {
    pub base_url: String,
    pub username: String,
    pub remote_root: String,
    /// 密码的密钥库引用（明文绝不落库）。
    pub password_ref: Option<SecretReference>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct S3Form {
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    pub endpoint: String,
    pub remote_root: String,
    pub secret_key_ref: Option<SecretReference>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SyncStatus {
    pub last_sync_at: Option<i64>,
    pub last_snapshot_id: Option<String>,
    pub last_device_name: Option<String>,
    pub last_error: Option<String>,
    /// "manual" | "auto"
    pub last_error_source: Option<String>,
}

/// 前端视图：凭据引用收敛为 has* 布尔。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSettingsView {
    pub provider: String,
    pub auto_sync: bool,
    pub webdav: WebDavFormView,
    pub s3: S3FormView,
    pub status: SyncStatus,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDavFormView {
    pub base_url: String,
    pub username: String,
    pub remote_root: String,
    pub has_password: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct S3FormView {
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    pub endpoint: String,
    pub remote_root: String,
    pub has_secret_key: bool,
}

/// 保存输入：明文凭据可选——None/空串表示沿用已存凭据。
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSettingsInput {
    pub provider: String,
    pub auto_sync: bool,
    pub webdav: WebDavInput,
    pub s3: S3Input,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDavInput {
    pub base_url: String,
    pub username: String,
    pub remote_root: String,
    #[serde(default)]
    pub password: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct S3Input {
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    pub endpoint: String,
    pub remote_root: String,
    #[serde(default)]
    pub secret_key: Option<String>,
}

fn view_of(settings: &SyncSettings) -> SyncSettingsView {
    SyncSettingsView {
        // 收敛后再交给前端：TS 那边的联合类型 "off" | "webdav" | "s3" 得是真的。
        provider: normalized_provider(&settings.provider).to_owned(),
        auto_sync: settings.auto_sync,
        webdav: WebDavFormView {
            base_url: settings.webdav.base_url.clone(),
            username: settings.webdav.username.clone(),
            remote_root: settings.webdav.remote_root.clone(),
            has_password: settings.webdav.password_ref.is_some(),
        },
        s3: S3FormView {
            region: settings.s3.region.clone(),
            bucket: settings.s3.bucket.clone(),
            access_key_id: settings.s3.access_key_id.clone(),
            endpoint: settings.s3.endpoint.clone(),
            remote_root: settings.s3.remote_root.clone(),
            has_secret_key: settings.s3.secret_key_ref.is_some(),
        },
        status: settings.status.clone(),
    }
}

pub(crate) async fn load_settings() -> Result<SyncSettings, String> {
    let repo = mcpane_storage::SettingsRepo::new(super::db_pool().await?);
    let raw = repo.get_sync_config().await.map_err(|e| e.to_string())?;
    Ok(raw
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default())
}

async fn save_settings(settings: &SyncSettings) -> Result<(), String> {
    let repo = mcpane_storage::SettingsRepo::new(super::db_pool().await?);
    let json = serde_json::to_string(settings).map_err(|e| e.to_string())?;
    repo.set_sync_config(&json).await.map_err(|e| e.to_string())
}

/// 物化凭据并构造远端存取器。
enum AnyStore {
    WebDav(WebDavStore),
    S3(S3Store),
}

impl AnyStore {
    async fn test(&self) -> Result<(), mcpane_sync::SyncError> {
        match self {
            Self::WebDav(s) => s.test().await,
            Self::S3(s) => s.test().await,
        }
    }
}

async fn build_store(settings: &SyncSettings) -> Result<AnyStore, String> {
    match normalized_provider(&settings.provider) {
        "webdav" => {
            let form = &settings.webdav;
            if form.base_url.trim().is_empty() {
                return Err("请先填写 WebDAV 地址".into());
            }
            let reference = form.password_ref.clone().ok_or("尚未录入 WebDAV 密码")?;
            // Keychain 同步阻塞，移出 async 线程。
            let password = tauri::async_runtime::spawn_blocking(move || {
                KeyringSecretStore
                    .get(&reference)
                    .map_err(|e| format!("读取密码失败：{e}"))
            })
            .await
            .map_err(|e| e.to_string())??;
            Ok(AnyStore::WebDav(
                WebDavStore::new(WebDavConfig {
                    base_url: form.base_url.clone(),
                    username: form.username.clone(),
                    password: password.expose().to_owned(),
                    remote_root: effective_root(&form.remote_root),
                })
                .map_err(|e| e.to_string())?,
            ))
        }
        "s3" => {
            let form = &settings.s3;
            if form.bucket.trim().is_empty() {
                return Err("请先填写 S3 Bucket".into());
            }
            let reference = form
                .secret_key_ref
                .clone()
                .ok_or("尚未录入 S3 Secret Key")?;
            // Keychain 同步阻塞，移出 async 线程。
            let secret = tauri::async_runtime::spawn_blocking(move || {
                KeyringSecretStore
                    .get(&reference)
                    .map_err(|e| format!("读取密钥失败：{e}"))
            })
            .await
            .map_err(|e| e.to_string())??;
            Ok(AnyStore::S3(
                S3Store::new(S3Config {
                    region: form.region.clone(),
                    bucket: form.bucket.clone(),
                    access_key_id: form.access_key_id.clone(),
                    secret_access_key: secret.expose().to_owned(),
                    endpoint: form.endpoint.clone(),
                    remote_root: effective_root(&form.remote_root),
                })
                .map_err(|e| e.to_string())?,
            ))
        }
        _ => Err("尚未启用云同步".into()),
    }
}

fn effective_root(root: &str) -> String {
    let trimmed = root.trim().trim_matches('/');
    if trimmed.is_empty() {
        "mcpane-sync".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 记录同步结果到状态（成功清 error，失败记 error+来源）。
pub(crate) async fn record_status(result: &Result<mcpane_sync::SyncOutcome, String>, source: &str) {
    let Ok(mut settings) = load_settings().await else {
        return;
    };
    match result {
        Ok(outcome) => {
            settings.status.last_sync_at = Some(now_secs());
            settings.status.last_snapshot_id = Some(outcome.snapshot_id.clone());
            settings.status.last_device_name = Some(outcome.device_name.clone());
            settings.status.last_error = None;
            settings.status.last_error_source = None;
            // 成功同步（上传或下载）意味着基线已对齐：解除自动同步暂停。
            AUTO_PAUSED.store(false, std::sync::atomic::Ordering::Relaxed);
        }
        Err(error) => {
            settings.status.last_error = Some(error.clone());
            settings.status.last_error_source = Some(source.to_owned());
        }
    }
    let _ = save_settings(&settings).await;
}

// ---------- 本地备份命令 ----------

#[tauri::command]
pub async fn backup_create() -> Result<String, String> {
    let pool = super::db_pool().await?;
    mcpane_storage::backup_now(&pool, &mcpane_home())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn backup_list() -> Result<Vec<mcpane_storage::BackupEntry>, String> {
    Ok(mcpane_storage::list_backups(&mcpane_home()))
}

#[tauri::command]
pub async fn backup_restore(app: tauri::AppHandle, file_name: String) -> Result<(), String> {
    let _guard = SYNC_MUTEX.lock().await;
    let pool = super::db_pool().await?;
    mcpane_storage::restore_backup(&pool, &mcpane_home(), &file_name)
        .await
        .map_err(|e| e.to_string())?;
    // 业务数据变了：Hub 重载下游 + 托盘刷新。重载失败要上抛（恢复本身已成功，
    // 但 Hub 还带着旧下游在跑），不过收尾动作先做完——半途 return 会叠加故障。
    let reload = super::hub::notify_hub_reload(&app).await;
    // providers.json 边车已被整份替换，其中的 desktop_gateway.token 可能变了，
    // 而运行中的 desktop_proxy 单例还攥着恢复前那份。不在这里重同步的话，要
    // 等到下次「用户手动改某个 Provider」才会自然刷新，期间 Claude Desktop
    // 配置里写的旧 token 与代理实际校验的新 token 对不上，只会看到一句
    // 「无效的本地网关 token」——恢复备份把用户带进了一个他无从归因的状态。
    let gateway = crate::desktop_proxy::sync_from_store().await.err();
    crate::refresh_tray_menu(&app);
    // 恢复本身已成功；收尾故障如实合并上抛，别让用户自己猜。
    match (reload, gateway) {
        (Ok(()), None) => Ok(()),
        (Err(r), None) => Err(r),
        (Ok(()), Some(g)) => Err(format!("备份已恢复，但本地网关同步失败：{g}")),
        (Err(r), Some(g)) => Err(format!("{r}；本地网关同步也失败：{g}")),
    }
}

#[tauri::command]
pub async fn backup_delete(file_name: String) -> Result<(), String> {
    mcpane_storage::delete_backup(&mcpane_home(), &file_name).map_err(|e| e.to_string())
}

// ---------- 云同步命令 ----------

#[tauri::command]
pub async fn sync_settings_get() -> Result<SyncSettingsView, String> {
    Ok(view_of(&load_settings().await?))
}

/// 保存同步配置。明文凭据仅在非空时更新密钥库；status 以存量为准。
#[tauri::command]
pub async fn sync_settings_set(input: SyncSettingsInput) -> Result<SyncSettingsView, String> {
    if !matches!(input.provider.as_str(), "off" | "webdav" | "s3") {
        return Err(format!("未知同步方式：{}", input.provider));
    }
    let mut settings = load_settings().await?;
    settings.provider = input.provider;
    settings.auto_sync = input.auto_sync;

    settings.webdav.base_url = input.webdav.base_url.trim().to_owned();
    settings.webdav.username = input.webdav.username.trim().to_owned();
    settings.webdav.remote_root = input.webdav.remote_root.trim().to_owned();
    if let Some(password) = input.webdav.password.as_deref().filter(|p| !p.is_empty()) {
        let reference = settings
            .webdav
            .password_ref
            .clone()
            .unwrap_or_else(SecretReference::generate);
        let secret = Secret::new(password);
        let store_ref = reference.clone();
        // Keychain 同步阻塞，移出 async 线程。
        tauri::async_runtime::spawn_blocking(move || {
            KeyringSecretStore
                .set(&store_ref, &secret)
                .map_err(|e| format!("保存密码失败：{e}"))
        })
        .await
        .map_err(|e| e.to_string())??;
        settings.webdav.password_ref = Some(reference);
    }

    settings.s3.region = input.s3.region.trim().to_owned();
    settings.s3.bucket = input.s3.bucket.trim().to_owned();
    settings.s3.access_key_id = input.s3.access_key_id.trim().to_owned();
    settings.s3.endpoint = input.s3.endpoint.trim().to_owned();
    settings.s3.remote_root = input.s3.remote_root.trim().to_owned();
    if let Some(secret) = input.s3.secret_key.as_deref().filter(|s| !s.is_empty()) {
        let reference = settings
            .s3
            .secret_key_ref
            .clone()
            .unwrap_or_else(SecretReference::generate);
        let secret = Secret::new(secret);
        let store_ref = reference.clone();
        // Keychain 同步阻塞，移出 async 线程。
        tauri::async_runtime::spawn_blocking(move || {
            KeyringSecretStore
                .set(&store_ref, &secret)
                .map_err(|e| format!("保存密钥失败：{e}"))
        })
        .await
        .map_err(|e| e.to_string())??;
        settings.s3.secret_key_ref = Some(reference);
    }

    save_settings(&settings).await?;
    // 配置变更（可能换了远端/凭据）：给自动同步重新尝试的机会。
    AUTO_PAUSED.store(false, std::sync::atomic::Ordering::Relaxed);
    Ok(view_of(&settings))
}

#[tauri::command]
pub async fn sync_test() -> Result<(), String> {
    let settings = load_settings().await?;
    let store = build_store(&settings).await?;
    store.test().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn sync_remote_info() -> Result<mcpane_sync::RemoteInfo, String> {
    let settings = load_settings().await?;
    let store = build_store(&settings).await?;
    let pool = super::db_pool().await?;
    let home = mcpane_home();
    let engine = SyncEngine::new(&pool, &home);
    match store {
        AnyStore::WebDav(s) => engine.remote_info(&s).await,
        AnyStore::S3(s) => engine.remote_info(&s).await,
    }
    .map_err(|e| e.to_string())
}

/// 上传结果：正常成功给 outcome；检出并发冲突给 conflict（交前端弹窗决策，
/// 不算失败不落 last_error）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadOutcome {
    pub outcome: Option<mcpane_sync::SyncOutcome>,
    pub conflict: Option<mcpane_sync::ConflictInfo>,
}

#[tauri::command]
pub async fn sync_upload(force: Option<bool>) -> Result<UploadOutcome, String> {
    let force = force.unwrap_or(false);
    let _guard = SYNC_MUTEX.lock().await;
    let settings = load_settings().await?;
    let last_synced = settings.status.last_snapshot_id.clone();
    let store = build_store(&settings).await?;
    let pool = super::db_pool().await?;
    let home = mcpane_home();
    let engine = SyncEngine::new(&pool, &home);
    let result = match store {
        AnyStore::WebDav(s) => engine.upload(&s, last_synced.as_deref(), force).await,
        AnyStore::S3(s) => engine.upload(&s, last_synced.as_deref(), force).await,
    };
    match result {
        Ok(outcome) => {
            record_status(&Ok(outcome.clone()), "manual").await;
            Ok(UploadOutcome {
                outcome: Some(outcome),
                conflict: None,
            })
        }
        Err(mcpane_sync::SyncError::Conflict(info)) => Ok(UploadOutcome {
            outcome: None,
            conflict: Some(info),
        }),
        Err(e) => {
            let message = e.to_string();
            record_status(&Err(message.clone()), "manual").await;
            Err(message)
        }
    }
}

#[tauri::command]
pub async fn sync_download(app: tauri::AppHandle) -> Result<mcpane_sync::SyncOutcome, String> {
    // 抑制自动同步：download 触发的 DB 写入不该反向触发 upload。
    let _suppress = crate::auto_sync::suppress();
    let _guard = SYNC_MUTEX.lock().await;
    let settings = load_settings().await?;
    let store = build_store(&settings).await?;
    let pool = super::db_pool().await?;
    let home = mcpane_home();
    let engine = SyncEngine::new(&pool, &home);
    let result = match store {
        AnyStore::WebDav(s) => engine.download(&s).await,
        AnyStore::S3(s) => engine.download(&s).await,
    }
    .map_err(|e| e.to_string());
    record_status(&result, "manual").await;
    if result.is_ok() {
        let reload = super::hub::notify_hub_reload(&app).await;
        // 同 backup_restore：下载覆盖了 providers.json，运行中的本地网关单例
        // 必须跟着重读，否则 Claude Desktop 会拿旧 token 撞上新校验。
        let gateway = crate::desktop_proxy::sync_from_store().await.err();
        crate::refresh_tray_menu(&app);
        // 数据已应用成功；收尾故障是独立问题，如实合并上抛。
        match (reload, gateway) {
            (Ok(()), None) => {}
            (Err(r), None) => return Err(r),
            (Ok(()), Some(g)) => return Err(format!("云端数据已应用，但本地网关同步失败：{g}")),
            (Err(r), Some(g)) => return Err(format!("{r}；本地网关同步也失败：{g}")),
        }
    }
    result
}

/// 自动同步 worker 调用的上传入口（共享互斥与状态记录）。
pub(crate) async fn auto_upload() {
    let Ok(settings) = load_settings().await else {
        return;
    };
    if !settings.auto_sync || normalized_provider(&settings.provider) == PROVIDER_OFF {
        return;
    }
    // 冲突暂停中：等用户在设置页裁决（成功同步/改配置后自动恢复）。
    if AUTO_PAUSED.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let _guard = SYNC_MUTEX.lock().await;
    // 锁内重读：等锁期间可能有手动下载/上传更新了基线，
    // 用锁前的旧基线会对着新远端误报冲突。
    let Ok(settings) = load_settings().await else {
        return;
    };
    let last_synced = settings.status.last_snapshot_id.clone();
    let result: Result<mcpane_sync::SyncOutcome, String> = async {
        let store = build_store(&settings).await?;
        let pool = super::db_pool().await?;
        let home = mcpane_home();
        let engine = SyncEngine::new(&pool, &home);
        // 自动同步永不 force：冲突置暂停标志，交由用户在设置页手动裁决。
        match store {
            AnyStore::WebDav(s) => engine.upload(&s, last_synced.as_deref(), false).await,
            AnyStore::S3(s) => engine.upload(&s, last_synced.as_deref(), false).await,
        }
        .map_err(|e| match e {
            mcpane_sync::SyncError::Conflict(info) => {
                AUTO_PAUSED.store(true, std::sync::atomic::Ordering::Relaxed);
                format!(
                    "云端已被其他设备（{}）更新，自动同步已暂停；请到设置页下载合并或强制上传",
                    info.device_name.as_deref().unwrap_or("未知")
                )
            }
            other => other.to_string(),
        })
    }
    .await;
    if let Err(error) = &result {
        eprintln!("[mcpane] 自动同步上传失败: {error}");
    }
    record_status(&result, "auto").await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归：全新安装时 `SyncSettings::default()` 的 provider 是空串，直接透给
    /// 前端会让设置页出现"配置区展开着、分段器一个都不高亮"的无解状态——
    /// 用户读不出当前到底开没开。出口必须收敛成三选一。
    #[test]
    fn view_normalizes_unknown_provider_to_off() {
        let fresh = SyncSettings::default();
        assert_eq!(fresh.provider, "", "默认值确实是空串（这正是要收敛的原因）");
        assert_eq!(view_of(&fresh).provider, PROVIDER_OFF);

        // 老库里可能留着任意值；未知一律当关闭，而不是原样透传。
        for raw in ["", "  ", "OFF", "gdrive", "WebDAV"] {
            let settings = SyncSettings {
                provider: raw.to_owned(),
                ..Default::default()
            };
            assert_eq!(view_of(&settings).provider, PROVIDER_OFF, "raw = {raw:?}");
        }

        // 合法值原样保留。
        for raw in ["off", "webdav", "s3"] {
            let settings = SyncSettings {
                provider: raw.to_owned(),
                ..Default::default()
            };
            assert_eq!(view_of(&settings).provider, raw);
        }
    }
}
