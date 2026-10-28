//! LaunchAgent plist 塞进 `<string>` 的文本转义。
//!
//! 开机自启（插件生成后再补字段）和 envsync（自己渲染整份 plist）共用，
//! 避免两处转义规则或 bundle id 各写一份、改了只改到一边。

/// 与 `tauri.conf.json` 的 `identifier`、NAMING.md 冻结值同步。
pub const BUNDLE_IDENTIFIER: &str = "io.mcpane.desktop";

pub fn xml_escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::{BUNDLE_IDENTIFIER, xml_escape};

    #[test]
    fn bundle_id_matches_frozen_identifier() {
        assert_eq!(BUNDLE_IDENTIFIER, "io.mcpane.desktop");
    }

    #[test]
    fn escapes_ampersand_first_so_it_is_not_double_encoded() {
        assert_eq!(xml_escape("a&b<c>d"), "a&amp;b&lt;c&gt;d");
    }
}
