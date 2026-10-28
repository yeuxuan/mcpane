//! Codex `config.toml` 的最小 Patch 生成与验证。

use mcpane_config::{
    ConfigDocument, CredentialDelivery, CredentialMode, PatchOp, PatchPlan, is_usable_inline_bearer,
};
use serde_json::json;

use crate::{DEFAULT_HUB_URL, MCP_ENTRY_NAME, TOKEN_ENV_VAR};

/// 生成 `[mcp_servers.mcpane]` 写入计划（TASK-601 目标语义）。
///
/// 按键逐个 Set（而非整表替换）：若用户在该表下有自定义键/注释，尽量保留。
/// 凭据两种形态互斥（ADR-015）：
/// - EnvVar（默认）：`bearer_token_env_var`，删除 `http_headers.Authorization`；
/// - Inline（可选）：`http_headers.Authorization` 明文，删除 `bearer_token_env_var`。
///
/// `required = false`：Hub 停机时 Codex 只是缺 mcpane 工具，不阻塞整个会话。
pub fn hub_entry_patch(hub_url: Option<&str>, credential: CredentialDelivery<'_>) -> PatchPlan {
    let base = ["mcp_servers", MCP_ENTRY_NAME];
    let entry = |key: &str, value: serde_json::Value| PatchOp::Set {
        path: base
            .iter()
            .map(|s| (*s).to_owned())
            .chain([key.to_owned()])
            .collect(),
        value,
    };
    let mut ops = vec![
        entry("url", json!(hub_url.unwrap_or(DEFAULT_HUB_URL))),
        entry("enabled", json!(true)),
        entry("required", json!(false)),
        entry("startup_timeout_sec", json!(10)),
        entry("tool_timeout_sec", json!(60)),
        // 存量清理：0.3.x 及更早的一键接入写过 `default_tools_approval_mode`，
        // 而 Codex 不认这个字段——平时静默忽略，`codex --strict-config` 下会让
        // 用户的 Codex 直接起不来。删掉自己留下的垃圾，不能指望用户去发现它。
        // remove 对不存在的键是 no-op（json5::remove 早返回），新用户无副作用。
        PatchOp::remove(&["mcp_servers", MCP_ENTRY_NAME, "default_tools_approval_mode"]),
    ];
    match credential {
        CredentialDelivery::Inline(token) => {
            ops.push(PatchOp::set(
                &[
                    "mcp_servers",
                    MCP_ENTRY_NAME,
                    "http_headers",
                    "Authorization",
                ],
                json!(format!("Bearer {token}")),
            ));
            ops.push(PatchOp::remove(&[
                "mcp_servers",
                MCP_ENTRY_NAME,
                "bearer_token_env_var",
            ]));
        }
        CredentialDelivery::EnvVar => {
            ops.push(entry("bearer_token_env_var", json!(TOKEN_ENV_VAR)));
            // 只删 Authorization，保留用户在 http_headers 下的其他自定义头。
            ops.push(PatchOp::remove(&[
                "mcp_servers",
                MCP_ENTRY_NAME,
                "http_headers",
                "Authorization",
            ]));
        }
    }
    PatchPlan::new(ops)
}

/// 删除 `[mcp_servers.mcpane]`（不影响其他 Server 表）。
pub fn remove_hub_entry_patch() -> PatchPlan {
    PatchPlan::new(vec![PatchOp::remove(&["mcp_servers", MCP_ENTRY_NAME])])
}

/// 读取全部已配置 MCP Server 名称（导入功能用）。
pub fn read_mcp_servers(doc: &ConfigDocument) -> Vec<String> {
    match doc.get(&["mcp_servers"]) {
        Some(serde_json::Value::Object(map)) => map.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// 业务验证：写入后 `[mcp_servers.mcpane]` 结构完整，凭据形态与模式一致（单源）。
pub fn validate_hub_entry(doc: &ConfigDocument, mode: CredentialMode) -> Result<(), String> {
    let entry = doc
        .get(&["mcp_servers", MCP_ENTRY_NAME])
        .ok_or("缺少 [mcp_servers.mcpane]")?;

    let url = entry["url"].as_str().unwrap_or_default();
    if !url.starts_with("http://127.0.0.1") && !url.starts_with("http://localhost") {
        return Err(format!("url 必须指向 loopback：{url}"));
    }
    let authorization = entry
        .get("http_headers")
        .and_then(|h| h.get("Authorization"))
        .and_then(|v| v.as_str());
    match mode {
        CredentialMode::Inline => {
            if !authorization.is_some_and(is_usable_inline_bearer) {
                return Err("http_headers.Authorization 必须是可用的 Bearer Token".into());
            }
            if entry.get("bearer_token_env_var").is_some() {
                return Err("Inline 模式下不得残留 bearer_token_env_var（双源歧义）".into());
            }
        }
        CredentialMode::EnvVar => {
            if entry["bearer_token_env_var"].as_str() != Some(TOKEN_ENV_VAR) {
                return Err(format!(
                    "必须使用 bearer_token_env_var = \"{TOKEN_ENV_VAR}\""
                ));
            }
            if authorization.is_some() {
                return Err("EnvVar 模式下不得残留 http_headers.Authorization（双源歧义）".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use mcpane_config::ConfigFormat;

    const USER_CONFIG: &str = r#"# 用户 Codex 配置
model = "o3"  # 保留注释

[mcp_servers.context7]  # 已有 server
command = "npx"
args = ["-y", "@upstash/context7-mcp"]

[unknown_future_table]
flag = true
"#;

    #[test]
    fn inline_patch_writes_bearer_and_preserves_everything_else() {
        let mut doc = ConfigDocument::parse(USER_CONFIG, ConfigFormat::Toml).unwrap();
        hub_entry_patch(None, CredentialDelivery::Inline("tok1234abcd"))
            .apply(&mut doc)
            .unwrap();
        validate_hub_entry(&doc, CredentialMode::Inline).unwrap();

        let out = doc.serialize();
        assert!(out.contains("# 用户 Codex 配置"));
        assert!(out.contains("model = \"o3\"  # 保留注释"));
        assert!(out.contains("[mcp_servers.context7]  # 已有 server"));
        assert!(out.contains("[unknown_future_table]"));
        assert!(out.contains("[mcp_servers.mcpane]"));
        assert!(
            out.contains("Authorization = \"Bearer tok1234abcd\""),
            "明文 Bearer 必须写入: {out}"
        );
        assert!(
            !out.contains("bearer_token_env_var"),
            "Inline 模式不得写 env 引用: {out}"
        );
        assert_eq!(
            doc.get(&["mcp_servers", "mcpane", "required"]).unwrap(),
            json!(false),
            "Hub 停机不应阻塞 Codex 会话"
        );
        assert_eq!(
            doc.get(&["mcp_servers", "mcpane", "startup_timeout_sec"])
                .unwrap(),
            json!(10)
        );
        assert_eq!(
            doc.get(&["mcp_servers", "mcpane", "tool_timeout_sec"])
                .unwrap(),
            json!(60)
        );
        // 曾经这里写过 `default_tools_approval_mode = "approve"`。
        // 真机复核（Codex 0.133，`codex --strict-config`）：
        // `unknown configuration field \`default_tools_approval_mode\``——
        // 这个键 config.toml 根本不认，非严格模式下被静默忽略，严格模式下
        // 直接让用户的 Codex 起不来。headless 取消的真正根因是缺
        // ToolAnnotations（见 docs/interop/codex-direct-http.md），已由内建
        // 工具补全注解治本。不写用不上的键。
        assert!(
            doc.get(&["mcp_servers", "mcpane", "default_tools_approval_mode"])
                .is_none(),
            "Codex 不认这个字段，写进去只会在 --strict-config 下炸掉用户的配置"
        );
    }

    /// 旧版接入留下的无效键必须被清掉：用户不会自己去发现它，而它在
    /// `codex --strict-config` 下会让 Codex 起不来。
    #[test]
    fn legacy_approval_key_is_removed_on_reconnect() {
        const LEGACY: &str = r#"
[mcp_servers.mcpane]
url = "http://127.0.0.1:24860/mcp"
enabled = true
default_tools_approval_mode = "approve"

[mcp_servers.other]
default_tools_approval_mode = "approve"
"#;
        let mut doc = ConfigDocument::parse(LEGACY, ConfigFormat::Toml).unwrap();
        assert!(
            doc.get(&["mcp_servers", "mcpane", "default_tools_approval_mode"])
                .is_some(),
            "夹具本身要带上遗留键，否则这个测试什么也没验"
        );

        hub_entry_patch(None, CredentialDelivery::Inline("tok"))
            .apply(&mut doc)
            .unwrap();

        assert!(
            doc.get(&["mcp_servers", "mcpane", "default_tools_approval_mode"])
                .is_none(),
            "接入时应清掉自己留下的无效键"
        );
        // 别人家的同名键不归我们管——只清 mcpane 段。
        assert_eq!(
            doc.get(&["mcp_servers", "other", "default_tools_approval_mode"])
                .unwrap(),
            json!("approve"),
            "不得越界删别的 server 的配置"
        );
    }

    #[test]
    fn env_patch_keeps_env_reference_and_no_plaintext() {
        let mut doc = ConfigDocument::parse(USER_CONFIG, ConfigFormat::Toml).unwrap();
        hub_entry_patch(None, CredentialDelivery::EnvVar)
            .apply(&mut doc)
            .unwrap();
        validate_hub_entry(&doc, CredentialMode::EnvVar).unwrap();

        let out = doc.serialize();
        assert!(out.contains("bearer_token_env_var = \"MCPANE_CODEX_TOKEN\""));
        assert!(
            !out.to_lowercase().contains("authorization"),
            "EnvVar 模式不得出现明文 header"
        );
    }

    #[test]
    fn switching_modes_removes_the_other_source() {
        let mut doc = ConfigDocument::parse(USER_CONFIG, ConfigFormat::Toml).unwrap();
        hub_entry_patch(None, CredentialDelivery::EnvVar)
            .apply(&mut doc)
            .unwrap();
        hub_entry_patch(None, CredentialDelivery::Inline("tok-new"))
            .apply(&mut doc)
            .unwrap();
        validate_hub_entry(&doc, CredentialMode::Inline).unwrap();

        let mut doc2 = ConfigDocument::parse(&doc.serialize(), ConfigFormat::Toml).unwrap();
        hub_entry_patch(None, CredentialDelivery::EnvVar)
            .apply(&mut doc2)
            .unwrap();
        validate_hub_entry(&doc2, CredentialMode::EnvVar).unwrap();
    }

    #[test]
    fn patch_is_key_level_and_keeps_user_custom_keys_in_entry() {
        let existing = "[mcp_servers.mcpane]\nurl = \"http://127.0.0.1:9999/mcp\"\nuser_note = \"我自己加的\" # 注释\n";
        let mut doc = ConfigDocument::parse(existing, ConfigFormat::Toml).unwrap();
        hub_entry_patch(None, CredentialDelivery::Inline("tok"))
            .apply(&mut doc)
            .unwrap();
        let out = doc.serialize();
        assert!(
            out.contains("user_note = \"我自己加的\" # 注释"),
            "表内自定义键保留: {out}"
        );
        assert_eq!(
            doc.get(&["mcp_servers", "mcpane", "url"]).unwrap(),
            json!("http://127.0.0.1:24860/mcp"),
            "url 更新为默认地址"
        );
    }

    #[test]
    fn remove_entry_leaves_other_servers_intact() {
        let mut doc = ConfigDocument::parse(USER_CONFIG, ConfigFormat::Toml).unwrap();
        hub_entry_patch(None, CredentialDelivery::Inline("tok"))
            .apply(&mut doc)
            .unwrap();
        remove_hub_entry_patch().apply(&mut doc).unwrap();
        let out = doc.serialize();
        assert!(!out.contains("mcpane"), "mcpane 表必须删除: {out}");
        assert!(out.contains("[mcp_servers.context7]"));
        assert!(out.contains("[unknown_future_table]"));
    }

    #[test]
    fn inline_validation_rejects_placeholder_and_dual_source() {
        // 占位符不可用。
        let placeholder = r#"[mcp_servers.mcpane]
url = "http://127.0.0.1:24860/mcp"
http_headers = { Authorization = "Bearer <接入时自动发放>" }
"#;
        let doc = ConfigDocument::parse(placeholder, ConfigFormat::Toml).unwrap();
        assert!(validate_hub_entry(&doc, CredentialMode::Inline).is_err());

        // 双源歧义：inline 校验拒绝残留 env 引用。
        let dual = r#"[mcp_servers.mcpane]
url = "http://127.0.0.1:24860/mcp"
bearer_token_env_var = "MCPANE_CODEX_TOKEN"
http_headers = { Authorization = "Bearer tok" }
"#;
        let doc = ConfigDocument::parse(dual, ConfigFormat::Toml).unwrap();
        assert!(validate_hub_entry(&doc, CredentialMode::Inline).is_err());
        assert!(validate_hub_entry(&doc, CredentialMode::EnvVar).is_err());
    }

    #[test]
    fn read_servers_lists_names() {
        let doc = ConfigDocument::parse(USER_CONFIG, ConfigFormat::Toml).unwrap();
        assert_eq!(read_mcp_servers(&doc), vec!["context7"]);
    }
}
