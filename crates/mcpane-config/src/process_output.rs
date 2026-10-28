//! 子进程输出解码。
//!
//! 放在本 crate 是为了收敛 unsafe：7 个 adapter 都要解 CLI 的 `--version` 输出，
//! 而解码在 Windows 上只能走 Win32 FFI。各自实现一份等于把同一段 unsafe 复制
//! 七遍，对 `unsafe_code = "deny"` 的工作区是明显退步——它们的公共依赖里只有
//! 本 crate 已经因 `permissions` 链了 `windows`，故一并落这里。

/// 解码子进程输出。
///
/// 中文 Windows 的 CLI 常按系统代码页（CP936 等）而非 UTF-8 输出，
/// `from_utf8_lossy` 会把整段变成替换字符，版本号就此解析不出来。
pub fn decode(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    #[cfg(windows)]
    if let Some(text) = windows_codepage::decode(bytes) {
        return text;
    }
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(windows)]
mod windows_codepage {
    // 豁免理由：查询系统代码页并转码只有 Win32 FFI 一条路，windows crate 无安全
    // 封装。unsafe 收敛在本模块。
    #![allow(unsafe_code)]

    use windows::Win32::Globalization::{
        GetACP, GetOEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS, MultiByteToWideChar,
    };

    /// 依次试 ANSI 代码页与 OEM 代码页（控制台默认用后者）。
    pub(super) fn decode(bytes: &[u8]) -> Option<String> {
        let (acp, oemcp) = unsafe { (GetACP(), GetOEMCP()) };
        for codepage in [acp, oemcp] {
            if codepage == 0 {
                continue;
            }
            if let Some(text) = decode_with(bytes, codepage) {
                return Some(text);
            }
        }
        None
    }

    fn decode_with(bytes: &[u8], codepage: u32) -> Option<String> {
        // 先问需要多少 u16，再按量分配。
        let needed =
            unsafe { MultiByteToWideChar(codepage, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, None) };
        if needed <= 0 {
            return None;
        }
        let mut wide = vec![0u16; needed as usize];
        let written = unsafe {
            MultiByteToWideChar(
                codepage,
                MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0),
                bytes,
                Some(&mut wide),
            )
        };
        if written <= 0 {
            return None;
        }
        wide.truncate(written as usize);
        String::from_utf16(&wide).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_passes_through() {
        assert_eq!(decode(b"2.1.207 (Claude Code)"), "2.1.207 (Claude Code)");
        assert_eq!(decode("版本 1.2.3".as_bytes()), "版本 1.2.3");
    }

    #[test]
    fn empty_input_is_empty_string() {
        assert_eq!(decode(b""), "");
    }

    /// 非 UTF-8 字节不得让调用方拿到 panic 或空串：Windows 上按代码页解，
    /// 其他平台退回 lossy。两条路都必须产出可解析的文本。
    #[test]
    fn non_utf8_still_yields_text() {
        // GBK 的「版本」= B0 E6 B1 BE，在 UTF-8 下非法。
        let gbk = [0xB0, 0xE6, 0xB1, 0xBE, b' ', b'1', b'.', b'2', b'.', b'3'];
        let decoded = decode(&gbk);
        assert!(
            decoded.ends_with("1.2.3"),
            "版本号部分必须完好，实际：{decoded:?}"
        );
    }
}
