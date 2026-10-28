//! Claude Code 的最小 Patch 生成与验证。

use mcpane_config::{
    ConfigDocument, CredentialDelivery, CredentialMode, PatchOp, PatchPlan, is_usable_inline_bearer,
};
use serde_json::json;

use crate::{DEFAULT_HUB_URL, MCP_ENTRY_NAME, TOKEN_ENV_VAR};

pub const HUB_ENTRY_KEY: &str = MCP_ENTRY_NAME;

/// 生成 Hub MCP Entry 的写入计划。
///
/// `prefix` 为 mcpServers 所在的 JSON 前缀路径（scope 决定，见 discovery）。
/// 凭据形态（ADR-015）：EnvVar 写 `${MCPANE_CLAUDE_TOKEN}` 引用（默认）；Inline 明文直写（可选）。
pub fn hub_entry_patch(
    prefix: &[String],
    hub_url: Option<&str>,
    credential: CredentialDelivery<'_>,
) -> PatchPlan {
    let mut path: Vec<String> = prefix.to_vec();
    path.push(HUB_ENTRY_KEY.to_owned());
    let authorization = match credential {
        CredentialDelivery::Inline(token) => format!("Bearer {token}"),
        CredentialDelivery::EnvVar => format!("Bearer ${{{TOKEN_ENV_VAR}}}"),
    };
    PatchPlan::new(vec![PatchOp::Set {
        path,
        value: json!({
            "type": "http",
            "url": hub_url.unwrap_or(DEFAULT_HUB_URL),
            "headers": {
                "Authorization": authorization
            }
        }),
    }])
}

/// 删除 MCPane 管理的 Hub Entry（不触碰其他 Server）。
pub fn remove_hub_entry_patch(prefix: &[String]) -> PatchPlan {
    let mut path: Vec<String> = prefix.to_vec();
    path.push(HUB_ENTRY_KEY.to_owned());
    PatchPlan::new(vec![PatchOp::Remove { path }])
}

/// 读取某前缀下的全部 MCP Server 名称（导入功能用）。
pub fn read_mcp_servers(doc: &ConfigDocument, prefix: &[String]) -> Vec<String> {
    let segs: Vec<&str> = prefix.iter().map(String::as_str).collect();
    match doc.get(&segs) {
        Some(serde_json::Value::Object(map)) => map.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// 业务验证：写入后的文档中 Hub Entry 必须结构完整，凭据形态与模式一致。
pub fn validate_hub_entry(
    doc: &ConfigDocument,
    prefix: &[String],
    mode: CredentialMode,
) -> Result<(), String> {
    let mut segs: Vec<&str> = prefix.iter().map(String::as_str).collect();
    segs.push(HUB_ENTRY_KEY);
    let entry = doc
        .get(&segs)
        .ok_or_else(|| format!("缺少 {} Entry", segs.join(".")))?;

    if entry["type"] != "http" {
        return Err("mcpane Entry type 必须为 http".into());
    }
    let url = entry["url"].as_str().unwrap_or_default();
    if !url.starts_with("http://127.0.0.1") && !url.starts_with("http://localhost") {
        return Err(format!("mcpane Entry url 必须指向 loopback：{url}"));
    }
    let auth = entry["headers"]["Authorization"]
        .as_str()
        .unwrap_or_default();
    match mode {
        CredentialMode::Inline => {
            if !is_usable_inline_bearer(auth) {
                return Err("Authorization 必须是可用的 Bearer Token（非空、非占位）".into());
            }
        }
        CredentialMode::EnvVar => {
            if !auth.contains(&format!("${{{TOKEN_ENV_VAR}}}")) {
                return Err(format!(
                    "Authorization 必须使用 ${{{TOKEN_ENV_VAR}}} 环境变量展开"
                ));
            }
        }
    }
    Ok(())
}

/// MCPane 管理的 Provider env 键：切换中转站时写入，切回官方订阅时全部移除。
///
/// 四个角色槽（haiku/sonnet/opus/fable）对应 Claude Code 的四个模型别名，
/// `CLAUDE_CODE_SUBAGENT_MODEL` 统管全部子代理/agent team/workflow 的模型。
/// 官方文档：<https://code.claude.com/docs/en/model-config#environment-variables>
pub const PROVIDER_ENV_KEYS: [&str; 9] = [
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_FABLE_MODEL",
    "CLAUDE_CODE_SUBAGENT_MODEL",
];

/// 历史遗留 / 他方工具留下的 env 键：MCPane 自己从不写入，但切换到任何
/// Provider（含切回官方）时都主动清除——它们全都是**绑在具体供应商上**的值，
/// 换了中转站还留着就会错路由或错标注。
///
/// - `ANTHROPIC_SMALL_FAST_MODEL`：官方已标 DEPRECATED，被
///   `ANTHROPIC_DEFAULT_HAIKU_MODEL` 取代。从旧工具迁移来的用户 live 里若残留
///   它，会持续把 haiku 流量路由到旧中转站的模型名。
/// - `ANTHROPIC_DEFAULT_*_MODEL_NAME`：`/model` 选择器里的显示名，缺省回落
///   模型 id。cc-switch 会写它们，所以从 cc-switch 迁移过来的用户 live 里挂的
///   是**上一个供应商**的模型显示名——切到新中转站后选择器会顶着个错标签。
pub const LEGACY_ENV_KEYS: [&str; 5] = [
    "ANTHROPIC_SMALL_FAST_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
    "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
    "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
    "ANTHROPIC_DEFAULT_FABLE_MODEL_NAME",
];

/// 生成 Provider env Patch（写入 `~/.claude/settings.json` 的 `env` 块）。
///
/// 调用方负责决定键值（如 `ANTHROPIC_BASE_URL`、`ANTHROPIC_MODEL`）。
/// Secret 值必须以环境变量引用或 Secret Store 物化方案传入（Phase 3 接管）。
pub fn provider_env_patch(env: &[(&str, &str)]) -> PatchPlan {
    PatchPlan::new(
        env.iter()
            .map(|(key, value)| PatchOp::Set {
                path: vec!["env".into(), (*key).to_owned()],
                value: json!(value),
            })
            .collect(),
    )
}

/// 移除 Provider env 键。
pub fn remove_provider_env_patch(keys: &[&str]) -> PatchPlan {
    PatchPlan::new(
        keys.iter()
            .map(|key| PatchOp::Remove {
                path: vec!["env".into(), (*key).to_owned()],
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use mcpane_config::ConfigFormat;

    fn user_prefix() -> Vec<String> {
        vec!["mcpServers".to_owned()]
    }

    #[test]
    fn inline_patch_writes_bearer_and_preserves_unknown_fields() {
        let mut doc = ConfigDocument::parse(
            r#"{"mcpServers": {"other": {"type": "stdio", "command": "npx"}}, "userState": 1}"#,
            ConfigFormat::Json,
        )
        .unwrap();
        hub_entry_patch(&user_prefix(), None, CredentialDelivery::Inline("tok1234"))
            .apply(&mut doc)
            .unwrap();

        validate_hub_entry(&doc, &user_prefix(), CredentialMode::Inline).unwrap();
        let out = doc.serialize();
        assert!(out.contains("Bearer tok1234"), "明文 Bearer 必须写入");
        assert!(out.contains("\"other\""), "其他 Server 保留");
        assert!(out.contains("userState"), "未知字段保留");
    }

    #[test]
    fn env_patch_uses_env_var_reference() {
        let mut doc = ConfigDocument::parse(r#"{"mcpServers": {}}"#, ConfigFormat::Json).unwrap();
        hub_entry_patch(&user_prefix(), None, CredentialDelivery::EnvVar)
            .apply(&mut doc)
            .unwrap();

        validate_hub_entry(&doc, &user_prefix(), CredentialMode::EnvVar).unwrap();
        let out = doc.serialize();
        assert!(out.contains("${MCPANE_CLAUDE_TOKEN}"), "必须是环境变量引用");
        // 两模式交叉校验互斥。
        assert!(validate_hub_entry(&doc, &user_prefix(), CredentialMode::Inline).is_err());
    }

    #[test]
    fn remove_hub_entry_keeps_other_servers() {
        let mut doc = ConfigDocument::parse(
            r#"{"mcpServers": {"mcpane": {"type": "http"}, "other": {"type": "stdio"}}}"#,
            ConfigFormat::Json,
        )
        .unwrap();
        remove_hub_entry_patch(&user_prefix())
            .apply(&mut doc)
            .unwrap();
        assert!(doc.get(&["mcpServers", "mcpane"]).is_none());
        assert!(doc.get(&["mcpServers", "other"]).is_some());
    }

    #[test]
    fn validation_rejects_placeholder_and_non_loopback() {
        // Inline 模式：占位/env 引用都不是可用凭据。
        let doc = ConfigDocument::parse(
            r#"{"mcpServers": {"mcpane": {"type": "http", "url": "http://127.0.0.1:24860/mcp",
                "headers": {"Authorization": "Bearer ${MCPANE_CLAUDE_TOKEN}"}}}}"#,
            ConfigFormat::Json,
        )
        .unwrap();
        assert!(
            validate_hub_entry(&doc, &user_prefix(), CredentialMode::Inline).is_err(),
            "Inline 模式下 env 引用不是可用凭据"
        );

        let doc = ConfigDocument::parse(
            r#"{"mcpServers": {"mcpane": {"type": "http", "url": "http://evil.com/mcp",
                "headers": {"Authorization": "Bearer tok"}}}}"#,
            ConfigFormat::Json,
        )
        .unwrap();
        assert!(
            validate_hub_entry(&doc, &user_prefix(), CredentialMode::Inline).is_err(),
            "非 loopback 必须被拒"
        );
    }

    #[test]
    fn read_servers_lists_names() {
        let doc =
            ConfigDocument::parse(r#"{"mcpServers": {"a": {}, "b": {}}}"#, ConfigFormat::Json)
                .unwrap();
        let mut names = read_mcp_servers(&doc, &user_prefix());
        names.sort();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn provider_env_patch_roundtrip() {
        let mut doc = ConfigDocument::parse(
            r#"{"env": {"USER_VAR": "keep"}, "permissions": {"allow": []}}"#,
            ConfigFormat::Json,
        )
        .unwrap();
        provider_env_patch(&[
            ("ANTHROPIC_BASE_URL", "https://api.example.com"),
            ("ANTHROPIC_MODEL", "claude-sonnet-5"),
        ])
        .apply(&mut doc)
        .unwrap();
        assert_eq!(
            doc.get(&["env", "ANTHROPIC_BASE_URL"]).unwrap(),
            json!("https://api.example.com")
        );
        assert_eq!(doc.get(&["env", "USER_VAR"]).unwrap(), json!("keep"));

        remove_provider_env_patch(&["ANTHROPIC_BASE_URL", "ANTHROPIC_MODEL"])
            .apply(&mut doc)
            .unwrap();
        assert!(doc.get(&["env", "ANTHROPIC_BASE_URL"]).is_none());
        assert_eq!(doc.get(&["env", "USER_VAR"]).unwrap(), json!("keep"));
        assert!(doc.get(&["permissions"]).is_some(), "未知字段保留");
    }
}
