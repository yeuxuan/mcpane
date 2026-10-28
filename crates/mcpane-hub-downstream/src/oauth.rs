//! 下游远程 MCP 的 OAuth 登录（ADR-024）。
//!
//! 协议部分全部交给 rmcp 的 `auth` 模块：授权服务器发现（RFC 9728 / 8414）、动态客户端注册
//! （RFC 7591）、PKCE、换令牌、过期前自动刷新。这里只补三样：
//! 1. 令牌的**文件存储**（`FileCredentialStore`）；
//! 2. 一次登录的**两段式编排**（`OAuthLogins`：`begin` 产出授权链接，`complete` 凭回调换令牌）；
//! 3. 连接时把已存令牌挂到 HTTP 客户端上（`authorized_client`）。
//!
//! 令牌存文件而不是系统钥匙串：Windows 凭据存储放不下 OAuth 令牌（`CRED_MAX_CREDENTIAL_BLOB_SIZE`），
//! macOS 上 sidecar 读主应用写的钥匙串条目还会反复弹授权框；Hub 是令牌唯一的读写方。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rmcp::transport::auth::{
    AuthClient, AuthError, AuthorizationManager, AuthorizationRequest, CredentialStore, OAuthState,
    StoredCredentials,
};

use crate::DownstreamError;

/// 一次登录从发起到回调的最长间隔（用户在浏览器里点同意的时间）。
const PENDING_TTL: Duration = Duration::from_secs(10 * 60);
const CLIENT_NAME: &str = "MCPane";

/// 某个下游 Server 的 OAuth 令牌位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthSpec {
    pub dir: PathBuf,
    pub server_id: String,
}

impl OAuthSpec {
    pub fn credentials_file(&self) -> PathBuf {
        credentials_file(&self.dir, &self.server_id)
    }

    /// 是否已有令牌（登录过）。
    pub fn signed_in(&self) -> bool {
        self.credentials_file().is_file()
    }
}

/// 令牌文件路径：Server id 只留字母数字与 `-_`，杜绝路径穿越。
pub fn credentials_file(dir: &Path, server_id: &str) -> PathBuf {
    let safe: String = server_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.join(format!("{safe}.json"))
}

fn internal(error: impl std::fmt::Display) -> AuthError {
    AuthError::InternalError(error.to_string())
}

/// 文件版令牌存储：Unix 下目录 0700、文件 0600，写入走临时文件 + 原子改名。
pub struct FileCredentialStore {
    path: PathBuf,
}

impl FileCredentialStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

#[async_trait]
impl CredentialStore for FileCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let bytes = match tokio::fs::read(&self.path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(internal(error)),
        };
        // 文件被改坏等同于没登录过：重新登录即可覆盖，不让一个坏文件卡死连接。
        Ok(serde_json::from_slice(&bytes)
            .map_err(|error| {
                tracing::warn!(path = %self.path.display(), %error, "OAuth 令牌文件无法解析，按未登录处理");
            })
            .ok())
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        let bytes = serde_json::to_vec(&credentials).map_err(internal)?;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || write_private(&path, &bytes))
            .await
            .map_err(internal)?
            .map_err(internal)
    }

    async fn clear(&self) -> Result<(), AuthError> {
        match tokio::fs::remove_file(&self.path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(internal(error)),
        }
    }
}

/// 仅属主可读写地原子写文件。
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let Some(dir) = path.parent() else {
        return Err(std::io::Error::other("令牌路径没有父目录"));
    };
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

/// 删除令牌文件（退出登录、服务器被删除）。
pub fn forget(spec: &OAuthSpec) {
    let _ = std::fs::remove_file(spec.credentials_file());
}

/// 回收孤儿令牌文件：目录里不属于任何现存 Server 的 `*.json` 一律删掉。
pub fn prune(dir: &Path, live_server_ids: &[String]) {
    let keep: Vec<PathBuf> = live_server_ids
        .iter()
        .map(|id| credentials_file(dir, id))
        .collect();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") && !keep.contains(&path) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// 已登录的 Server：把令牌挂到 HTTP 客户端上，请求自带 `Authorization` 并在过期前自动刷新。
/// 没有令牌（或授权服务器已换、令牌作废）返回 `None`，调用方按未登录连接。
pub async fn authorized_client(
    spec: &OAuthSpec,
    url: &str,
    http: reqwest::Client,
) -> Result<Option<AuthClient<reqwest::Client>>, DownstreamError> {
    if !spec.signed_in() {
        return Ok(None);
    }
    let mut manager = AuthorizationManager::new(url).await.map_err(auth_error)?;
    manager.with_client(http.clone()).map_err(auth_error)?;
    manager.set_credential_store(FileCredentialStore::new(spec.credentials_file()));
    match manager.initialize_from_store().await {
        Ok(true) => Ok(Some(AuthClient::new(http, manager))),
        Ok(false) => Ok(None),
        Err(error) => Err(auth_error(error)),
    }
}

pub(crate) fn auth_error(error: AuthError) -> DownstreamError {
    DownstreamError::Auth(error.to_string())
}

struct Pending {
    state: OAuthState,
    started: Instant,
}

/// 进行中的登录（内存里，Hub 重启即作废，重新点登录即可）。
pub struct OAuthLogins {
    dir: PathBuf,
    pending: Mutex<HashMap<String, Pending>>,
}

impl OAuthLogins {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            pending: Mutex::new(HashMap::new()),
        }
    }

    pub fn spec(&self, server_id: &str) -> OAuthSpec {
        OAuthSpec {
            dir: self.dir.clone(),
            server_id: server_id.to_owned(),
        }
    }

    /// 第一段：做授权服务器发现与动态注册，返回要在浏览器里打开的授权链接。
    ///
    /// `challenge` 是服务器 401 时的 `WWW-Authenticate`，有则据此发现（不必再探测）。
    /// `redirect_uri` 由桌面端先绑好本机回环端口后传入——注册时就要用它。
    pub async fn begin(
        &self,
        server_id: &str,
        url: &str,
        challenge: Option<String>,
        redirect_uri: &str,
        proxy: &mcpane_net::ProxyConfig,
    ) -> Result<String, DownstreamError> {
        // 发现 / 注册 / 换令牌都走与下游连接同一条出网通道（代理、内网豁免）。
        let http = proxy
            .apply(reqwest::Client::builder(), url)
            .map_err(|e| DownstreamError::Config(e.to_string()))?
            .build()
            .map_err(|e| DownstreamError::Config(e.to_string()))?;
        let mut manager = AuthorizationManager::new(url).await.map_err(auth_error)?;
        manager.with_client(http).map_err(auth_error)?;
        manager.set_credential_store(FileCredentialStore::new(credentials_file(
            &self.dir, server_id,
        )));
        let mut state = OAuthState::Unauthorized(manager);
        let mut request = AuthorizationRequest::new(redirect_uri).with_client_name(CLIENT_NAME);
        if let Some(challenge) = challenge.filter(|c| !c.is_empty()) {
            request = request.with_challenge(challenge);
        }
        state
            .start_authorization(request)
            .await
            .map_err(auth_error)?;
        let auth_url = state.get_authorization_url().await.map_err(auth_error)?;
        let mut pending = self
            .pending
            .lock()
            .map_err(|e| DownstreamError::Auth(e.to_string()))?;
        pending.retain(|_, p| p.started.elapsed() < PENDING_TTL);
        pending.insert(
            server_id.to_owned(),
            Pending {
                state,
                started: Instant::now(),
            },
        );
        Ok(auth_url)
    }

    /// 第二段：凭浏览器回调带回的 `code` 与 `state` 换令牌并落盘。
    /// `state` 与第一段生成的不一致会被 SDK 拒绝（防 CSRF）。
    pub async fn complete(
        &self,
        server_id: &str,
        code: &str,
        state: &str,
    ) -> Result<(), DownstreamError> {
        let taken = self
            .pending
            .lock()
            .map_err(|e| DownstreamError::Auth(e.to_string()))?
            .remove(server_id);
        let Some(mut pending) = taken.filter(|p| p.started.elapsed() < PENDING_TTL) else {
            return Err(DownstreamError::Auth(
                "没有进行中的登录，或已超时，请重新点击登录".into(),
            ));
        };
        pending
            .state
            .handle_callback(code, state)
            .await
            .map_err(auth_error)
    }

    /// 放弃一次进行中的登录（用户取消）。
    pub fn cancel(&self, server_id: &str) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(server_id);
        }
    }

    /// 退出登录：删令牌文件、丢弃进行中的登录。
    pub fn logout(&self, server_id: &str) {
        self.cancel(server_id);
        forget(&self.spec(server_id));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn sample() -> StoredCredentials {
        StoredCredentials::new("client-1".into(), None, vec!["read".into()], Some(42))
    }

    #[test]
    fn file_name_never_escapes_the_directory() {
        let dir = Path::new("/data/mcp-oauth");
        for evil in ["../../etc/passwd", "a/b", "..", "x\\y", ""] {
            let path = credentials_file(dir, evil);
            assert_eq!(path.parent(), Some(dir), "{evil}");
            assert!(path.to_string_lossy().ends_with(".json"));
        }
        assert_eq!(
            credentials_file(dir, "srv_01-ab"),
            dir.join("srv_01-ab.json")
        );
    }

    #[tokio::test]
    async fn store_roundtrips_and_clears() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(tmp.path().join("oauth").join("s1.json"));
        assert!(store.load().await.unwrap().is_none());
        store.save(sample()).await.unwrap();
        let loaded = store.load().await.unwrap().unwrap();
        assert_eq!(loaded.client_id, "client-1");
        assert_eq!(loaded.granted_scopes, vec!["read".to_owned()]);
        store.clear().await.unwrap();
        assert!(store.load().await.unwrap().is_none());
        store.clear().await.unwrap(); // 重复清除不报错
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn token_file_and_directory_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("oauth").join("s1.json");
        FileCredentialStore::new(path.clone())
            .save(sample())
            .await
            .unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        assert!(
            !path.with_extension("json.tmp").exists(),
            "临时文件不应残留"
        );
    }

    #[tokio::test]
    async fn corrupt_file_means_signed_out_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s1.json");
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(
            FileCredentialStore::new(path)
                .load()
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn logout_and_prune_remove_only_what_they_should() {
        let tmp = tempfile::tempdir().unwrap();
        let logins = OAuthLogins::new(tmp.path().to_path_buf());
        for id in ["keep", "drop", "gone"] {
            std::fs::write(credentials_file(tmp.path(), id), b"{}").unwrap();
        }
        std::fs::write(tmp.path().join("note.txt"), b"x").unwrap();
        logins.logout("gone");
        assert!(!logins.spec("gone").signed_in());
        prune(tmp.path(), &["keep".to_owned()]);
        assert!(logins.spec("keep").signed_in());
        assert!(!logins.spec("drop").signed_in(), "孤儿令牌被回收");
        assert!(tmp.path().join("note.txt").exists(), "非令牌文件不碰");
    }

    #[tokio::test]
    async fn completing_without_a_pending_login_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let logins = OAuthLogins::new(tmp.path().to_path_buf());
        let error = logins.complete("s1", "code", "state").await.unwrap_err();
        assert!(error.to_string().contains("没有进行中的登录"), "{error}");
    }
}
