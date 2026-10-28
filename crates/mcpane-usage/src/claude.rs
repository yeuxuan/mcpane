//! Claude Code 会话解析：`~/.claude/projects/**/*.jsonl` → `UsageEvent`。
//!
//! 数据形态：每条 assistant 消息一行 JSON，token 在 `message.usage`。
//! 同一 `message.id` 会因流式快照出现多行（message_start / 中间块 / 最终块），
//! 文件内按 id 去重后每消息只产出一条事件。

use crate::{
    FileParse, SyncCursor, UsageError, UsageEvent, file_stamp, normalize_model, parse_rfc3339_secs,
};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Claude 会话文件 + 其归属项目（projects 下一级目录名原样，不解码）。
#[derive(Debug, Clone)]
pub struct ClaudeSessionFile {
    pub path: PathBuf,
    pub project: String,
}

/// 收集 projects 目录下所有会话 jsonl。固定深度扫描，不递归，避免符号链接死循环：
///   projects/<项目>/*.jsonl                                    （主会话）
///   projects/<项目>/<SESSION_ID>/subagents/*.jsonl              （Task/Agent 子 agent）
///   projects/<项目>/<SESSION_ID>/subagents/workflows/<wf>/*.jsonl（Workflow 子 agent）
///
/// Workflow 层比普通子 agent 多嵌套一级，漏掉会让 Workflow 用量完全不计。
/// journal.jsonl 等无 assistant 行的文件会被解析天然跳过，无需按文件名过滤。
pub fn collect_claude_files(projects_dir: &Path) -> Vec<ClaudeSessionFile> {
    let mut files = Vec::new();
    let Ok(entries) = fs::read_dir(projects_dir) else {
        return files;
    };

    for entry in entries.flatten() {
        let project_dir = entry.path();
        if !project_dir.is_dir() {
            continue;
        }
        let project = entry.file_name().to_string_lossy().to_string();
        let Ok(sub_entries) = fs::read_dir(&project_dir) else {
            continue;
        };
        for sub_entry in sub_entries.flatten() {
            let sub_path = sub_entry.path();
            if sub_path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                files.push(ClaudeSessionFile {
                    path: sub_path,
                    project: project.clone(),
                });
            } else if sub_path.is_dir() {
                let subagents_dir = sub_path.join("subagents");
                if subagents_dir.is_dir() {
                    push_jsonl_children(&subagents_dir, &project, &mut files);
                    let workflows_dir = subagents_dir.join("workflows");
                    if let Ok(wf_entries) = fs::read_dir(&workflows_dir) {
                        for wf_entry in wf_entries.flatten() {
                            let wf_path = wf_entry.path();
                            if wf_path.is_dir() {
                                push_jsonl_children(&wf_path, &project, &mut files);
                            }
                        }
                    }
                }
            }
        }
    }

    files
}

fn push_jsonl_children(dir: &Path, project: &str, files: &mut Vec<ClaudeSessionFile>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                files.push(ClaudeSessionFile {
                    path,
                    project: project.to_string(),
                });
            }
        }
    }
}

/// 去重槽：同一 message.id 的多行快照里选出「最终态」代表行。
struct MessageSlot {
    event: UsageEvent,
    has_stop_reason: bool,
}

/// 解析单个 Claude 会话文件。
///
/// - mtime + size 未变 → 返回 `Ok(None)`，整文件跳过；
/// - 只增不改（size ≥ 上次记的字节数）→ 从上次字节位置接着读：活跃会话文件
///   十几 MB，每轮整读要秒级，接着读只有毫秒（对齐 cc-switch v3.20.1）。
///   会话内去重本就只看新行（旧行此前也只用来补 session_id），不需要全文件视野；
/// - 缩短/重写 → 从头读，行号 <= `prev.last_line_offset` 的行不再产出事件；
/// - 尾行没换行且解析不了（客户端正在写）→ 留到下轮，`last_size` 停在它前面；
/// - 逐行容错：坏 JSON / 空行 / 缺字段一律跳过。
pub fn parse_claude_file(
    file: &ClaudeSessionFile,
    prev: Option<&SyncCursor>,
) -> Result<Option<FileParse>, UsageError> {
    let stamp = file_stamp(&file.path);
    let mtime = stamp.modified_nanos;
    let prev = prev.copied().unwrap_or_default();
    // 未变判定（mtime+size，含 metadata 取不到时的 fail-safe）见 SyncCursor::is_unchanged。
    if prev.is_unchanged(stamp) {
        return Ok(None);
    }

    let mut handle = fs::File::open(&file.path)?;
    let incremental =
        prev.last_size > 0 && prev.last_line_offset > 0 && stamp.size >= prev.last_size;
    let (start_line, start_byte) = if incremental {
        (prev.last_line_offset, prev.last_size)
    } else {
        (0, 0)
    };
    if start_byte > 0 {
        handle.seek(SeekFrom::Start(start_byte as u64))?;
    }
    let mut reader = BufReader::new(handle);

    let mut line_offset: i64 = start_line;
    // 已消费到的字节位置：只推进到最后一个完整行的末尾。
    let mut consumed: i64 = start_byte;
    let mut slots: HashMap<String, MessageSlot> = HashMap::new();
    let mut session_id: Option<String> = None;
    let mut raw: Vec<u8> = Vec::new();

    loop {
        raw.clear();
        let n = reader.read_until(b'\n', &mut raw)?;
        if n == 0 {
            break;
        }
        let complete = raw.ends_with(b"\n");
        let text = String::from_utf8_lossy(&raw);
        let line = text.trim_end_matches(['\n', '\r']);
        let parsed = if line.trim().is_empty() {
            None
        } else {
            serde_json::from_str::<serde_json::Value>(line).ok()
        };
        // 尾巴没换行又解析不了：多半正在写，别把游标推过去，下轮再读这一段。
        if !complete && parsed.is_none() {
            break;
        }
        // 上轮把没换行的尾行按完整行收了，这轮开头读到的那个换行符是它的结尾，
        // 不是新的一行。
        if incremental && consumed == start_byte && line.is_empty() && complete {
            consumed += n as i64;
            continue;
        }
        consumed += n as i64;
        line_offset += 1;
        let Some(value) = parsed else { continue };

        if session_id.is_none()
            && let Some(sid) = value.get("sessionId").and_then(|v| v.as_str())
        {
            session_id = Some(sid.to_string());
        }

        // 已处理行只用于补 session_id，不再产出事件
        if line_offset <= prev.last_line_offset {
            continue;
        }

        if value.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let Some(message) = value.get("message") else {
            continue;
        };
        let Some(msg_id) = message.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(usage) = message.get("usage") else {
            continue;
        };

        let token =
            |key: &str| -> i64 { usage.get(key).and_then(|v| v.as_i64()).unwrap_or(0).max(0) };
        let model = message
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let has_stop_reason = message.get("stop_reason").is_some_and(|v| !v.is_null());
        let ts = value
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(parse_rfc3339_secs)
            .unwrap_or(0);

        let event = UsageEvent {
            request_id: format!("session:{msg_id}"),
            ts,
            client_app: "claude",
            model_key: normalize_model(&model),
            model,
            project: Some(file.project.clone()),
            session_id: session_id.clone(),
            input_tokens: token("input_tokens"),
            output_tokens: token("output_tokens"),
            cache_read_tokens: token("cache_read_input_tokens"),
            cache_creation_tokens: token("cache_creation_input_tokens"),
        };

        // 去重规则：新有 stop_reason 且旧无 → 替换（最终块优先于中间快照）；
        // 存在性相同 → 取 output_tokens 更大者；新无旧有 → 不替换。
        let replace = match slots.get(msg_id) {
            None => true,
            Some(existing) => {
                if has_stop_reason && !existing.has_stop_reason {
                    true
                } else if has_stop_reason == existing.has_stop_reason {
                    event.output_tokens > existing.event.output_tokens
                } else {
                    false
                }
            }
        };
        if replace {
            slots.insert(
                msg_id.to_string(),
                MessageSlot {
                    event,
                    has_stop_reason,
                },
            );
        }
    }

    let events = slots
        .into_values()
        .map(|slot| slot.event)
        .filter(UsageEvent::has_billable_tokens)
        .collect();

    Ok(Some(FileParse {
        events,
        cursor: SyncCursor {
            last_modified_nanos: mtime,
            last_size: consumed,
            last_line_offset: line_offset,
        },
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn write_session(dir: &Path, name: &str, lines: &[&str]) -> ClaudeSessionFile {
        let path = dir.join(name);
        fs::write(&path, lines.join("\n")).unwrap();
        ClaudeSessionFile {
            path,
            project: "proj-a".to_string(),
        }
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mcpane-usage-claude-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    const FINAL_LINE: &str = r#"{"type":"assistant","message":{"id":"msg_1","model":"claude-opus-4-6","usage":{"input_tokens":3,"output_tokens":1349,"cache_read_input_tokens":5000,"cache_creation_input_tokens":10000},"stop_reason":"end_turn"},"timestamp":"2026-04-05T12:00:00Z","sessionId":"session-abc"}"#;
    const SNAPSHOT_LINE: &str = r#"{"type":"assistant","message":{"id":"msg_1","model":"claude-opus-4-6","usage":{"input_tokens":3,"output_tokens":26,"cache_read_input_tokens":5000,"cache_creation_input_tokens":10000}},"timestamp":"2026-04-05T12:00:00Z","sessionId":"session-abc"}"#;

    #[test]
    fn parses_assistant_usage_line() {
        let dir = tmp_dir("basic");
        let file = write_session(&dir, "s.jsonl", &[FINAL_LINE]);

        let parse = parse_claude_file(&file, None).unwrap().unwrap();
        assert_eq!(parse.events.len(), 1);
        let ev = &parse.events[0];
        assert_eq!(ev.request_id, "session:msg_1");
        assert_eq!(ev.model_key, "claude-opus-4-6");
        assert_eq!(ev.input_tokens, 3);
        assert_eq!(ev.output_tokens, 1349);
        assert_eq!(ev.cache_read_tokens, 5000);
        assert_eq!(ev.cache_creation_tokens, 10000);
        assert_eq!(ev.session_id.as_deref(), Some("session-abc"));
        assert_eq!(ev.project.as_deref(), Some("proj-a"));
        assert_eq!(ev.ts, parse_rfc3339_secs("2026-04-05T12:00:00Z"));
        assert_eq!(parse.cursor.last_line_offset, 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dedups_by_message_id_prefers_stop_reason_then_larger_output() {
        let dir = tmp_dir("dedup");
        // 快照在后也不能覆盖最终块（新无 stop_reason、旧有 → 不替换）
        let file = write_session(&dir, "s.jsonl", &[SNAPSHOT_LINE, FINAL_LINE, SNAPSHOT_LINE]);

        let parse = parse_claude_file(&file, None).unwrap().unwrap();
        assert_eq!(parse.events.len(), 1);
        assert_eq!(parse.events[0].output_tokens, 1349);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn imports_billable_message_without_stop_reason_and_skips_zero() {
        let dir = tmp_dir("billable");
        let zero = r#"{"type":"assistant","message":{"id":"msg_zero","model":"claude-opus-4-8","usage":{"input_tokens":0,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}},"timestamp":"2026-06-07T13:01:24Z"}"#;
        let file = write_session(&dir, "s.jsonl", &[SNAPSHOT_LINE, zero]);

        let parse = parse_claude_file(&file, None).unwrap().unwrap();
        // 无 stop_reason 但有 cache 成本 → 必须计入；全 0 → 跳过
        assert_eq!(parse.events.len(), 1);
        assert_eq!(parse.events[0].request_id, "session:msg_1");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tolerates_malformed_lines() {
        let dir = tmp_dir("garbage");
        let file = write_session(
            &dir,
            "s.jsonl",
            &[
                "{broken json",
                "",
                r#"{"type":"user"}"#,
                FINAL_LINE,
                r#"{"type":"assistant"}"#,
            ],
        );

        let parse = parse_claude_file(&file, None).unwrap().unwrap();
        assert_eq!(parse.events.len(), 1);
        assert_eq!(parse.cursor.last_line_offset, 5);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn incremental_skips_unchanged_file_and_old_lines() {
        let dir = tmp_dir("incr");
        let file = write_session(&dir, "s.jsonl", &[FINAL_LINE]);

        let first = parse_claude_file(&file, None).unwrap().unwrap();
        assert_eq!(first.events.len(), 1);

        // mtime 未变 → 整文件跳过
        assert!(
            parse_claude_file(&file, Some(&first.cursor))
                .unwrap()
                .is_none()
        );

        // 追加一行新消息，mtime 前移 → 只产出新行
        let second_line = FINAL_LINE.replace("msg_1", "msg_2");
        fs::write(&file.path, format!("{FINAL_LINE}\n{second_line}")).unwrap();
        let bumped = SyncCursor {
            last_modified_nanos: first.cursor.last_modified_nanos - 1,
            ..first.cursor
        };
        let second = parse_claude_file(&file, Some(&bumped)).unwrap().unwrap();
        assert_eq!(second.events.len(), 1);
        assert_eq!(second.events[0].request_id, "session:msg_2");
        assert_eq!(second.cursor.last_line_offset, 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn incremental_reads_only_the_appended_tail() {
        let dir = tmp_dir("tail");
        let file = write_session(&dir, "s.jsonl", &[FINAL_LINE, ""]);
        let first = parse_claude_file(&file, None).unwrap().unwrap();
        assert_eq!(first.cursor.last_size, (FINAL_LINE.len() + 1) as i64);

        // 把已扫过的前缀换成「垃圾 + 一条新 assistant 行」、总字节数不变：
        // 若真是从头读，按行号只跳过第 1 行，第 2 行的 msg_9 会被多算出来；
        // 接着读则跳过整个前缀，只看见追加的 msg_2。
        let planted = SNAPSHOT_LINE.replace("msg_1", "msg_9");
        let garbage = "x".repeat(FINAL_LINE.len() - planted.len() - 1);
        let second_line = FINAL_LINE.replace("msg_1", "msg_2");
        fs::write(&file.path, format!("{garbage}\n{planted}\n{second_line}\n")).unwrap();
        assert_eq!(
            (garbage.len() + 1 + planted.len() + 1) as i64,
            first.cursor.last_size
        );
        let bumped = SyncCursor {
            last_modified_nanos: first.cursor.last_modified_nanos - 1,
            ..first.cursor
        };
        let second = parse_claude_file(&file, Some(&bumped)).unwrap().unwrap();
        assert_eq!(second.events.len(), 1);
        assert_eq!(second.events[0].request_id, "session:msg_2");
        assert_eq!(second.events[0].session_id.as_deref(), Some("session-abc"));
        assert_eq!(second.cursor.last_line_offset, 2);
        assert_eq!(
            second.cursor.last_size,
            first.cursor.last_size + second_line.len() as i64 + 1
        );

        // 文件被缩短（重写）→ 退回从头读，旧行号之后的行才产出。
        fs::write(&file.path, format!("{FINAL_LINE}\n")).unwrap();
        let shrunk = SyncCursor {
            last_modified_nanos: second.cursor.last_modified_nanos - 1,
            ..second.cursor
        };
        let third = parse_claude_file(&file, Some(&shrunk)).unwrap().unwrap();
        assert!(third.events.is_empty());
        assert_eq!(third.cursor.last_line_offset, 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unterminated_unparsable_tail_is_held_back_until_complete() {
        let dir = tmp_dir("partial");
        // 第二行正在写：只有半截 JSON、没有换行。
        let half = &SNAPSHOT_LINE[..SNAPSHOT_LINE.len() / 2];
        let file = write_session(&dir, "s.jsonl", &[FINAL_LINE, half]);
        let first = parse_claude_file(&file, None).unwrap().unwrap();
        assert_eq!(first.events.len(), 1);
        // 游标停在半截行之前：行号 1、字节数只含第一行 + 换行。
        assert_eq!(first.cursor.last_line_offset, 1);
        assert_eq!(first.cursor.last_size, (FINAL_LINE.len() + 1) as i64);

        // 半截行写完（mtime 不变也无妨——size 变了就得重扫）→ 补出 msg_2，且只多一行。
        let second_line = FINAL_LINE.replace("msg_1", "msg_2");
        fs::write(&file.path, format!("{FINAL_LINE}\n{second_line}")).unwrap();
        let same_mtime = SyncCursor {
            last_modified_nanos: crate::file_mtime_nanos(&file.path),
            ..first.cursor
        };
        let second = parse_claude_file(&file, Some(&same_mtime))
            .unwrap()
            .unwrap();
        assert_eq!(second.events.len(), 1);
        assert_eq!(second.events[0].request_id, "session:msg_2");
        assert_eq!(second.cursor.last_line_offset, 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn collects_three_fixed_depths() {
        let dir = tmp_dir("collect");
        let project = dir.join("-Users-me-proj");
        let subagents = project.join("sess-1").join("subagents");
        let wf = subagents.join("workflows").join("wf_1");
        fs::create_dir_all(&wf).unwrap();
        fs::write(project.join("main.jsonl"), "{}").unwrap();
        fs::write(subagents.join("agent-a.jsonl"), "{}").unwrap();
        fs::write(wf.join("agent-wf.jsonl"), "{}").unwrap();
        // 深度之外的文件不收集
        let deeper = wf.join("nested");
        fs::create_dir_all(&deeper).unwrap();
        fs::write(deeper.join("too-deep.jsonl"), "{}").unwrap();

        let files = collect_claude_files(&dir);
        let names: Vec<String> = files
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(files.len(), 3);
        assert!(names.contains(&"main.jsonl".to_string()));
        assert!(names.contains(&"agent-a.jsonl".to_string()));
        assert!(names.contains(&"agent-wf.jsonl".to_string()));
        assert!(files.iter().all(|f| f.project == "-Users-me-proj"));

        let _ = fs::remove_dir_all(&dir);
    }
}
