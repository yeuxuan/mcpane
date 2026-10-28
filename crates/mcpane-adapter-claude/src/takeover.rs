//! 接管体验：切到中转站时抹平 Claude Code 的"官方登录"摩擦点。
//!
//! 两个状态文件都不归引擎管（非受管配置、无凭据），走增量读改写：
//! - `~/.claude.json`：CLI 状态。`hasCompletedOnboarding=true` 让 CLI 不再
//!   弹首启引导（新装机器先配中转后装 CLI 的合法路径会踩到）。
//! - `~/.claude/config.json`：插件配置。`primaryApiKey="any"` 让插件/引导
//!   接受任意 Key 不再要求官方登录。
//!
//! 纪律：**只增量写自己的键、只清自己写过的值**——文件解析失败时返回
//! `Err` 交调用方降级为 warning，绝不覆盖用户数据（绝不把坏文件
//! 直接重置为 `{}`）。

/// 变换结果：`None` = 已是目标状态，无需写。
pub type TakeoverEdit = Result<Option<String>, String>;

fn parse_object(
    existing: Option<&str>,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    match existing {
        None => Ok(serde_json::Map::new()),
        Some(text) if text.trim().is_empty() => Ok(serde_json::Map::new()),
        Some(text) => match serde_json::from_str::<serde_json::Value>(text) {
            Ok(serde_json::Value::Object(map)) => Ok(map),
            Ok(_) => Err("顶层不是 JSON 对象".to_owned()),
            Err(error) => Err(format!("JSON 解析失败：{error}")),
        },
    }
}

fn render(map: serde_json::Map<String, serde_json::Value>) -> String {
    let mut text = serde_json::to_string_pretty(&serde_json::Value::Object(map))
        .unwrap_or_else(|_| "{}".to_owned());
    text.push('\n');
    text
}

/// `~/.claude.json`：置 `hasCompletedOnboarding=true`（保留其余键）。
pub fn onboarding_upsert(existing: Option<&str>) -> TakeoverEdit {
    let mut map = parse_object(existing)?;
    if map.get("hasCompletedOnboarding") == Some(&serde_json::Value::Bool(true)) {
        return Ok(None);
    }
    map.insert(
        "hasCompletedOnboarding".into(),
        serde_json::Value::Bool(true),
    );
    Ok(Some(render(map)))
}

/// `~/.claude/config.json`：置 `primaryApiKey="any"`（保留其余键）。
pub fn plugin_key_upsert(existing: Option<&str>) -> TakeoverEdit {
    let mut map = parse_object(existing)?;
    if map.get("primaryApiKey").and_then(|v| v.as_str()) == Some("any") {
        return Ok(None);
    }
    map.insert(
        "primaryApiKey".into(),
        serde_json::Value::String("any".into()),
    );
    Ok(Some(render(map)))
}

/// 切回官方：仅当 `primaryApiKey` 恰为我们写的 `"any"` 时移除——
/// 用户自设的其它值不动（只清自己写过的）。文件不存在/解析失败均视为无需动作。
pub fn plugin_key_clear(existing: Option<&str>) -> Option<String> {
    let mut map = parse_object(existing).ok()?;
    if map.get("primaryApiKey").and_then(|v| v.as_str()) != Some("any") {
        return None;
    }
    map.remove("primaryApiKey");
    Some(render(map))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn onboarding_preserves_user_keys_and_is_idempotent() {
        // 首建：文件不存在 → 只含我们的键。
        let fresh = onboarding_upsert(None).unwrap().unwrap();
        assert!(fresh.contains("\"hasCompletedOnboarding\": true"));
        // 增量：用户既有键保留。
        let merged = onboarding_upsert(Some(r#"{"numStartups": 5, "theme": "dark"}"#))
            .unwrap()
            .unwrap();
        assert!(merged.contains("numStartups"));
        assert!(merged.contains("hasCompletedOnboarding"));
        // 幂等：已是 true → 不写（No-op 不产生多余落盘与 mtime 扰动）。
        assert!(onboarding_upsert(Some(&fresh)).unwrap().is_none());
    }

    #[test]
    fn corrupt_state_file_is_left_untouched() {
        // 坏文件返回 Err 交上层降级为 warning——绝不重置用户数据
        //（cc-switch 对坏文件直接写 {}，刻意偏离）。
        assert!(onboarding_upsert(Some("{broken")).is_err());
        assert!(plugin_key_upsert(Some("[1,2]")).is_err());
        assert!(plugin_key_clear(Some("{broken")).is_none());
    }

    #[test]
    fn plugin_key_roundtrip_only_touches_own_value() {
        let written = plugin_key_upsert(Some(r#"{"installMethod": "brew"}"#))
            .unwrap()
            .unwrap();
        assert!(written.contains("\"primaryApiKey\": \"any\""));
        assert!(written.contains("installMethod"));
        // 幂等。
        assert!(plugin_key_upsert(Some(&written)).unwrap().is_none());
        // 切回官方：清掉自己写的 "any"，其余键保留。
        let cleared = plugin_key_clear(Some(&written)).unwrap();
        assert!(!cleared.contains("primaryApiKey"));
        assert!(cleared.contains("installMethod"));
        // 用户自设的真实 Key 不动。
        assert!(plugin_key_clear(Some(r#"{"primaryApiKey": "sk-user-real"}"#)).is_none());
        // 本就没有该键 → 无需动作。
        assert!(plugin_key_clear(Some(r#"{"other": 1}"#)).is_none());
        assert!(plugin_key_clear(None).is_none());
    }
}
