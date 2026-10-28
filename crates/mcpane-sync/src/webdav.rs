//! WebDAV 远端存取（reqwest 手写 PROPFIND/MKCOL/PUT/GET，Basic Auth）。
//!
//! - 走 `mcpane_net::client_for`（全局出网代理，ADR-017）；
//! - 建目录用**乐观 MKCOL**：直接建，遇 405/409（已存在）视为成功，
//!   省一半 PROPFIND 往返；
//! - 坚果云（jianguoyun/nutstore）常见配置错误给定向提示。

use reqwest::{Method, StatusCode};

use crate::{ConflictInfo, PutCondition, RemoteObject, RemoteStore, SyncError, redact_url};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDavConfig {
    /// 如 `https://dav.jianguoyun.com/dav/mcpane`。
    pub base_url: String,
    pub username: String,
    /// 明文密码由命令层从系统密钥库物化后注入，不落任何配置文件。
    #[serde(skip_serializing, default)]
    pub password: String,
    /// 远端根目录名（默认 mcpane-sync）。
    #[serde(default = "default_remote_root")]
    pub remote_root: String,
}

/// 凭据红线纵深防御：Debug 打印永不外泄明文密码。
impl std::fmt::Debug for WebDavConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebDavConfig")
            .field("base_url", &self.base_url)
            .field("username", &self.username)
            .field("password", &"***")
            .field("remote_root", &self.remote_root)
            .finish()
    }
}

pub fn default_remote_root() -> String {
    "mcpane-sync".to_owned()
}

/// 是否坚果云（错误提示定向用）。
pub fn is_jianguoyun(base_url: &str) -> bool {
    let lower = base_url.to_lowercase();
    lower.contains("jianguoyun.com") || lower.contains("nutstore")
}

pub struct WebDavStore {
    config: WebDavConfig,
}

impl WebDavStore {
    /// Basic Auth 会把密码随每个请求发出：base_url 必须是 http/https 完整
    /// URL，其它 scheme（file/ftp/…）与裸主机名在配置入口就拒绝，
    /// 而不是等 reqwest 在第一次同步时报一句难归因的错。
    pub fn new(config: WebDavConfig) -> Result<Self, SyncError> {
        let lower = config.base_url.trim().to_ascii_lowercase();
        if !lower.starts_with("http://") && !lower.starts_with("https://") {
            return Err(SyncError::Local(format!(
                "WebDAV 地址仅支持 http/https：{}",
                redact_url(&config.base_url)
            )));
        }
        Ok(Self { config })
    }

    /// `{base_url}/{remote_root}/v1[/key]`，逐段百分号编码。
    fn url_for(&self, key: Option<&str>) -> String {
        let base = self.config.base_url.trim_end_matches('/');
        let mut segments: Vec<String> = vec![
            encode_segment(self.config.remote_root.trim_matches('/')),
            "v1".to_owned(),
        ];
        if let Some(key) = key {
            segments.push(encode_segment(key));
        }
        format!("{base}/{}", segments.join("/"))
    }

    fn client(&self) -> reqwest::Client {
        mcpane_net::client_for(&self.config.base_url)
    }

    fn request(&self, method: Method, url: &str) -> reqwest::RequestBuilder {
        self.client()
            .request(method, url)
            .basic_auth(&self.config.username, Some(&self.config.password))
            .timeout(std::time::Duration::from_secs(300))
    }

    /// 发送 PUT（可选条件头），只返回状态码；状态语义由调用方裁决。
    async fn put_raw(
        &self,
        url: &str,
        body: Vec<u8>,
        condition: &PutCondition,
    ) -> Result<StatusCode, SyncError> {
        let mut request = self.request(Method::PUT, url);
        request = match condition {
            PutCondition::IfMatch(etag) => request.header("If-Match", etag.clone()),
            PutCondition::IfAbsent => request.header("If-None-Match", "*"),
            PutCondition::None => request,
        };
        let response = request
            .body(body)
            .send()
            .await
            .map_err(|e| self.friendly("上传失败", e.to_string()))?;
        Ok(response.status())
    }

    /// 网络错误 → 人话（坚果云定向提示）。
    fn friendly(&self, context: &str, detail: String) -> SyncError {
        if is_jianguoyun(&self.config.base_url) {
            SyncError::Network(format!(
                "{context}：{detail}。坚果云用户请检查：使用「第三方应用密码」而非登录密码；\
                 URL 需指向 /dav/ 下已在网页端创建的文件夹"
            ))
        } else {
            SyncError::Network(format!("{context}：{detail}"))
        }
    }
}

impl RemoteStore for WebDavStore {
    async fn test(&self) -> Result<(), SyncError> {
        // PROPFIND Depth: 0 探测 base_url 可达 + 凭据有效。
        let url = self.config.base_url.trim_end_matches('/').to_owned();
        let method =
            Method::from_bytes(b"PROPFIND").map_err(|e| SyncError::Local(e.to_string()))?;
        let response = self
            .request(method, &url)
            .header("Depth", "0")
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| self.friendly("连接失败", e.to_string()))?;
        match response.status() {
            s if s.is_success() || s == StatusCode::MULTI_STATUS => Ok(()),
            StatusCode::UNAUTHORIZED => {
                Err(self.friendly("认证失败", "用户名或密码错误（HTTP 401）".into()))
            }
            s => Err(self.friendly("服务器返回异常", format!("HTTP {s}"))),
        }
    }

    async fn ensure_dirs(&self) -> Result<(), SyncError> {
        // 乐观 MKCOL：{root} 与 {root}/v1 逐级建；405/409/301/302 视为已存在。
        let base = self.config.base_url.trim_end_matches('/');
        let root = format!(
            "{base}/{}",
            encode_segment(self.config.remote_root.trim_matches('/'))
        );
        let v1 = format!("{root}/v1");
        for dir in [&root, &v1] {
            let method =
                Method::from_bytes(b"MKCOL").map_err(|e| SyncError::Local(e.to_string()))?;
            let response = self
                .request(method, dir)
                .timeout(std::time::Duration::from_secs(30))
                .send()
                .await
                .map_err(|e| self.friendly("创建远端目录失败", e.to_string()))?;
            match response.status() {
                s if s.is_success() => {}
                StatusCode::METHOD_NOT_ALLOWED | StatusCode::CONFLICT => {}
                s if s.is_redirection() => {}
                StatusCode::UNAUTHORIZED => {
                    return Err(self.friendly("认证失败", "HTTP 401".into()));
                }
                s => {
                    return Err(self.friendly(
                        "创建远端目录失败",
                        format!("MKCOL {} → HTTP {s}", redact_url(dir)),
                    ));
                }
            }
        }
        Ok(())
    }

    async fn get(&self, key: &str, max_bytes: u64) -> Result<Option<RemoteObject>, SyncError> {
        let url = self.url_for(Some(key));
        let response = self
            .request(Method::GET, &url)
            .send()
            .await
            .map_err(|e| self.friendly("下载失败", e.to_string()))?;
        match response.status() {
            StatusCode::NOT_FOUND => Ok(None),
            s if s.is_success() => {
                if let Some(len) = response.content_length()
                    && len > max_bytes
                {
                    return Err(SyncError::Integrity(format!(
                        "{key} 超过大小上限（{len} 字节）"
                    )));
                }
                let etag = etag_of(response.headers());
                let data = response
                    .bytes()
                    .await
                    .map_err(|e| self.friendly("下载中断", e.to_string()))?;
                if data.len() as u64 > max_bytes {
                    return Err(SyncError::Integrity(format!("{key} 超过大小上限")));
                }
                Ok(Some(RemoteObject {
                    data: data.to_vec(),
                    etag,
                }))
            }
            StatusCode::UNAUTHORIZED => Err(self.friendly("认证失败", "HTTP 401".into())),
            s => Err(self.friendly("下载失败", format!("GET {} → HTTP {s}", redact_url(&url)))),
        }
    }

    async fn put(
        &self,
        key: &str,
        body: Vec<u8>,
        condition: PutCondition,
    ) -> Result<(), SyncError> {
        let url = self.url_for(Some(key));
        let status = match &condition {
            PutCondition::None => self.put_raw(&url, body, &PutCondition::None).await?,
            cond => {
                let first = self.put_raw(&url, body.clone(), cond).await?;
                if first == StatusCode::NOT_IMPLEMENTED {
                    // 服务器不支持条件写 → 降级无条件重试（主防线在引擎层预检）。
                    self.put_raw(&url, body, &PutCondition::None).await?
                } else {
                    first
                }
            }
        };
        match status {
            s if s.is_success() => Ok(()),
            StatusCode::PRECONDITION_FAILED => Err(SyncError::Conflict(ConflictInfo::default())),
            StatusCode::UNAUTHORIZED => Err(self.friendly("认证失败", "HTTP 401".into())),
            s => Err(self.friendly("上传失败", format!("PUT {} → HTTP {s}", redact_url(&url)))),
        }
    }

    async fn delete(&self, key: &str) -> Result<(), SyncError> {
        let url = self.url_for(Some(key));
        let response = self
            .request(Method::DELETE, &url)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| self.friendly("删除失败", e.to_string()))?;
        match response.status() {
            s if s.is_success() => Ok(()),
            StatusCode::NOT_FOUND | StatusCode::GONE => Ok(()),
            StatusCode::UNAUTHORIZED => Err(self.friendly("认证失败", "HTTP 401".into())),
            s => Err(self.friendly(
                "删除失败",
                format!("DELETE {} → HTTP {s}", redact_url(&url)),
            )),
        }
    }
}

/// 响应头里的 ETag（原样保留引号；If-Match 需回传服务器给的原文）。
fn etag_of(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_owned())
}

/// 单个路径段的百分号编码（保留非保留字符）。
fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// 非 http/https 的 base_url 在构造入口即拒绝——Basic Auth 凭据不进
    /// 未知通道。
    #[test]
    fn non_http_scheme_is_rejected() {
        for bad in ["ftp://host/dav", "file:///tmp/dav", "dav.example.com/x"] {
            let result = WebDavStore::new(WebDavConfig {
                base_url: bad.into(),
                username: "u".into(),
                password: "p".into(),
                remote_root: "mcpane-sync".into(),
            });
            assert!(result.is_err(), "{bad} 不该被接受");
        }
    }

    #[test]
    fn url_building_encodes_segments() {
        let store = WebDavStore::new(WebDavConfig {
            base_url: "https://dav.example.com/dav/box/".into(),
            username: "u".into(),
            password: "p".into(),
            remote_root: "my sync".into(),
        })
        .unwrap();
        assert_eq!(
            store.url_for(Some("manifest.json")),
            "https://dav.example.com/dav/box/my%20sync/v1/manifest.json"
        );
        assert_eq!(
            store.url_for(None),
            "https://dav.example.com/dav/box/my%20sync/v1"
        );
    }

    #[test]
    fn jianguoyun_detection() {
        assert!(is_jianguoyun("https://dav.jianguoyun.com/dav/x"));
        assert!(is_jianguoyun("https://dav.NutStore.com/dav"));
        assert!(!is_jianguoyun("https://dav.example.com"));
    }
}
