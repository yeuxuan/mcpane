//! `mcpane://import/provider?...` 深链导入（ADR-023）。
//!
//! 链接来自网页/聊天，是不可信输入：解析成草稿后**只暂存在后端内存**，由前端弹确认框，
//! 用户点「导入」才落库。Key 不经事件与 IPC 返回值往返（预览里只有掩码），整条 URL
//! 不进日志。导入复用 `import_relays`：Key 转存系统凭据库、同 app 同地址自动跳过；
//! 导入后**不切换、不开路由**，由用户自己选择启用。

use std::sync::Mutex;

use mcpane_provider::{ImportDraft, ProviderProfile, RelayImportOutcome, parse_import_link};
use tauri::Emitter;

use super::provider::{provider_service, provider_write_guard};

/// 链接已暂存，前端应取预览（无载荷，避免经事件带出 Key）。
const EVENT_REQUEST: &str = "import-link-request";
/// 链接无效；载荷是固定文案，不含原链接。
const EVENT_ERROR: &str = "import-link-error";

/// 待确认的草稿：只留最新一条（用户连点两个链接，以后者为准）。
static PENDING: Mutex<Option<ImportDraft>> = Mutex::new(None);

/// 给界面展示的预览（不含 Key 明文）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub app: String,
    pub name: String,
    pub base_url: String,
    /// 目标主机名，界面突出显示，便于用户认出「这是不是我要导入的那家」。
    pub host: String,
    pub model: Option<String>,
    /// 链接里带了 Key 时的掩码（仅尾 4 位）；`None` = 链接没带，界面让用户自己填。
    pub key_hint: Option<String>,
}

fn preview_of(draft: &ImportDraft) -> ImportPreview {
    ImportPreview {
        app: draft.app.as_str().to_owned(),
        name: draft.name.clone(),
        base_url: draft.base_url.clone(),
        host: draft.host(),
        model: draft.model.clone(),
        key_hint: draft.api_key.as_deref().map(mask_key),
    }
}

fn mask_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() > 8 {
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("••••{tail}")
    } else {
        "••••".to_owned()
    }
}

/// 系统把 `mcpane://` 链接交给应用时调用（运行中回调 / 冷启动补取）。
pub fn handle_import_urls(app: &tauri::AppHandle, urls: impl IntoIterator<Item = String>) {
    for raw in urls {
        match parse_import_link(&raw) {
            Ok(draft) => {
                if let Ok(mut pending) = PENDING.lock() {
                    *pending = Some(draft);
                }
                crate::window::show_main_window(app);
                let _ = app.emit(EVENT_REQUEST, ());
            }
            Err(error) => {
                // 只记错误种类，不记链接（可能含 Key）。
                eprintln!("[mcpane] 导入链接无效: {error}");
                let _ = app.emit(EVENT_ERROR, error.to_string());
            }
        }
    }
}

/// 取当前待确认草稿的预览（不消耗）；前端挂载时与收到事件后各取一次。
#[tauri::command]
pub fn import_link_peek() -> Option<ImportPreview> {
    PENDING
        .lock()
        .ok()
        .and_then(|pending| pending.as_ref().map(preview_of))
}

/// 用户确认：落库并清掉草稿。链接没带 Key 时 `api_key` 可由界面补填。
#[tauri::command]
pub async fn import_link_confirm(
    handle: tauri::AppHandle,
    api_key: Option<String>,
) -> Result<RelayImportOutcome, String> {
    let override_key = api_key
        .map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty());
    if let Some(key) = &override_key
        && (key.chars().count() > 512 || key.chars().any(|c| c.is_whitespace() || c.is_control()))
    {
        return Err("API Key 含有空白或控制字符，或长度超限".into());
    }
    let draft = PENDING
        .lock()
        .map_err(|e| e.to_string())?
        .take()
        .ok_or("没有待导入的链接，可能已处理或已过期")?;
    let key = override_key.or(draft.api_key);
    let mut profile = ProviderProfile::new_relay(draft.app, draft.name);
    profile.base_url = Some(draft.base_url);
    profile.model = draft.model;
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let _guard = provider_write_guard();
        provider_service()
            .import_relays(vec![(profile, key)])
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    crate::refresh_tray_menu(&handle);
    Ok(outcome)
}

/// 用户取消：丢弃草稿。
#[tauri::command]
pub fn import_link_discard() {
    if let Ok(mut pending) = PENDING.lock() {
        *pending = None;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn key_mask_keeps_only_the_last_four_chars() {
        assert_eq!(mask_key("sk-abcdef123456"), "••••3456");
        assert_eq!(mask_key("short"), "••••", "短 Key 不泄露任何字符");
    }

    #[test]
    fn preview_never_carries_the_key_and_extracts_host() {
        let draft = parse_import_link(
            "mcpane://import/provider?app=claude&name=A&baseUrl=https://relay.example.com/v1&apiKey=sk-secret-value-9999",
        )
        .unwrap();
        let preview = preview_of(&draft);
        let json = serde_json::to_string(&preview).unwrap();
        assert!(!json.contains("sk-secret-value"), "{json}");
        assert_eq!(preview.host, "relay.example.com");
        assert_eq!(preview.key_hint.as_deref(), Some("••••9999"));
    }
}
