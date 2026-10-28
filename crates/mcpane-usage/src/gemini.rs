//! Gemini CLI 会话解析：`~/.gemini/tmp/<hash>/chats/session-*.json` → `UsageEvent`。
//!
//! 数据形态与 Claude/Codex 都不同：**单文件是一个 JSON 对象**（非 JSONL），
//! 顶层 `sessionId` + `messages[]`；`type=="gemini"` 的消息带
//! `tokens:{input,output,cached,thoughts}`（per-message 绝对值，无需 delta）。
//! 计费口径（对齐 cc-switch）：thoughts 按输出计费（output += thoughts）；
//! `cached` 是 input 的子集（Gemini API 的 cachedContentTokenCount ⊆
//! promptTokenCount），billable input 在查询层扣除 cache_read。

use crate::{
    FileParse, SyncCursor, UsageError, UsageEvent, file_stamp, normalize_model, parse_rfc3339_secs,
};
use std::fs;
use std::path::{Path, PathBuf};

/// 收集会话文件：`<gemini>/tmp/<project_hash>/chats/session-*.json`（固定深度）。
pub fn collect_gemini_files(gemini_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let tmp_dir = gemini_dir.join("tmp");
    let Ok(entries) = fs::read_dir(&tmp_dir) else {
        return files;
    };
    for entry in entries.flatten() {
        let chats = entry.path().join("chats");
        let Ok(chat_entries) = fs::read_dir(&chats) else {
            continue;
        };
        for chat in chat_entries.flatten() {
            let path = chat.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                files.push(path);
            }
        }
    }
    files
}

/// 解析单个 Gemini 会话文件。
///
/// - mtime（纳秒）未变 → `Ok(None)`，整文件跳过；
/// - 变了 → 整文件重新解析全部消息（单 JSON 无行游标可言），重复事件由
///   request_id 主键 + INSERT OR IGNORE 兜底。`cursor.last_line_offset`
///   记消息数（仅作"已处理过"标记）。
/// - 整文件 JSON 损坏（写入中途）→ 产出空事件集但**不推进 cursor**，
///   下轮重试。
pub fn parse_gemini_file(
    path: &Path,
    prev: Option<&SyncCursor>,
) -> Result<Option<FileParse>, UsageError> {
    let stamp = file_stamp(path);
    let mtime = stamp.modified_nanos;
    let prev = prev.copied().unwrap_or_default();
    // 未变判定（mtime+size，含 metadata 取不到时的 fail-safe）见 SyncCursor::is_unchanged。
    if prev.is_unchanged(stamp) {
        return Ok(None);
    }

    let content = fs::read_to_string(path)?;
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
        // 客户端可能正在写入：本轮不产出、不推进 cursor，下轮重读。
        return Ok(Some(FileParse {
            events: Vec::new(),
            cursor: prev,
        }));
    };

    let session_id = value
        .get("sessionId")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_owned();
    let empty = Vec::new();
    let messages = value
        .get("messages")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);

    let mut events = Vec::new();
    for msg in messages {
        if msg.get("type").and_then(|t| t.as_str()) != Some("gemini") {
            continue;
        }
        let Some(tokens) = msg.get("tokens").filter(|t| t.is_object()) else {
            continue;
        };
        let field =
            |key: &str| -> i64 { tokens.get(key).and_then(|v| v.as_i64()).unwrap_or(0).max(0) };
        let input = field("input");
        // thoughts（思考 token）按输出计费。
        let output = field("output") + field("thoughts");
        // cached 是 input 的子集，钳制防护异常数据。
        let cached = field("cached").min(input);

        let message_id = msg.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");
        let model = msg
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_owned();
        let ts = msg
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(parse_rfc3339_secs)
            .unwrap_or(0);

        let event = UsageEvent {
            request_id: format!("gemini_session:{session_id}:{message_id}"),
            ts,
            client_app: "gemini",
            model_key: normalize_model(&model),
            model,
            project: None,
            session_id: Some(session_id.clone()),
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cached,
            cache_creation_tokens: 0,
        };
        if event.has_billable_tokens() {
            events.push(event);
        }
    }

    let message_count = messages.len() as i64;
    Ok(Some(FileParse {
        events,
        cursor: SyncCursor {
            last_modified_nanos: mtime,
            last_size: stamp.size,
            // 至少 1：空会话文件也要能命中"mtime 未变即跳过"的快路径。
            last_line_offset: message_count.max(1),
        },
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mcpane-usage-gemini-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    const SESSION: &str = r#"{
      "sessionId": "sess-1",
      "messages": [
        {"id": "u1", "type": "user", "content": "hi"},
        {"id": "g1", "type": "gemini", "model": "gemini-3.1-pro-preview",
         "timestamp": "2026-07-10T03:00:02Z",
         "tokens": {"input": 1000, "output": 200, "cached": 400, "thoughts": 50}},
        {"id": "g2", "type": "gemini", "model": "gemini-3.1-pro-preview",
         "tokens": {"input": 0, "output": 0, "cached": 0, "thoughts": 0}},
        {"id": "g3", "type": "gemini",
         "tokens": {"input": 10, "output": 0, "cached": 30, "thoughts": 0}}
      ]
    }"#;

    #[test]
    fn parses_gemini_messages_with_thoughts_billed_as_output() {
        let dir = tmp_dir("basic");
        let path = dir.join("session-1.json");
        fs::write(&path, SESSION).unwrap();

        let parse = parse_gemini_file(&path, None).unwrap().unwrap();
        // g1 计入；g2 全零跳过；g3 有 token 计入（cached 钳制到 input）
        assert_eq!(parse.events.len(), 2);

        let first = &parse.events[0];
        assert_eq!(first.request_id, "gemini_session:sess-1:g1");
        assert_eq!(first.client_app, "gemini");
        assert_eq!(first.model_key, "gemini-3.1-pro-preview");
        assert_eq!(first.input_tokens, 1000);
        assert_eq!(first.output_tokens, 250, "thoughts 按输出计费");
        assert_eq!(first.cache_read_tokens, 400);
        assert_eq!(first.cache_creation_tokens, 0);
        assert_eq!(first.session_id.as_deref(), Some("sess-1"));
        assert_eq!(first.ts, parse_rfc3339_secs("2026-07-10T03:00:02Z"));

        let second = &parse.events[1];
        assert_eq!(second.model, "unknown");
        assert_eq!(second.cache_read_tokens, 10, "cached 钳制到 input");
        assert_eq!(parse.cursor.last_line_offset, 4);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unchanged_mtime_skips_and_corrupt_json_does_not_advance_cursor() {
        let dir = tmp_dir("incr");
        let path = dir.join("session-1.json");
        fs::write(&path, SESSION).unwrap();

        let first = parse_gemini_file(&path, None).unwrap().unwrap();
        assert!(
            parse_gemini_file(&path, Some(&first.cursor))
                .unwrap()
                .is_none()
        );

        // 写入中途的坏 JSON：不产出、cursor 停在原值（下轮重试）
        fs::write(&path, "{\"sessionId\": \"sess-1\", \"messages\": [").unwrap();
        let bumped = SyncCursor {
            last_modified_nanos: first.cursor.last_modified_nanos - 1,
            ..first.cursor
        };
        let retry = parse_gemini_file(&path, Some(&bumped)).unwrap().unwrap();
        assert!(retry.events.is_empty());
        assert_eq!(retry.cursor.last_modified_nanos, bumped.last_modified_nanos);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn collects_session_files_under_tmp_hash_chats() {
        let dir = tmp_dir("collect");
        let chats = dir.join("tmp").join("abc123hash").join("chats");
        fs::create_dir_all(&chats).unwrap();
        fs::write(chats.join("session-a.json"), "{}").unwrap();
        fs::write(chats.join("notes.txt"), "x").unwrap();

        let files = collect_gemini_files(&dir);
        assert_eq!(files.len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }
}
