//! 凭据类文件的属主独占权限。
//!
//! 配置与快照里可能带 Hub Token、OAuth Token 明文，落盘后必须只有本人能读。

use std::path::Path;

/// 把已存在的文件收紧到「仅属主可读写」。
///
/// - Unix：`chmod 0600`；
/// - Windows：DACL 收紧到仅对象 owner。
///
/// Windows 侧此前是空实现。补它不是因为现状不安全——`%APPDATA%` 的默认 ACL
/// 本就只给本人 + SYSTEM + Administrators，其他标准用户已经进不来。补的是第二道
/// 防线：共享 / RDP / Citrix 主机上多个交互式用户同时在线，以及目录继承被外力
/// 放宽时（域漫游 Profile、AppData 被同步到网络盘），显式 DACL 仍然成立。
///
/// 防不住本机 Administrator 与 SYSTEM——它们靠特权绕过 DACL，跟 Unix 上 0600
/// 挡不住 root 是一回事；也防不住文件被复制出 NTFS 卷（那要靠加密，见
/// `mcpane-secrets` 的 DPAPI 降级）。
pub fn harden_to_owner_only(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(windows)]
    {
        windows_acl::restrict_to_owner(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Ok(())
    }
}

/// 新建文件并写入，全程只有属主可读写。
///
/// Unix 靠 `OpenOptions::mode` 在创建时就带上 0600（`mode` 只在创建时生效，
/// 所以调用方必须写全新文件而不是重写已有文件）；Windows 没有等价的原子方式，
/// 退化为「写完立刻收紧 DACL」——存在一个极短的窗口，但目标目录本身已经排除了
/// 其他标准用户，可接受。
pub fn write_owner_only(path: &Path, content: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(content.as_bytes())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, content)?;
        // fail-closed：收紧失败就把刚写的文件删掉，不留一个权限状态未知、
        // 却已经含明文 Token 的文件在盘上。
        if let Err(error) = harden_to_owner_only(path) {
            let _ = std::fs::remove_file(path);
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(windows)]
mod windows_acl {
    // 豁免理由：设置文件 DACL 只有 Win32 FFI 一条路，windows crate 未提供安全
    // 封装。unsafe 收敛在本模块的三处调用。
    #![allow(unsafe_code)]

    use std::path::Path;

    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        SetFileSecurityW,
    };

    /// `D:P` 阻断继承（否则父目录的宽松 ACE 会流下来），`(A;;FA;;;OW)` 只给对象
    /// owner 完全访问。用 OW（OWNER_RIGHTS）而不是去 token 里查当前用户 SID：
    /// 文件由本进程创建，owner 即当前用户，少一整套 SID 查询代码。
    ///
    /// 刻意不给 Administrators / SYSTEM 显式条目——Unix 的 0600 也没有"管理员组
    /// 直通"，它们该靠特权走，语义才对得上。
    const OWNER_ONLY_SDDL: windows::core::PCWSTR = windows::core::w!("D:P(A;;FA;;;OW)");

    pub(super) fn restrict_to_owner(path: &Path) -> std::io::Result<()> {
        let sd = SecurityDescriptor::from_sddl(OWNER_ONLY_SDDL)?;
        let wide = wide_path(path);
        // PROTECTED 位是关键：不加的话继承来的 ACE 仍会叠加进最终 DACL。
        let info = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;
        let ok = unsafe {
            SetFileSecurityW(
                windows::core::PCWSTR(wide.as_ptr()),
                info,
                PSECURITY_DESCRIPTOR(sd.0.0),
            )
        };
        if ok.as_bool() {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    fn wide_path(path: &Path) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// SDDL 解析出的安全描述符，Drop 时 `LocalFree`。
    struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

    impl SecurityDescriptor {
        fn from_sddl(sddl: windows::core::PCWSTR) -> std::io::Result<Self> {
            let mut psd = PSECURITY_DESCRIPTOR::default();
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl,
                    SDDL_REVISION_1,
                    &mut psd,
                    None,
                )
                .map_err(|_| std::io::Error::last_os_error())?;
            }
            Ok(Self(psd))
        }
    }

    impl Drop for SecurityDescriptor {
        fn drop(&mut self) {
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.0.0)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn write_owner_only_creates_readable_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.json");
        write_owner_only(&path, r#"{"token":"x"}"#).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"token":"x"}"#,
            "收紧权限之后自己仍然要读得到"
        );
    }

    #[test]
    fn harden_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.json");
        std::fs::write(&path, "x").unwrap();
        harden_to_owner_only(&path).unwrap();
        harden_to_owner_only(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x");
    }

    /// 目标不存在时必须报错，不能静默"成功"——否则调用方会以为权限已收紧。
    #[test]
    fn harden_missing_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(harden_to_owner_only(&dir.path().join("absent.json")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unix_mode_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.json");
        write_owner_only(&path, "x").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "实际 {:o}", mode & 0o777);
    }
}
