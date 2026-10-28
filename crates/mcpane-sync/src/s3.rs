//! S3 兼容存储的远端存取：reqwest + 手写 AWS SigV4，不引 aws-sdk 全家桶
//! （对齐 cc-switch 方案；且自带 SDK 会绕开 mcpane-net 的代理链路）。
//!
//! - AWS（endpoint 空或含 amazonaws.com）→ virtual-hosted style
//!   `https://{bucket}.s3.{region}.amazonaws.com/{key}`；
//! - 兼容服务（MinIO/R2/OSS…）→ path-style `{endpoint}/{bucket}/{key}`，
//!   endpoint 的 http:// 保留（MinIO 本地部署）。

use hmac::{Hmac, Mac};
use reqwest::{Method, StatusCode};
use sha2::Sha256;

use crate::manifest::sha256_hex;
use crate::{ConflictInfo, PutCondition, RemoteObject, RemoteStore, SyncError, redact_url};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct S3Config {
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    /// 明文密钥由命令层从系统密钥库物化后注入，不落任何配置文件。
    #[serde(skip_serializing, default)]
    pub secret_access_key: String,
    /// 兼容服务 endpoint（空 = AWS）。
    #[serde(default)]
    pub endpoint: String,
    #[serde(default = "crate::webdav::default_remote_root")]
    pub remote_root: String,
}

/// 凭据红线纵深防御：Debug 打印永不外泄 Secret Key。
impl std::fmt::Debug for S3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Config")
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"***")
            .field("endpoint", &self.endpoint)
            .field("remote_root", &self.remote_root)
            .finish()
    }
}

pub struct S3Store {
    config: S3Config,
}

impl S3Store {
    /// endpoint 允许留空（AWS）或裸主机名（默认按 https 拼装，兼容存量配置）；
    /// 显式写了 scheme 则只接受 http/https——SigV4 请求带凭据签名，
    /// 其它 scheme 只会把请求带出预期通道。
    pub fn new(config: S3Config) -> Result<Self, SyncError> {
        if let Some((scheme, _)) = config.endpoint.trim().split_once("://") {
            let scheme = scheme.to_ascii_lowercase();
            if scheme != "http" && scheme != "https" {
                return Err(SyncError::Local(format!(
                    "S3 endpoint 仅支持 http/https，当前 scheme：{scheme}"
                )));
            }
        }
        Ok(Self { config })
    }

    fn is_aws(&self) -> bool {
        self.config.endpoint.trim().is_empty() || self.config.endpoint.contains("amazonaws.com")
    }

    /// (scheme, host, url_path)：AWS virtual-hosted；兼容服务 path-style。
    fn endpoint_parts(&self, object_key: &str) -> Result<(String, String, String), SyncError> {
        let key_path = format!(
            "{}/v1/{object_key}",
            self.config.remote_root.trim_matches('/')
        );
        if self.is_aws() {
            let host = format!(
                "{}.s3.{}.amazonaws.com",
                self.config.bucket, self.config.region
            );
            Ok(("https".into(), host, format!("/{key_path}")))
        } else {
            let trimmed = self.config.endpoint.trim_end_matches('/');
            let (scheme, host) = match trimmed.split_once("://") {
                Some((s, h)) => (s.to_owned(), h.to_owned()),
                None => ("https".to_owned(), trimmed.to_owned()),
            };
            if host.is_empty() {
                return Err(SyncError::Local("S3 endpoint 无效".into()));
            }
            Ok((scheme, host, format!("/{}/{key_path}", self.config.bucket)))
        }
    }

    /// SigV4 签名后发起请求。path 段按 S3 规则编码（此处 key 均为安全文件名）。
    /// `extra_headers`（如条件写的 If-Match）一并纳入签名（AWS 要求条件头入签）。
    async fn signed_request(
        &self,
        method: Method,
        object_key: &str,
        body: Vec<u8>,
        extra_headers: &[(&str, String)],
    ) -> Result<reqwest::Response, SyncError> {
        let (scheme, host, path) = self.endpoint_parts(object_key)?;
        let url = format!("{scheme}://{host}{path}");

        let now = time_stamps();
        let payload_hash = sha256_hex(&body);

        // Canonical request（无 query；签 host + x-amz-* + 条件头，按名排序）。
        let mut header_pairs: Vec<(String, String)> = vec![
            ("host".into(), host.clone()),
            ("x-amz-content-sha256".into(), payload_hash.clone()),
            ("x-amz-date".into(), now.amz_date.clone()),
        ];
        for (name, value) in extra_headers {
            header_pairs.push((name.to_lowercase(), value.clone()));
        }
        header_pairs.sort_by(|a, b| a.0.cmp(&b.0));
        let canonical_headers: String = header_pairs
            .iter()
            .map(|(name, value)| format!("{name}:{value}\n"))
            .collect();
        let signed_headers = header_pairs
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(";");
        let canonical_request = format!(
            "{}\n{}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
            method.as_str(),
            uri_encode_path(&path),
        );

        let scope = format!("{}/{}/s3/aws4_request", now.date, self.config.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{scope}\n{}",
            now.amz_date,
            sha256_hex(canonical_request.as_bytes())
        );

        let signature = {
            let k_date = hmac(
                format!("AWS4{}", self.config.secret_access_key).as_bytes(),
                now.date.as_bytes(),
            );
            let k_region = hmac(&k_date, self.config.region.as_bytes());
            let k_service = hmac(&k_region, b"s3");
            let k_signing = hmac(&k_service, b"aws4_request");
            hex(&hmac(&k_signing, string_to_sign.as_bytes()))
        };

        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            self.config.access_key_id
        );

        let mut request = mcpane_net::client_for(&url)
            .request(method, &url)
            .header("Host", &host)
            .header("x-amz-date", &now.amz_date)
            .header("x-amz-content-sha256", &payload_hash)
            .header("Authorization", authorization);
        for (name, value) in extra_headers {
            request = request.header(*name, value.clone());
        }
        request
            .timeout(std::time::Duration::from_secs(300))
            .body(body)
            .send()
            .await
            .map_err(|e| SyncError::Network(format!("S3 请求失败（{}）：{e}", redact_url(&url))))
    }
}

impl RemoteStore for S3Store {
    async fn test(&self) -> Result<(), SyncError> {
        // HEAD manifest：404 也证明桶可达且凭据有效。
        let response = self
            .signed_request(Method::HEAD, "manifest.json", Vec::new(), &[])
            .await?;
        match response.status() {
            s if s.is_success() => Ok(()),
            StatusCode::NOT_FOUND => Ok(()),
            StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => Err(SyncError::Network(
                "认证失败：请检查 Access Key / Secret Key / 桶权限".into(),
            )),
            s => Err(SyncError::Remote(format!("S3 返回异常：HTTP {s}"))),
        }
    }

    async fn ensure_dirs(&self) -> Result<(), SyncError> {
        // 对象存储无目录概念。
        Ok(())
    }

    async fn get(&self, key: &str, max_bytes: u64) -> Result<Option<RemoteObject>, SyncError> {
        let response = self
            .signed_request(Method::GET, key, Vec::new(), &[])
            .await?;
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
                let etag = response
                    .headers()
                    .get(reqwest::header::ETAG)
                    .and_then(|v| v.to_str().ok())
                    .map(|v| v.to_owned());
                let data = response
                    .bytes()
                    .await
                    .map_err(|e| SyncError::Network(format!("下载中断：{e}")))?;
                if data.len() as u64 > max_bytes {
                    return Err(SyncError::Integrity(format!("{key} 超过大小上限")));
                }
                Ok(Some(RemoteObject {
                    data: data.to_vec(),
                    etag,
                }))
            }
            StatusCode::FORBIDDEN => Err(SyncError::Network("认证失败（HTTP 403）".into())),
            s => Err(SyncError::Remote(format!("GET {key} → HTTP {s}"))),
        }
    }

    async fn put(
        &self,
        key: &str,
        body: Vec<u8>,
        condition: PutCondition,
    ) -> Result<(), SyncError> {
        let headers: Vec<(&str, String)> = match &condition {
            PutCondition::IfMatch(etag) => vec![("If-Match", etag.clone())],
            PutCondition::IfAbsent => vec![("If-None-Match", "*".into())],
            PutCondition::None => vec![],
        };
        // conditional_final：最终那次请求是否带条件——501 降级重试后按
        // 无条件语义裁决状态码（避免把降级请求的 409 误判成冲突）。
        let mut conditional_final = !headers.is_empty();
        let status = if headers.is_empty() {
            self.signed_request(Method::PUT, key, body, &[])
                .await?
                .status()
        } else {
            let first = self
                .signed_request(Method::PUT, key, body.clone(), &headers)
                .await?
                .status();
            if first == StatusCode::NOT_IMPLEMENTED {
                // 兼容服务不支持条件写 → 降级无条件重试（主防线在引擎层预检）。
                conditional_final = false;
                self.signed_request(Method::PUT, key, body, &[])
                    .await?
                    .status()
            } else {
                first
            }
        };
        // AWS 条件写并发竞争返回 409 ConditionalRequestConflict，等同冲突。
        let status = if status == StatusCode::CONFLICT && conditional_final {
            StatusCode::PRECONDITION_FAILED
        } else {
            status
        };
        match status {
            s if s.is_success() => Ok(()),
            StatusCode::PRECONDITION_FAILED => Err(SyncError::Conflict(ConflictInfo::default())),
            StatusCode::FORBIDDEN => Err(SyncError::Network("认证失败（HTTP 403）".into())),
            s => Err(SyncError::Remote(format!("PUT {key} → HTTP {s}"))),
        }
    }

    async fn delete(&self, key: &str) -> Result<(), SyncError> {
        let response = self
            .signed_request(Method::DELETE, key, Vec::new(), &[])
            .await?;
        match response.status() {
            s if s.is_success() => Ok(()),
            StatusCode::NOT_FOUND => Ok(()),
            StatusCode::FORBIDDEN => Err(SyncError::Network("认证失败（HTTP 403）".into())),
            s => Err(SyncError::Remote(format!("DELETE {key} → HTTP {s}"))),
        }
    }
}

struct TimeStamps {
    /// `YYYYMMDD'T'HHMMSS'Z'`
    amz_date: String,
    /// `YYYYMMDD`
    date: String,
}

/// 当前 UTC 时间戳（SigV4 格式）。手写自 Unix 秒，避免引 chrono 的 tz 面。
fn time_stamps() -> TimeStamps {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = civil_from_unix(secs as i64);
    TimeStamps {
        amz_date: format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z"),
        date: format!("{y:04}{mo:02}{d:02}"),
    }
}

/// Unix 秒 → UTC 历法（Howard Hinnant civil_from_days 算法）。
fn civil_from_unix(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, mi, s) = (
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    );
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, h, mi, s)
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = match HmacSha256::new_from_slice(key) {
        Ok(mac) => mac,
        // HMAC-SHA256 接受任意长度 key，new_from_slice 实际不会失败。
        Err(_) => return Vec::new(),
    };
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

/// S3 canonical URI：按段编码，`/` 保留。
fn uri_encode_path(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            let mut out = String::new();
            for byte in segment.bytes() {
                match byte {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                        out.push(byte as char);
                    }
                    other => out.push_str(&format!("%{other:02X}")),
                }
            }
            out
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// 显式非 http/https scheme 在构造入口拒绝；裸主机名保持兼容（默认 https）。
    #[test]
    fn explicit_non_http_scheme_is_rejected() {
        let build = |endpoint: &str| {
            S3Store::new(S3Config {
                region: "us-east-1".into(),
                bucket: "b".into(),
                access_key_id: "ak".into(),
                secret_access_key: "sk".into(),
                endpoint: endpoint.into(),
                remote_root: "mcpane-sync".into(),
            })
        };
        assert!(build("ftp://minio.local").is_err());
        assert!(build("file:///srv/s3").is_err());
        assert!(build("minio.local:9000").is_ok(), "裸主机名是存量合法配置");
        assert!(build("http://minio.local:9000").is_ok(), "MinIO http 保留");
    }

    #[test]
    fn endpoint_styles_aws_vs_compatible() {
        let aws = S3Store::new(S3Config {
            region: "us-east-1".into(),
            bucket: "my-bucket".into(),
            access_key_id: "ak".into(),
            secret_access_key: "sk".into(),
            endpoint: String::new(),
            remote_root: "mcpane-sync".into(),
        })
        .unwrap();
        let (scheme, host, path) = aws.endpoint_parts("manifest.json").unwrap();
        assert_eq!(scheme, "https");
        assert_eq!(host, "my-bucket.s3.us-east-1.amazonaws.com");
        assert_eq!(path, "/mcpane-sync/v1/manifest.json");

        let minio = S3Store::new(S3Config {
            region: "us-east-1".into(),
            bucket: "b".into(),
            access_key_id: "ak".into(),
            secret_access_key: "sk".into(),
            endpoint: "http://127.0.0.1:9000".into(),
            remote_root: "mcpane-sync".into(),
        })
        .unwrap();
        let (scheme, host, path) = minio.endpoint_parts("mcpane.db").unwrap();
        assert_eq!(scheme, "http", "MinIO 本地 http 必须保留");
        assert_eq!(host, "127.0.0.1:9000");
        assert_eq!(path, "/b/mcpane-sync/v1/mcpane.db");
    }

    #[test]
    fn civil_from_unix_known_dates() {
        // 1970-01-01 00:00:00 与 2023-11-14 22:13:20（独立核对过的向量）。
        assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil_from_unix(1_700_000_000), (2023, 11, 14, 22, 13, 20));
        assert_eq!(civil_from_unix(1_784_937_600), (2026, 7, 25, 0, 0, 0));
    }

    /// HMAC-SHA256 权威向量（RFC 4231 Test Case 1/2）：签名派生链的基础正确性。
    /// SigV4 全链路正确性由真机 S3 端点验收（错误签名会被服务端 403 拒绝）。
    #[test]
    fn hmac_sha256_matches_rfc4231_vectors() {
        let key = [0x0bu8; 20];
        assert_eq!(
            hex(&hmac(&key, b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hex(&hmac(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }
}
