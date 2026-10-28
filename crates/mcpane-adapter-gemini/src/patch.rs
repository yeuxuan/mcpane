//! Gemini CLI 的最小 Patch 生成（Provider env + auth 类型）。

use mcpane_config::{PatchOp, PatchPlan};
use serde_json::json;

/// MCPane 管理的 Provider env 键（`~/.gemini/.env`）：切换中转站时写入，
/// 切回官方登录时全部移除。注意 base_url 的键名是 `GOOGLE_GEMINI_BASE_URL`
/// （不是 GEMINI_BASE_URL——Gemini CLI 只认前者）。
pub const PROVIDER_ENV_KEYS: [&str; 3] =
    ["GOOGLE_GEMINI_BASE_URL", "GEMINI_API_KEY", "GEMINI_MODEL"];

/// `settings.json` 中 auth 类型的路径。漏写它会让 CLI 仍走上次的 auth 模式
///（.env 有 key 但 selectedType 还是 oauth 时 key 不生效）。
pub const AUTH_TYPE_PATH: [&str; 3] = ["security", "auth", "selectedType"];

/// 第三方中转站（API Key 认证）。
pub const AUTH_TYPE_API_KEY: &str = "gemini-api-key";
/// 官方 Google OAuth 登录。
pub const AUTH_TYPE_OAUTH: &str = "oauth-personal";

/// 生成 Provider env Patch（写入 `~/.gemini/.env`；路径为单段键）。
pub fn provider_env_patch(env: &[(&str, &str)]) -> PatchPlan {
    PatchPlan::new(
        env.iter()
            .map(|(key, value)| PatchOp::Set {
                path: vec![(*key).to_owned()],
                value: json!(value),
            })
            .collect(),
    )
}

/// 移除 Provider env 键（`~/.gemini/.env`）。
pub fn remove_provider_env_patch(keys: &[&str]) -> PatchPlan {
    PatchPlan::new(
        keys.iter()
            .map(|key| PatchOp::Remove {
                path: vec![(*key).to_owned()],
            })
            .collect(),
    )
}

/// 生成 auth 类型 Patch（写入 `~/.gemini/settings.json`）。
/// settings.json 走引擎 JSON patch：只触碰该路径，mcpServers 等用户配置不动。
pub fn auth_type_patch(selected_type: &str) -> PatchPlan {
    PatchPlan::new(vec![PatchOp::Set {
        path: AUTH_TYPE_PATH.iter().map(|s| (*s).to_owned()).collect(),
        value: json!(selected_type),
    }])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use mcpane_config::{ConfigDocument, ConfigFormat};

    #[test]
    fn env_patch_writes_flat_keys_and_preserves_others() {
        let raw = "# 用户注释\nGOOGLE_CLOUD_PROJECT=keep\n";
        let mut doc = ConfigDocument::parse(raw, ConfigFormat::Env).unwrap();
        provider_env_patch(&[
            ("GOOGLE_GEMINI_BASE_URL", "https://relay.example.com"),
            ("GEMINI_API_KEY", "sk-test"),
        ])
        .apply(&mut doc)
        .unwrap();
        let out = doc.serialize();
        assert!(out.contains("# 用户注释"));
        assert!(out.contains("GOOGLE_CLOUD_PROJECT=keep"));
        assert!(out.contains("GOOGLE_GEMINI_BASE_URL=https://relay.example.com"));
        assert!(out.contains("GEMINI_API_KEY=sk-test"));
    }

    #[test]
    fn remove_patch_clears_managed_keys_only() {
        let raw = "GOOGLE_GEMINI_BASE_URL=x\nGEMINI_API_KEY=y\nGEMINI_MODEL=z\nUSER=w\n";
        let mut doc = ConfigDocument::parse(raw, ConfigFormat::Env).unwrap();
        remove_provider_env_patch(&PROVIDER_ENV_KEYS)
            .apply(&mut doc)
            .unwrap();
        assert_eq!(doc.serialize(), "USER=w\n");
    }

    #[test]
    fn auth_type_patch_touches_only_selected_type() {
        let raw = r#"{"mcpServers":{"keep":{"httpUrl":"http://x"}},"security":{"auth":{"selectedType":"oauth-personal"}}}"#;
        let mut doc = ConfigDocument::parse(raw, ConfigFormat::Json).unwrap();
        auth_type_patch(AUTH_TYPE_API_KEY).apply(&mut doc).unwrap();
        assert_eq!(
            doc.get(&["security", "auth", "selectedType"]).unwrap(),
            serde_json::json!("gemini-api-key")
        );
        assert!(
            doc.get(&["mcpServers", "keep"]).is_some(),
            "用户配置必须保留"
        );
    }

    #[test]
    fn auth_type_patch_creates_missing_parents() {
        let mut doc = ConfigDocument::parse("{}", ConfigFormat::Json).unwrap();
        auth_type_patch(AUTH_TYPE_OAUTH).apply(&mut doc).unwrap();
        assert_eq!(
            doc.get(&["security", "auth", "selectedType"]).unwrap(),
            serde_json::json!("oauth-personal")
        );
    }
}
