//! MCPane Secret Store（TASK-301）。
//!
//! - macOS：Keychain；Windows：Credential Manager（keyring crate 统一封装）；
//! - 持久层（SQLite/JSON）只保存 [`SecretReference`]，绝不保存明文；
//! - Keychain Service 名冻结为 `MCPane`（NAMING.md）；
//! - 读取出的明文只在内存中短暂存在，`Secret` 包装类型的 Debug/Serialize
//!   一律脱敏，防止意外进入日志或导出。

use std::fmt;

/// Keychain / Credential Manager 的 Service 名（冻结标识）。
pub const KEYCHAIN_SERVICE: &str = "MCPane";

/// 持久层可保存的 Secret 引用（不含任何敏感值）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretReference {
    /// Keychain account 名，格式 `mcpane/<uuid>`。
    pub account: String,
}

impl SecretReference {
    pub fn generate() -> Self {
        Self {
            account: format!("mcpane/{}", uuid::Uuid::new_v4().simple()),
        }
    }
}

/// 内存中的 Secret 明文包装：Debug/Display/Serialize 全部脱敏。
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// 取出明文——调用点应即用即弃，禁止存储或记日志。
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// UI 展示用掩码（保留前 4 位）。
    pub fn masked(&self) -> String {
        let prefix: String = self.0.chars().take(4).collect();
        format!("{prefix}****")
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret([REDACTED])")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

impl serde::Serialize for Secret {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // 导出/序列化路径一律脱敏（导出默认排除 Secret）。
        serializer.serialize_str("[REDACTED]")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("Secret 不存在：{account}")]
    NotFound { account: String },
    #[error("系统凭据库错误：{0}")]
    Backend(String),
}

/// Windows Credential Manager 单条凭据的 blob 上限：
/// `CRED_MAX_CREDENTIAL_BLOB_SIZE` = 5 * 512 = 2560 **字节**（wincred.h）。
const WINDOWS_CREDENTIAL_BLOB_LIMIT: usize = 2560;

/// 明文是否超出 Windows Credential Manager 容量（超出则走 DPAPI 文件降级）。
///
/// 口径必须与 keyring 一致：它按 `encode_utf16().len() * 2` 比上限
/// （`windows-native-keyring-store-1.1.0/src/utils.rs:79-90`），纯 ASCII 只能放
/// 1280 字符。此前拿 UTF-8 字节数比 2000，1281..=2000 字节的值被放行进 keyring
/// 再被打回（用户侧即「password encoded as UTF-16 … 2560 chars」），降级形同虚设。
///
/// 不加 `#[cfg(windows)]`：纯字符串计算，放这里才能在 macOS 上被测试覆盖。
pub fn exceeds_windows_credential_limit(value: &str) -> bool {
    value.encode_utf16().count() * 2 > WINDOWS_CREDENTIAL_BLOB_LIMIT
}

/// Secret 存取接口。
pub trait SecretStore: Send + Sync {
    fn set(&self, reference: &SecretReference, value: &Secret) -> Result<(), SecretError>;
    fn get(&self, reference: &SecretReference) -> Result<Secret, SecretError>;
    fn delete(&self, reference: &SecretReference) -> Result<(), SecretError>;
}

/// 系统凭据库实现（macOS Keychain / Windows Credential Manager）。
pub struct KeyringSecretStore;

impl KeyringSecretStore {
    fn entry(&self, reference: &SecretReference) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(KEYCHAIN_SERVICE, &reference.account)
            .map_err(|e| SecretError::Backend(e.to_string()))
    }

    /// 从系统凭据库移除该条目；本就不存在视为成功（语义是"确保它不在"）。
    fn discard_credential(&self, reference: &SecretReference) -> Result<(), SecretError> {
        match self.entry(reference)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretError::Backend(e.to_string())),
        }
    }
}

impl SecretStore for KeyringSecretStore {
    fn set(&self, reference: &SecretReference, value: &Secret) -> Result<(), SecretError> {
        #[cfg(windows)]
        if exceeds_windows_credential_limit(value.expose()) {
            windows_fallback::store(reference, value)?;
            // 值变大、改存降级文件：Credential Manager 里那条旧的必须清掉，
            // 与下方「值变小、改存 keyring 时清降级文件」严格对称。
            //
            // 不清的后果不止是残留一条陈旧凭据：`get` 只在降级文件**存在**时
            // 优先读它，一旦该文件丢失（MCPANE_HOME 被清、换 Windows 配置文件后
            // DPAPI 解不开、同步/备份漏掉 secrets 目录），读取就会穿透回
            // Credential Manager，把用户以为早已换掉的旧 Key 当成当前值继续用，
            // 表现为一串莫名其妙的鉴权失败。失败同样要报错不吞：新值此刻已安全
            // 落进降级文件，调用方重试是幂等的。
            return self.discard_credential(reference);
        }
        match self.entry(reference)?.set_password(value.expose()) {
            Ok(()) => {}
            // 兜底：keyring 换版本可能让两边口径错开。blob 上限不是用户能处置的
            // 东西，报出去只会卡死保存，静默降级到文件。
            #[cfg(windows)]
            Err(keyring::Error::TooLong(..)) => return windows_fallback::store(reference, value),
            Err(e) => return Err(SecretError::Backend(e.to_string())),
        }
        // 曾经超限落过文件、这次变小了：必须清掉降级文件。读取时它的优先级更高，
        // 残留就等于静默继续用旧凭据——这里失败要报错，不能吞。
        #[cfg(windows)]
        windows_fallback::discard(reference)?;
        Ok(())
    }

    fn get(&self, reference: &SecretReference) -> Result<Secret, SecretError> {
        // 降级文件优先：只有超限时才会存在，存在即说明它才是当前值。
        #[cfg(windows)]
        if let Some(secret) = windows_fallback::load(reference)? {
            return Ok(secret);
        }
        match self.entry(reference)?.get_password() {
            Ok(value) => Ok(Secret::new(value)),
            Err(keyring::Error::NoEntry) => Err(SecretError::NotFound {
                account: reference.account.clone(),
            }),
            Err(e) => Err(SecretError::Backend(e.to_string())),
        }
    }

    fn delete(&self, reference: &SecretReference) -> Result<(), SecretError> {
        #[cfg(windows)]
        windows_fallback::remove(reference);
        self.discard_credential(reference)
    }
}

/// Windows 上超出 Credential Manager 容量的 Secret 的落点：DPAPI 加密文件。
///
/// `CRED_MAX_CREDENTIAL_BLOB_SIZE` 是 5*512 = 2560 字节，而 keyring 在 Windows 走
/// `CredWriteW`（UTF-16），ASCII 内容的实际可用空间再腰斩到约 1280 字节。OAuth
/// 三件套（id/access/refresh token）动辄数 KB，必然写不进去——Codex 自己就报过
/// 这个错（openai/codex#10353）。
///
/// 选加密文件而非分片塞回 Credential Manager，是跟微软官方 git-credential-manager
/// 走同一条路：它专为这个上限加了 dpapi 存储后端（PR #464），Azure CLI 的
/// `msal_token_cache.bin` 也是 DPAPI 加密文件。分片要额外维护"多 key 原子性 +
/// 旧片清理"，不划算。
///
/// 防护边界要说清楚：DPAPI（CurrentUser）**挡不住**同一用户下的其他进程——它们
/// 照样能调 `CryptUnprotectData` 解开。它真正防的是设备丢失后离线挂盘读取，以及
/// 文件被误打包/误同步出本机（这两种场景 ACL 完全无能为力）。
#[cfg(windows)]
mod windows_fallback {
    // 豁免理由：DPAPI 只有 Win32 FFI，windows crate 没有安全封装。unsafe 收敛在
    // 本模块的两处调用，且都立刻把结果拷进 Vec 并释放 LocalAlloc 内存。
    #![allow(unsafe_code)]

    use std::path::PathBuf;

    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CryptProtectData, CryptUnprotectData,
    };

    use super::{Secret, SecretError, SecretReference};

    pub(super) fn store(reference: &SecretReference, value: &Secret) -> Result<(), SecretError> {
        let path = path_for(reference);
        let dir = path
            .parent()
            .ok_or_else(|| SecretError::Backend("降级目录路径无效".to_owned()))?;
        std::fs::create_dir_all(dir)
            .map_err(|e| SecretError::Backend(format!("创建 {} 失败：{e}", dir.display())))?;
        let encrypted = protect(value.expose().as_bytes())?;
        std::fs::write(&path, encrypted)
            .map_err(|e| SecretError::Backend(format!("写入 {} 失败：{e}", path.display())))?;
        mcpane_config::permissions::harden_to_owner_only(&path)
            .map_err(|e| SecretError::Backend(format!("收紧 {} 权限失败：{e}", path.display())))?;
        Ok(())
    }

    pub(super) fn load(reference: &SecretReference) -> Result<Option<Secret>, SecretError> {
        let path = path_for(reference);
        let encrypted = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(SecretError::Backend(format!(
                    "读取 {} 失败：{e}",
                    path.display()
                )));
            }
        };
        let plain = unprotect(&encrypted)?;
        String::from_utf8(plain)
            .map(|value| Some(Secret::new(value)))
            .map_err(|_| SecretError::Backend("降级文件内容不是合法 UTF-8".to_owned()))
    }

    /// 删除失败不算错：调用方（`delete`）的语义是"确保它不存在"，与 keyring 侧把
    /// `NoEntry` 当成功一致。
    pub(super) fn remove(reference: &SecretReference) {
        let _ = std::fs::remove_file(path_for(reference));
    }

    /// 同上，但失败要上报——用在"值变小、改存 keyring"的路径上，残留文件会让
    /// 后续读取继续命中旧值。
    pub(super) fn discard(reference: &SecretReference) -> Result<(), SecretError> {
        let path = path_for(reference);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(SecretError::Backend(format!(
                "清理旧降级文件 {} 失败：{e}",
                path.display()
            ))),
        }
    }

    /// account 形如 `mcpane/<uuid-simple>`，取末段做文件名（32 位十六进制，
    /// 不含路径分隔符与 Windows 保留字符）。
    fn path_for(reference: &SecretReference) -> PathBuf {
        let stem = reference
            .account
            .rsplit('/')
            .next()
            .unwrap_or(&reference.account);
        mcpane_hub_protocol::mcpane_home()
            .join("secrets")
            .join(format!("{stem}.dpapi"))
    }

    fn protect(plain: &[u8]) -> Result<Vec<u8>, SecretError> {
        let input = blob(plain);
        let mut output = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptProtectData(&input, None, None, None, None, 0, &mut output)
                .map_err(|e| SecretError::Backend(format!("DPAPI 加密失败：{e}")))?;
        }
        Ok(take_and_free(output))
    }

    fn unprotect(encrypted: &[u8]) -> Result<Vec<u8>, SecretError> {
        let input = blob(encrypted);
        let mut output = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptUnprotectData(&input, None, None, None, None, 0, &mut output)
                .map_err(|e| SecretError::Backend(format!("DPAPI 解密失败：{e}")))?;
        }
        Ok(take_and_free(output))
    }

    fn blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: bytes.len() as u32,
            // Win32 的 blob 结构没有 const 版本，但 CryptProtectData/CryptUnprotectData
            // 只读输入，不会改写这块内存。
            pbData: bytes.as_ptr() as *mut u8,
        }
    }

    /// 把 DPAPI 经 `LocalAlloc` 分配的结果拷出来并释放，避免泄漏。
    fn take_and_free(output: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        if output.pbData.is_null() {
            return Vec::new();
        }
        let bytes =
            unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
        unsafe {
            let _ = LocalFree(Some(HLOCAL(output.pbData as *mut core::ffi::c_void)));
        }
        bytes
    }

    #[cfg(test)]
    mod tests {
        #![allow(clippy::unwrap_used)]

        use super::*;

        #[test]
        fn dpapi_roundtrip_handles_oauth_sized_payload() {
            // 3 个 JWT 量级：正是塞不进 Credential Manager 的那种。
            let plain = "x".repeat(4096);
            assert!(crate::exceeds_windows_credential_limit(&plain));
            let encrypted = protect(plain.as_bytes()).unwrap();
            assert_ne!(encrypted, plain.as_bytes(), "落盘不得是明文");
            assert_eq!(unprotect(&encrypted).unwrap(), plain.as_bytes());
        }

        #[test]
        fn small_values_still_go_to_credential_manager() {
            assert!(!crate::exceeds_windows_credential_limit("short-api-key"));
        }
    }
}

impl<S: SecretStore> SecretStore for std::sync::Arc<S> {
    fn set(&self, reference: &SecretReference, value: &Secret) -> Result<(), SecretError> {
        (**self).set(reference, value)
    }
    fn get(&self, reference: &SecretReference) -> Result<Secret, SecretError> {
        (**self).get(reference)
    }
    fn delete(&self, reference: &SecretReference) -> Result<(), SecretError> {
        (**self).delete(reference)
    }
}

/// 内存实现：测试与无凭据库环境（CI）用。
#[derive(Default)]
pub struct InMemorySecretStore {
    inner: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

impl SecretStore for InMemorySecretStore {
    fn set(&self, reference: &SecretReference, value: &Secret) -> Result<(), SecretError> {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(reference.account.clone(), value.expose().to_owned());
        Ok(())
    }

    fn get(&self, reference: &SecretReference) -> Result<Secret, SecretError> {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&reference.account)
            .map(|v| Secret::new(v.clone()))
            .ok_or_else(|| SecretError::NotFound {
                account: reference.account.clone(),
            })
    }

    fn delete(&self, reference: &SecretReference) -> Result<(), SecretError> {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&reference.account);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn secret_never_leaks_via_debug_display_or_serialize() {
        let secret = Secret::new("sk-super-secret-value");
        assert_eq!(format!("{secret:?}"), "Secret([REDACTED])");
        assert_eq!(format!("{secret}"), "[REDACTED]");
        assert_eq!(serde_json::to_string(&secret).unwrap(), "\"[REDACTED]\"");
        assert_eq!(secret.masked(), "sk-s****");
        // tracing 结构化字段走 Debug/Display，同样被覆盖。
    }

    #[test]
    fn reference_contains_no_secret_material() {
        let r = SecretReference::generate();
        assert!(r.account.starts_with("mcpane/"));
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("secret"), "{json}");
    }

    /// 回归（Windows 实证 2026-08-04）：降级判定要与 keyring 同口径（UTF-16
    /// 字节数 vs 2560），此前按 UTF-8 比 2000 会漏判整段区间。
    #[test]
    fn credential_limit_uses_utf16_bytes_like_keyring_does() {
        // ASCII 边界：1280 字符 = 2560 字节，恰好存得下。
        assert!(!exceeds_windows_credential_limit(&"a".repeat(1280)));
        assert!(exceeds_windows_credential_limit(&"a".repeat(1281)));
        // 旧实现（UTF-8 > 2000）漏判的区间。
        assert!(exceeds_windows_credential_limit(&"a".repeat(1500)));
        assert!(exceeds_windows_credential_limit(&"a".repeat(2000)));
        // 反方向：汉字 UTF-8 三字节、UTF-16 两字节，1000 字不该被误判超限。
        let cjk = "汉".repeat(1000);
        assert_eq!(cjk.len(), 3000, "前提：UTF-8 下每个汉字 3 字节");
        assert!(!exceeds_windows_credential_limit(&cjk));
        // 代理对占 2 个 UTF-16 单元 = 4 字节。
        assert!(!exceeds_windows_credential_limit(&"😀".repeat(640)));
        assert!(exceeds_windows_credential_limit(&"😀".repeat(641)));
    }

    #[test]
    fn in_memory_store_crud() {
        let store = InMemorySecretStore::default();
        let r = SecretReference::generate();
        assert!(matches!(store.get(&r), Err(SecretError::NotFound { .. })));
        store.set(&r, &Secret::new("v1")).unwrap();
        assert_eq!(store.get(&r).unwrap().expose(), "v1");
        store.set(&r, &Secret::new("v2")).unwrap();
        assert_eq!(store.get(&r).unwrap().expose(), "v2");
        store.delete(&r).unwrap();
        assert!(matches!(store.get(&r), Err(SecretError::NotFound { .. })));
    }

    /// 真实 Keychain 冒烟（仅 macOS 本机执行；CI 无凭据库时忽略）。
    #[test]
    #[cfg(target_os = "macos")]
    fn keyring_store_roundtrip_on_macos() {
        let store = KeyringSecretStore;
        let r = SecretReference::generate();
        let secret = Secret::new("mcpane-test-secret");
        if let Err(SecretError::Backend(e)) = store.set(&r, &secret) {
            // 无 UI 会话的环境（如部分 CI）可能拒绝 Keychain 访问。
            eprintln!("跳过 Keychain 冒烟（后端不可用）: {e}");
            return;
        }
        assert_eq!(store.get(&r).unwrap().expose(), "mcpane-test-secret");
        store.delete(&r).unwrap();
        assert!(matches!(store.get(&r), Err(SecretError::NotFound { .. })));
    }
}
