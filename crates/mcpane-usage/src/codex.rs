//! Codex 会话解析：`~/.codex/sessions/**/rollout-*.jsonl` → `UsageEvent`。
//!
//! 数据形态：`event_msg`（payload.type=="token_count"）携带精确单次增量
//! `info.last_token_usage` 与会话累计 `info.total_token_usage`。计费优先取
//! last（精确值）；只有累计值时按 **high-water mark** 做 delta——Codex 会在
//! 多个 rate-limit lane（`payload.rate_limits.limit_id`）下交错上报累计快照，
//! 相邻相减会把同一请求算多遍。限流刷新还会
//! 换个 limit_id 重发相同快照：同源重复按该源最新快照识别，跨源重复只与
//! 紧邻前一个事件比（更早的旧签名在计数器重置后可能合法重现）。
//!
//! fork / 子代理处理：这类会话文件开头会逐字重放父线程的 token_count 历史，
//! 再接上自己的调用。重放段属于父线程已计入的用量，只用于恢复累计基线、不计费。
//!
//! 识别三信号（首个 session_meta.payload）：`forked_from_id` 非空 /
//! `source.subagent` 存在 / `session_id`≠线程 id。命中后**按父 rollout 的真实
//! 用量数值比对**出重放段有多长（见 [`replayed_prefix_len`]），而不是去找什么
//! 标志事件。
//!
//! 早先的做法是扫 `thread_settings_applied` / `inter_agent_communication*`
//! 特征事件当接管边界，后来整体放弃：这套启发式的致命缺陷是**标志事件不出现
//! 时判定为"无边界"，于是整份重放历史被当成全新用量计费**。抽样 223 个真实
//! rollout 里有 16 个（7%）命中该条件，最长的一条四层级联 fork 把根线程
//! 862 万 input token 重复计了 3~4 倍。`request_id` 去重挡不住这个场景——
//! 父子线程的 thread_id 天生不同，去重键永远不会撞。
//!
//! 拿不到父指针（老格式只有 `session_id`≠线程 id 这一个信号）时才退回特征
//! 事件法；确认是 fork 却找不到父 rollout 时整份跳过——宁可少计也不多计。

use crate::{
    FileParse, SyncCursor, UsageError, UsageEvent, file_stamp, normalize_model, parse_rfc3339_secs,
};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// 累计 token 用量（total_token_usage 快照）。
#[derive(Debug, Clone, Copy, Default)]
struct CumulativeTokens {
    input: i64,
    cached_input: i64,
    output: i64,
}

/// fork / 子代理会话的身份标记（源于首个 session_meta.payload）。
struct CodexSessionIdentity {
    thread_id: Option<String>,
    /// 父线程 id（`forked_from_id`，或 `source` 为对象时的 subagent 父指针）。
    /// 有它才能定位父 rollout、按真实用量数值比对出重放段。
    parent_thread_id: Option<String>,
    /// 该文件是否会先重放父线程历史（fork / 子代理）。
    carries_history_snapshot: bool,
}

/// 单个 `token_count` 事件的用量签名：累计值与增量值一并纳入。
///
/// 只比累计值不够——父线程在同一累计点上可能有多个事件（增量为 0 的
/// task 边界重复上报），单看累计值会把它们看成同一个，前缀对齐就会滑位。
#[derive(Debug, Clone, PartialEq, Eq)]
struct TokenSignature {
    total: Option<(i64, i64, i64)>,
    last: Option<(i64, i64, i64)>,
}

fn token_signature(info: &serde_json::Value) -> TokenSignature {
    let pick = |key: &str| {
        info.get(key)
            .and_then(parse_cumulative)
            .map(|c| (c.input, c.cached_input, c.output))
    };
    TokenSignature {
        total: pick("total_token_usage"),
        last: pick("last_token_usage"),
    }
}

/// 快照来源：`payload.rate_limits.limit_id`（哪条限流 lane 上报的）。
/// None = 无标注（旧格式），作为一个独立"源"参与同源去重。
fn token_snapshot_source(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("rate_limits")
        .and_then(|limits| limits.get("limit_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// 顺序扫出文件里全部 `token_count` 事件的签名。
///
/// 过滤条件必须与 [`parse_codex_file`] 主循环逐字一致，否则父子两边的序号
/// 会错位，前缀对齐随之失真。
fn token_signatures(path: &Path) -> Vec<TokenSignature> {
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        // 与主循环同一套字符串预过滤：先按字面量筛掉绝大多数无关行，再反
        // 序列化。少了这层，每个 fork 文件都要把父子两边所有行都解析一遍
        // （最大的会话有近两千个事件）。
        if !line.contains("\"event_msg\"") || !line.contains("\"token_count\"") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(|v| v.as_str()) != Some("event_msg") {
            continue;
        }
        let Some(payload) = value.get("payload") else {
            continue;
        };
        if payload.get("type").and_then(|t| t.as_str()) != Some("token_count") {
            continue;
        }
        let Some(info) = payload.get("info").filter(|i| !i.is_null()) else {
            continue;
        };
        out.push(token_signature(info));
    }
    out
}

/// 子文件开头有多少个 token 事件只是父线程历史的重放。
///
/// 逐个在父签名的**剩余**部分里找匹配（子序列对齐，不是严格逐一对应）：
/// 父文件里可能存在子文件没有重放的事件，但重放段的相对顺序一定保持。
/// 第一个在父线程里找不到落点的事件，就是子线程自己的第一次真实调用。
///
/// 这取代了原先"扫 `thread_settings_applied` / `inter_agent_communication`
/// 特征事件找边界"的启发式——那种做法在特征事件不出现时会判定"无边界"，
/// 于是整份重放历史被当作全新用量计费（实测本机 223 个 rollout 中 16 个
/// 命中该条件，最长的一条 fork 链把 862 万 token 重复计了 3~4 倍）。
fn replayed_prefix_len(child: &[TokenSignature], parent: &[TokenSignature]) -> usize {
    let mut parent_offset = 0usize;
    let mut matched = 0usize;
    for signature in child {
        let Some(hit) = parent[parent_offset..]
            .iter()
            .position(|candidate| candidate == signature)
        else {
            break;
        };
        parent_offset += hit + 1;
        matched += 1;
    }
    matched
}

/// 从 rollout 文件名尾部取线程 UUID。
///
/// 文件名形如 `rollout-2026-06-26T15-53-24-019f02eb-53a2-7093-be4d-7f07bde8c068.jsonl`，
/// 尾部正是 `session_meta.payload.id`（已用本机真实数据核对）。靠文件名建
/// 索引可以零 IO——否则定位父文件要把每个 rollout 都打开读一遍首行。
fn thread_id_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    // UUID 是尾部 5 段（8-4-4-4-12），连字符分隔；前面还有 rollout- 与时间戳。
    let parts: Vec<&str> = stem.split('-').collect();
    if parts.len() < 5 {
        return None;
    }
    let uuid = parts[parts.len() - 5..].join("-");
    let ok = uuid.len() == 36 && uuid.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    ok.then_some(uuid)
}

/// thread_id → rollout 路径。fork 文件靠它找到父线程的用量序列。
pub type RolloutIndex = std::collections::BTreeMap<String, PathBuf>;

/// 按文件名建线程索引（零 IO，不读文件内容）。
pub fn build_rollout_index(files: &[PathBuf]) -> RolloutIndex {
    files
        .iter()
        .filter_map(|path| thread_id_from_filename(path).map(|id| (id, path.clone())))
        .collect()
}

/// 从 session_meta.payload 解析线程 id 与"是否携带历史重放"标记。
/// thread_id 取值链与主循环一致：id → thread_id → threadId → session_id → sessionId。
fn parse_session_identity(payload: &serde_json::Value) -> CodexSessionIdentity {
    let thread_id = payload
        .get("id")
        .or_else(|| payload.get("thread_id"))
        .or_else(|| payload.get("threadId"))
        .or_else(|| payload.get("session_id"))
        .or_else(|| payload.get("sessionId"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let session_id = payload
        .get("session_id")
        .or_else(|| payload.get("sessionId"))
        .and_then(|v| v.as_str());
    let forked_from = payload
        .get("forked_from_id")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty());
    // `source` 在实测数据里是字符串（如 "vscode"），只有子代理形态才可能是
    // 对象；`get` 对字符串返回 None，两种形态都能安全处理。
    let subagent = payload
        .get("source")
        .and_then(|source| source.get("subagent"));
    let parent_thread_id = forked_from.map(|s| s.to_string()).or_else(|| {
        subagent
            .and_then(|s| {
                s.get("parent_thread_id")
                    .or_else(|| s.get("parentThreadId"))
                    .or_else(|| s.get("thread_id"))
            })
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(|s| s.to_string())
    });
    let carries_history_snapshot = forked_from.is_some()
        || subagent.is_some()
        || session_id.is_some_and(|sid| Some(sid) != thread_id.as_deref());
    CodexSessionIdentity {
        thread_id,
        parent_thread_id,
        carries_history_snapshot,
    }
}

/// 预扫首个 session_meta 取身份标记（fork/子代理判定需在主循环前完成）。
fn read_session_identity(path: &Path) -> Option<CodexSessionIdentity> {
    let reader = BufReader::new(fs::File::open(path).ok()?);
    for line_result in reader.lines() {
        let Ok(line) = line_result else { continue };
        if !line.contains("\"session_meta\"") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(|v| v.as_str()) != Some("session_meta") {
            continue;
        }
        if let Some(payload) = value.get("payload") {
            return Some(parse_session_identity(payload));
        }
    }
    None
}

/// fork/子代理日志会先重放父线程历史，再以接管事件开始当前线程。
/// 返回接管事件所在行号（1-based）；此前的 token_count 只用于恢复累计基线。
/// 非重放会话（无历史快照信号）恒返回 None，普通会话零影响。
fn history_replay_boundary(path: &Path, identity: Option<&CodexSessionIdentity>) -> Option<i64> {
    if !identity.is_some_and(|id| id.carries_history_snapshot) {
        return None;
    }
    let reader = BufReader::new(fs::File::open(path).ok()?);
    for (index, line_result) in reader.lines().enumerate() {
        let Ok(line) = line_result else { continue };
        if !line.contains("\"thread_settings_applied\"")
            && !line.contains("\"inter_agent_communication")
        {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(event_type) = value.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        let is_boundary = event_type.starts_with("inter_agent_communication")
            || (event_type == "event_msg"
                && value
                    .get("payload")
                    .and_then(|p| p.get("type"))
                    .and_then(|v| v.as_str())
                    == Some("thread_settings_applied"));
        if is_boundary {
            return Some(index as i64 + 1);
        }
    }
    None
}

/// 本文件开头有多少用量属于父线程、不该重复计费。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplayPlan {
    /// 非 fork/子代理，或确认没有重放段：全部计费。
    None,
    /// 前 n 个 `token_count` 事件是父线程历史的重放，跳过不计费。
    SkipPrefix(usize),
    /// 只有老信号（`session_id != thread_id`）、拿不到父指针：退回按特征
    /// 事件找边界。仍是启发式，但比"整份计费"稳。
    LegacyBoundary(i64),
    /// 确认是 fork，但父 rollout 不在扫描范围内，无从判定重放段长度。
    SkipFile,
}

/// 定夺本文件的重放范围。
fn replay_plan(
    path: &Path,
    identity: Option<&CodexSessionIdentity>,
    index: &RolloutIndex,
) -> ReplayPlan {
    let Some(identity) = identity else {
        return ReplayPlan::None;
    };
    if !identity.carries_history_snapshot {
        return ReplayPlan::None;
    }
    let Some(parent_id) = identity.parent_thread_id.as_deref() else {
        // 拿不到父指针，只能退回特征事件启发式（老格式/异常数据）。
        return match history_replay_boundary(path, Some(identity)) {
            Some(boundary) => ReplayPlan::LegacyBoundary(boundary),
            None => ReplayPlan::None,
        };
    };
    let Some(parent_path) = index.get(parent_id) else {
        return ReplayPlan::SkipFile;
    };
    let parent = token_signatures(parent_path);
    let child = token_signatures(path);
    match replayed_prefix_len(&child, &parent) {
        0 => ReplayPlan::None,
        matched => ReplayPlan::SkipPrefix(matched),
    }
}

/// 收集 Codex 会话文件：
/// - `<codex>/sessions/YYYY/MM/DD/rollout-*.jsonl`（日期分区，限深 3）；
/// - `<codex>/archived_sessions/*.jsonl`（扁平归档）。
///
/// 会话从 sessions/ 移入 archived_sessions/ 后路径变化、游标从零重扫，
/// 但 request_id 由 thread_id + 事件序号构成、与路径无关，
/// INSERT OR IGNORE 会拦下重复，不会双算。
pub fn collect_codex_files(codex_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();

    let sessions_dir = codex_dir.join("sessions");
    if sessions_dir.is_dir() {
        collect_jsonl_recursive(&sessions_dir, &mut files, 0, 3);
    }

    let archived_dir = codex_dir.join("archived_sessions");
    if let Ok(entries) = fs::read_dir(&archived_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                files.push(path);
            }
        }
    }

    files
}

fn collect_jsonl_recursive(dir: &Path, files: &mut Vec<PathBuf>, depth: u32, max_depth: u32) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && depth < max_depth {
            collect_jsonl_recursive(&path, files, depth + 1, max_depth);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
}

/// 解析单个 Codex 会话文件。
///
/// - mtime（纳秒）未变 → `Ok(None)`，整文件跳过；
/// - 变了 → **必须从头重扫**：total_token_usage 是累计值，prev_total 与
///   event_index 只能靠回放全部事件重建；行号 <= `prev.last_line_offset`
///   的事件只更新状态、不产出，序号对所有非零事件递增以保证 request_id 稳定。
pub fn parse_codex_file(
    path: &Path,
    prev: Option<&SyncCursor>,
    index: &RolloutIndex,
) -> Result<Option<FileParse>, UsageError> {
    let stamp = file_stamp(path);
    let mtime = stamp.modified_nanos;
    let prev = prev.copied().unwrap_or_default();
    // 未变判定（mtime+size，含 metadata 取不到时的 fail-safe）见 SyncCursor::is_unchanged。
    if prev.is_unchanged(stamp) {
        return Ok(None);
    }

    // fork/子代理判定：重放段先于任何标志出现，单遍扫描到 token_count 时还
    // 看不到后面的信息，故必须在主循环前定好"前多少个 token 事件不计费"。
    let identity = read_session_identity(path);
    let replay = replay_plan(path, identity.as_ref(), index);
    if matches!(replay, ReplayPlan::SkipFile) {
        // 确认是 fork 但父 rollout 不在扫描范围（被删/被归档到别处）：无从
        // 判定重放段有多长。计入 = 把父线程已计过的用量再算一遍（实测可达
        // 3~4 倍）；跳过 = 至多少计该子线程自己的增量。用量是成本监控口径，
        // 多计会误导决策，少计相对温和，故取跳过。
        //
        // 游标**原样返回、一格不进**（含 mtime）：父 rollout 日后若被恢复
        // 回扫描范围，这个文件必须能重新参与计算。若在此推进 mtime，下一轮
        // 的 `mtime <= prev` 判定就会把它当成"已处理且无变化"永久跳过。
        return Ok(Some(FileParse {
            events: Vec::new(),
            cursor: prev,
        }));
    }
    let replayed_token_events = match replay {
        ReplayPlan::SkipPrefix(n) => n,
        _ => 0,
    };
    let replay_boundary = match replay {
        ReplayPlan::LegacyBoundary(boundary) => Some(boundary),
        _ => None,
    };
    // 第几个 token_count 事件（与 token_signatures 的过滤规则严格一致）。
    let mut token_event_seq = 0usize;

    let reader = BufReader::new(fs::File::open(path)?);

    // 预扫已取到线程 id 时直接沿用；主循环的 session_meta 臂仅在此为 None 时兜底。
    let mut thread_id: Option<String> = identity.and_then(|id| id.thread_id);
    let mut current_model = "unknown".to_string();
    // 累计基线取各分量的历史最大值（high-water）：多 lane 交错时累计值会
    // 回退/跳跃，单一"上一快照"会把回退后的再涨部分重复计费。
    let mut total_high_water: Option<CumulativeTokens> = None;
    // 同源重复识别：各 limit_id 的最新完整快照；跨源重复只看紧邻前一事件。
    let mut last_signature_by_source: std::collections::HashMap<Option<String>, TokenSignature> =
        std::collections::HashMap::new();
    let mut previous_token_signature: Option<TokenSignature> = None;
    let mut event_index: i64 = 0;
    let mut line_offset: i64 = 0;
    let mut events = Vec::new();

    for line_result in reader.lines() {
        line_offset += 1;

        // 容忍不完整的最后一行（客户端可能正在写入）
        let Ok(line) = line_result else { continue };
        if line.trim().is_empty() {
            continue;
        }

        // JSON 反序列化前的快速过滤：会话文件绝大多数行是无关事件
        let is_event_msg = line.contains("\"event_msg\"");
        let is_turn_context = line.contains("\"turn_context\"");
        let is_session_meta = line.contains("\"session_meta\"");
        if !is_event_msg && !is_turn_context && !is_session_meta {
            continue;
        }
        if is_event_msg && !line.contains("\"token_count\"") {
            continue;
        }

        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(event_type) = value.get("type").and_then(|t| t.as_str()) else {
            continue;
        };

        match event_type {
            "session_meta" if thread_id.is_none() => {
                // 子代理日志的 payload.id 是当前线程唯一 ID，session_id 指向父线程
                thread_id = value
                    .get("payload")
                    .and_then(|p| {
                        p.get("id")
                            .or_else(|| p.get("thread_id"))
                            .or_else(|| p.get("threadId"))
                            .or_else(|| p.get("session_id"))
                            .or_else(|| p.get("sessionId"))
                    })
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
            }
            "turn_context" => {
                if let Some(model) = value.get("payload").and_then(|p| {
                    p.get("model")
                        .or_else(|| p.get("info").and_then(|info| info.get("model")))
                        .and_then(|v| v.as_str())
                }) {
                    current_model = model.to_string();
                }
            }
            "event_msg" => {
                let Some(payload) = value.get("payload") else {
                    continue;
                };
                if payload.get("type").and_then(|t| t.as_str()) != Some("token_count") {
                    continue;
                }
                // 首个 token_count 事件的 info 可能为 null
                let info = match payload.get("info") {
                    Some(i) if !i.is_null() => i,
                    _ => continue,
                };
                // 与 token_signatures 的过滤规则严格对齐（同样在 info 非 null
                // 后计数），父子两侧的第 n 个 token 事件才指的是同一个。
                token_event_seq += 1;

                if let Some(model) = info
                    .get("model")
                    .or_else(|| info.get("model_name"))
                    .or_else(|| payload.get("model"))
                    .and_then(|v| v.as_str())
                {
                    current_model = model.to_string();
                }

                let signature = token_signature(info);
                let to_tokens = |t: (i64, i64, i64)| CumulativeTokens {
                    input: t.0,
                    cached_input: t.1,
                    output: t.2,
                };
                let total = signature.total.map(to_tokens);
                let last = signature.last.map(to_tokens);
                if total.is_none() && last.is_none() {
                    continue;
                }

                // 限流刷新会换 limit_id 重发相同快照：同源比该源最新快照，
                // 跨源只比紧邻前一事件（更早的旧签名在计数器重置后可合法重现）。
                let snapshot_source = token_snapshot_source(payload);
                let duplicate_snapshot = total.is_some()
                    && (last_signature_by_source.get(&snapshot_source) == Some(&signature)
                        || previous_token_signature.as_ref() == Some(&signature));
                if total.is_some() {
                    last_signature_by_source.insert(snapshot_source, signature.clone());
                }
                previous_token_signature = Some(signature);

                // 计费优先级：重复快照 → 0；精确增量 last → 直用；
                // 仅累计值 → 对 high-water 基线做 delta。
                let (input, cached, output) = if duplicate_snapshot {
                    (0, 0, 0)
                } else if let Some(last) = last {
                    (last.input, last.cached_input, last.output)
                } else if let Some(total) = total.as_ref() {
                    delta(total_high_water.as_ref(), total)
                } else {
                    continue;
                };
                // 基线恒推进（含重复快照与重放段）：它是"见过的最大累计值"。
                if let Some(total) = total {
                    match total_high_water.as_mut() {
                        Some(high_water) => update_high_water(high_water, &total),
                        None => total_high_water = Some(total),
                    }
                }

                // 钳制：cached 不应超过 input（防护异常数据）
                let cached = cached.min(input);

                // task 边界会重复上报相同累计值，delta 全零无计费意义
                if input == 0 && cached == 0 && output == 0 {
                    continue;
                }

                // 所有非零事件占据稳定序号（含已同步的行），request_id 才可重放
                event_index += 1;

                // fork/子代理重放段：prev_total 已在上方更新（恢复累计基线），
                // 但这些是父线程已计入的用量，不再计费。序号已占，request_id 稳定。
                //
                // 首选按父 rollout 比对出的事件数跳过（精确、不依赖任何标志
                // 事件是否出现）；拿不到父指针时才退回老的特征事件行号边界。
                if token_event_seq <= replayed_token_events {
                    continue;
                }
                if replay_boundary.is_some_and(|boundary| line_offset < boundary) {
                    continue;
                }

                if line_offset <= prev.last_line_offset {
                    continue;
                }

                let tid = thread_id.as_deref().unwrap_or("unknown");
                events.push(UsageEvent {
                    request_id: format!("codex_session:thread-v2:{tid}:{event_index}"),
                    ts: value
                        .get("timestamp")
                        .and_then(|v| v.as_str())
                        .map(parse_rfc3339_secs)
                        .unwrap_or(0),
                    client_app: "codex",
                    model_key: normalize_model(&current_model),
                    model: current_model.clone(),
                    project: None,
                    session_id: thread_id.clone(),
                    input_tokens: input,
                    output_tokens: output,
                    cache_read_tokens: cached,
                    cache_creation_tokens: 0,
                });
            }
            _ => {}
        }
    }

    Ok(Some(FileParse {
        events,
        cursor: SyncCursor {
            last_modified_nanos: mtime,
            last_size: stamp.size,
            last_line_offset: line_offset,
        },
    }))
}

fn parse_cumulative(total_usage: &serde_json::Value) -> Option<CumulativeTokens> {
    if !total_usage.is_object() {
        return None;
    }
    let field = |keys: &[&str]| -> i64 {
        keys.iter()
            .find_map(|k| total_usage.get(k).and_then(|v| v.as_i64()))
            .unwrap_or(0)
            .max(0)
    };
    Some(CumulativeTokens {
        input: field(&["input_tokens"]),
        // 部分版本字段名为 cache_read_input_tokens
        cached_input: field(&["cached_input_tokens", "cache_read_input_tokens"]),
        output: field(&["output_tokens"]),
    })
}

/// 当前累计值相对基线（high-water）之差；无基线时首个快照即 delta。
/// `.max(0)` 防护累计值回退（回退段不计费，基线由 high-water 保持不降）。
fn delta(prev: Option<&CumulativeTokens>, current: &CumulativeTokens) -> (i64, i64, i64) {
    match prev {
        None => (current.input, current.cached_input, current.output),
        Some(p) => (
            (current.input - p.input).max(0),
            (current.cached_input - p.cached_input).max(0),
            (current.output - p.output).max(0),
        ),
    }
}

/// 基线取各分量历史最大值：多 lane 的累计值各自独立推进，取 max 才不会在
/// lane 切换（数值回退→再涨）时把同一段用量二次计费。
fn update_high_water(high_water: &mut CumulativeTokens, current: &CumulativeTokens) {
    high_water.input = high_water.input.max(current.input);
    high_water.cached_input = high_water.cached_input.max(current.cached_input);
    high_water.output = high_water.output.max(current.output);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::file_mtime_nanos;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mcpane-usage-codex-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn session_meta(thread_id: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-07-10T03:00:00Z","type":"session_meta","payload":{{"id":"{thread_id}"}}}}"#
        )
    }

    fn turn_context(model: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-07-10T03:00:01Z","type":"turn_context","payload":{{"model":"{model}"}}}}"#
        )
    }

    fn token_count(input: i64, cached: i64, output: i64) -> String {
        format!(
            r#"{{"timestamp":"2026-07-10T03:00:02Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{input},"cached_input_tokens":{cached},"output_tokens":{output}}}}}}}}}"#
        )
    }

    /// 带 rate-limit lane 标注的累计快照（可选附带精确增量 last）。
    fn token_count_lane(
        lane: &str,
        total: (i64, i64, i64),
        last: Option<(i64, i64, i64)>,
    ) -> String {
        let last_json = last
            .map(|(i, c, o)| {
                format!(
                    r#","last_token_usage":{{"input_tokens":{i},"cached_input_tokens":{c},"output_tokens":{o}}}"#
                )
            })
            .unwrap_or_default();
        let (i, c, o) = total;
        format!(
            r#"{{"timestamp":"2026-07-10T03:00:02Z","type":"event_msg","payload":{{"type":"token_count","rate_limits":{{"limit_id":"{lane}"}},"info":{{"total_token_usage":{{"input_tokens":{i},"cached_input_tokens":{c},"output_tokens":{o}}}{last_json}}}}}}}"#
        )
    }

    fn forked_session_meta(thread_id: &str, forked_from: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-07-10T03:00:00Z","type":"session_meta","payload":{{"id":"{thread_id}","forked_from_id":"{forked_from}"}}}}"#
        )
    }

    fn thread_settings_applied() -> String {
        r#"{"timestamp":"2026-07-10T03:00:05Z","type":"event_msg","payload":{"type":"thread_settings_applied"}}"#.to_string()
    }

    fn subagent_session_meta(thread_id: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-07-10T03:00:00Z","type":"session_meta","payload":{{"id":"{thread_id}","source":{{"subagent":{{"parent":"p"}}}}}}}}"#
        )
    }

    fn inter_agent_communication() -> String {
        r#"{"timestamp":"2026-07-10T03:00:05Z","type":"inter_agent_communication_request","payload":{}}"#.to_string()
    }

    fn write_file(dir: &Path, name: &str, lines: &[String]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, lines.join("\n")).unwrap();
        path
    }

    #[test]
    fn computes_delta_between_cumulative_snapshots() {
        let dir = tmp_dir("delta");
        let path = write_file(
            &dir,
            "rollout-a.jsonl",
            &[
                session_meta("thread-a"),
                turn_context("gpt-5.6"),
                token_count(17934, 9600, 454),
                token_count(36722, 27904, 804),
            ],
        );

        let parse = parse_codex_file(&path, None, &RolloutIndex::new())
            .unwrap()
            .unwrap();
        assert_eq!(parse.events.len(), 2);

        let first = &parse.events[0];
        assert_eq!(first.request_id, "codex_session:thread-v2:thread-a:1");
        assert_eq!(first.input_tokens, 17934);
        assert_eq!(first.cache_read_tokens, 9600);
        assert_eq!(first.output_tokens, 454);
        assert_eq!(first.model, "gpt-5.6");
        assert_eq!(first.model_key, "gpt-5.6");
        assert_eq!(first.session_id.as_deref(), Some("thread-a"));
        assert_eq!(first.cache_creation_tokens, 0);

        let second = &parse.events[1];
        assert_eq!(second.request_id, "codex_session:thread-v2:thread-a:2");
        assert_eq!(second.input_tokens, 36722 - 17934);
        assert_eq!(second.cache_read_tokens, 27904 - 9600);
        assert_eq!(second.output_tokens, 804 - 454);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_zero_delta_and_null_info() {
        let dir = tmp_dir("zero");
        let null_info = r#"{"timestamp":"2026-07-10T03:00:02Z","type":"event_msg","payload":{"type":"token_count","info":null}}"#.to_string();
        let path = write_file(
            &dir,
            "rollout-a.jsonl",
            &[
                session_meta("t"),
                null_info,
                token_count(100, 50, 10),
                // task 边界：累计值原地重复 → delta 全零，跳过且不占序号
                token_count(100, 50, 10),
                token_count(200, 60, 30),
            ],
        );

        let parse = parse_codex_file(&path, None, &RolloutIndex::new())
            .unwrap()
            .unwrap();
        assert_eq!(parse.events.len(), 2);
        assert_eq!(parse.events[1].request_id, "codex_session:thread-v2:t:2");
        assert_eq!(parse.events[1].input_tokens, 100);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn falls_back_to_last_token_usage() {
        let dir = tmp_dir("fallback");
        let last = r#"{"timestamp":"2026-07-10T03:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":1000,"cache_read_input_tokens":400,"output_tokens":200}}}}"#.to_string();
        let path = write_file(&dir, "rollout-a.jsonl", &[session_meta("t"), last]);

        let parse = parse_codex_file(&path, None, &RolloutIndex::new())
            .unwrap()
            .unwrap();
        assert_eq!(parse.events.len(), 1);
        let ev = &parse.events[0];
        assert_eq!(ev.input_tokens, 1000);
        // 备用字段名 cache_read_input_tokens 也要认
        assert_eq!(ev.cache_read_tokens, 400);
        assert_eq!(ev.output_tokens, 200);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clamps_cached_to_input() {
        let dir = tmp_dir("clamp");
        let path = write_file(
            &dir,
            "rollout-a.jsonl",
            &[
                session_meta("t"),
                token_count(100, 0, 50),
                // input delta=10，cached delta=80（异常）→ 钳制到 10
                token_count(110, 80, 60),
            ],
        );

        let parse = parse_codex_file(&path, None, &RolloutIndex::new())
            .unwrap()
            .unwrap();
        assert_eq!(parse.events.len(), 2);
        assert_eq!(parse.events[1].input_tokens, 10);
        assert_eq!(parse.events[1].cache_read_tokens, 10);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn incremental_rescan_rebuilds_state_and_keeps_indexes_stable() {
        let dir = tmp_dir("incr");
        let base = vec![
            session_meta("t"),
            turn_context("gpt-5.6"),
            token_count(100, 50, 10),
            token_count(200, 100, 20),
        ];
        let path = write_file(&dir, "rollout-a.jsonl", &base);

        let first = parse_codex_file(&path, None, &RolloutIndex::new())
            .unwrap()
            .unwrap();
        assert_eq!(first.events.len(), 2);
        assert_eq!(first.cursor.last_line_offset, 4);

        // mtime 未变 → 整文件跳过
        assert!(
            parse_codex_file(&path, Some(&first.cursor), &RolloutIndex::new())
                .unwrap()
                .is_none()
        );

        // 追加一个事件：重扫必须重建 prev_total（delta 相对第 2 个快照）
        // 且 event_index 稳定（新事件序号为 3），旧事件不重复产出
        let mut lines = base;
        lines.push(token_count(350, 150, 45));
        write_file(&dir, "rollout-a.jsonl", &lines);
        let bumped = SyncCursor {
            last_modified_nanos: first.cursor.last_modified_nanos - 1,
            ..first.cursor
        };
        let second = parse_codex_file(&path, Some(&bumped), &RolloutIndex::new())
            .unwrap()
            .unwrap();
        assert_eq!(second.events.len(), 1);
        let ev = &second.events[0];
        assert_eq!(ev.request_id, "codex_session:thread-v2:t:3");

        // Windows NTFS 场景：Codex 持有追加句柄，文件长了但 mtime 纹丝不动。
        // 游标 mtime 与当前文件一致、只有 size 落后 → 仍须重扫出新事件。
        lines.push(token_count(400, 160, 50));
        write_file(&dir, "rollout-a.jsonl", &lines);
        let stale_size = SyncCursor {
            last_modified_nanos: file_mtime_nanos(&path),
            last_line_offset: second.cursor.last_line_offset,
            last_size: second.cursor.last_size,
        };
        // size 增长而 mtime 不变时不能整文件跳过
        let third = parse_codex_file(&path, Some(&stale_size), &RolloutIndex::new())
            .unwrap()
            .unwrap();
        assert_eq!(third.events.len(), 1);
        assert_eq!(third.events[0].request_id, "codex_session:thread-v2:t:4");
        assert_eq!(third.cursor.last_size, file_stamp(&path).size);
        assert_eq!(ev.input_tokens, 150);
        assert_eq!(ev.cache_read_tokens, 50);
        assert_eq!(ev.output_tokens, 25);
        assert_eq!(second.cursor.last_line_offset, 5);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tolerates_malformed_lines() {
        let dir = tmp_dir("garbage");
        let path = write_file(
            &dir,
            "rollout-a.jsonl",
            &[
                "{broken \"event_msg\" \"token_count\"".to_string(),
                String::new(),
                r#"{"type":"event_msg","payload":{"type":"agent_message"}}"#.to_string(),
                session_meta("t"),
                token_count(10, 0, 5),
            ],
        );

        let parse = parse_codex_file(&path, None, &RolloutIndex::new())
            .unwrap()
            .unwrap();
        assert_eq!(parse.events.len(), 1);
        assert_eq!(parse.cursor.last_line_offset, 5);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn identity_detects_all_three_snapshot_signals() {
        // 信号一：forked_from_id 非空。
        let forked = serde_json::json!({"id": "t", "forked_from_id": "parent"});
        assert!(parse_session_identity(&forked).carries_history_snapshot);
        // 信号二：source.subagent 存在。
        let subagent = serde_json::json!({"id": "t", "source": {"subagent": {"x": 1}}});
        assert!(parse_session_identity(&subagent).carries_history_snapshot);
        // 信号三：session_id 与线程 id 不一致。
        let mismatch = serde_json::json!({"id": "child", "session_id": "parent"});
        assert!(parse_session_identity(&mismatch).carries_history_snapshot);

        // 普通会话：id 与 session_id 相同、无 fork/subagent → 不携带快照。
        assert!(
            !parse_session_identity(&serde_json::json!({"id": "t", "session_id": "t"}))
                .carries_history_snapshot
        );
        assert!(!parse_session_identity(&serde_json::json!({"id": "t"})).carries_history_snapshot);
        // forked_from_id 为空串不算 fork。
        assert!(
            !parse_session_identity(&serde_json::json!({"id": "t", "forked_from_id": ""}))
                .carries_history_snapshot
        );
    }

    #[test]
    fn fork_replay_segment_recovers_baseline_but_is_not_billed() {
        let dir = tmp_dir("fork");
        // 父线程：重放段的两条就来自这里，签名比对靠它定出重放边界。
        let parent = write_file(
            &dir,
            "rollout-parent.jsonl",
            &[
                session_meta("parent-thread"),
                turn_context("gpt-5.6"),
                token_count(100, 50, 10),
                token_count(200, 100, 20),
            ],
        );
        let path = write_file(
            &dir,
            "rollout-fork.jsonl",
            &[
                forked_session_meta("child-thread", "parent-thread"),
                turn_context("gpt-5.6"),
                token_count(100, 50, 10),  // 重放 line 3（父线程历史）
                token_count(200, 100, 20), // 重放 line 4
                thread_settings_applied(), // 接管边界 line 5
                token_count(350, 150, 45), // 真实 line 6：delta vs 200 = 150/50/25
                token_count(500, 200, 70), // 真实 line 7：delta vs 350 = 150/50/25
            ],
        );
        // 这两个测试文件名不含 UUID（历史写法），手工建索引而非走
        // build_rollout_index——本例要验的是重放剥离，不是文件名解析。
        let mut index = RolloutIndex::new();
        index.insert("parent-thread".to_string(), parent);

        let parse = parse_codex_file(&path, None, &index).unwrap().unwrap();
        // 重放段两条不计费，仅接管后两条产出。
        assert_eq!(parse.events.len(), 2, "重放段不得双算");
        // 序号连续（重放段占 1、2），真实事件为 3、4，request_id 稳定。
        assert_eq!(
            parse.events[0].request_id,
            "codex_session:thread-v2:child-thread:3"
        );
        // 基线从重放段的累计值恢复，delta 才正确（否则会把 350 当首个 delta）。
        assert_eq!(parse.events[0].input_tokens, 150, "基线恢复后 delta 正确");
        assert_eq!(parse.events[0].cache_read_tokens, 50);
        assert_eq!(parse.events[0].output_tokens, 25);
        assert_eq!(
            parse.events[1].request_id,
            "codex_session:thread-v2:child-thread:4"
        );
        assert_eq!(parse.events[1].input_tokens, 150);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn subagent_replay_boundary_via_inter_agent_communication() {
        let dir = tmp_dir("subagent");
        // source.subagent 信号 + inter_agent_communication 边界（另一条边界类型）。
        let path = write_file(
            &dir,
            "rollout-sub.jsonl",
            &[
                subagent_session_meta("sub-thread"),
                token_count(100, 50, 10),    // 重放 line 2
                inter_agent_communication(), // 接管边界 line 3
                token_count(250, 100, 30),   // 真实 line 4：delta vs 100 = 150/50/20
            ],
        );

        let parse = parse_codex_file(&path, None, &RolloutIndex::new())
            .unwrap()
            .unwrap();
        assert_eq!(parse.events.len(), 1, "子代理重放段不计费");
        assert_eq!(
            parse.events[0].request_id,
            "codex_session:thread-v2:sub-thread:2"
        );
        assert_eq!(parse.events[0].input_tokens, 150, "基线从重放段恢复");
        assert_eq!(parse.events[0].cache_read_tokens, 50);
        assert_eq!(parse.events[0].output_tokens, 20);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_forked_session_ignores_boundary_and_counts_all() {
        let dir = tmp_dir("nonfork");
        let path = write_file(
            &dir,
            "rollout-plain.jsonl",
            &[
                session_meta("plain-thread"), // 无 fork 信号
                token_count(100, 50, 10),
                thread_settings_applied(), // 普通会话也可能出现，但不该触发跳过
                token_count(200, 100, 20),
            ],
        );

        let parse = parse_codex_file(&path, None, &RolloutIndex::new())
            .unwrap()
            .unwrap();
        // 无历史快照信号 → boundary=None → 两条 token_count 全计入。
        assert_eq!(parse.events.len(), 2, "非 fork 会话不得因边界事件漏算");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn collects_sessions_and_archived() {
        let dir = tmp_dir("collect");
        let day_dir = dir.join("sessions").join("2026").join("07").join("10");
        let archived = dir.join("archived_sessions");
        fs::create_dir_all(&day_dir).unwrap();
        fs::create_dir_all(&archived).unwrap();
        fs::write(day_dir.join("rollout-a.jsonl"), "{}").unwrap();
        fs::write(archived.join("rollout-old.jsonl"), "{}").unwrap();
        fs::write(archived.join("notes.txt"), "x").unwrap();

        let files = collect_codex_files(&dir);
        assert_eq!(files.len(), 2);

        let _ = fs::remove_dir_all(&dir);
    }

    // ---------- fork 重放：按父 rollout 数值比对（取代特征事件启发式） ----------

    const PARENT_UUID: &str = "019eb735-6f8b-7751-a761-8055900db580";
    const CHILD_UUID: &str = "019f02eb-53a2-7093-be4d-7f07bde8c068";

    fn rollout_name(uuid: &str) -> String {
        format!("rollout-2026-06-26T15-53-24-{uuid}.jsonl")
    }

    /// 父线程三次调用；子线程 fork 后逐字重放这三条，再接自己的两次。
    fn parent_and_child(dir: &Path) -> (PathBuf, PathBuf) {
        let replayed = [
            token_count(100, 0, 10),
            token_count(200, 0, 20),
            token_count(300, 0, 30),
        ];
        let parent = write_file(
            dir,
            &rollout_name(PARENT_UUID),
            &[
                vec![session_meta(PARENT_UUID), turn_context("gpt-5.6")],
                replayed.to_vec(),
            ]
            .concat(),
        );
        // 关键：子文件里**没有** thread_settings_applied / inter_agent_communication，
        // 这正是本机 16 个真实文件命中的形态——老的特征事件启发式在这里判定
        // "无边界"，于是把重放段也算成新用量。
        let child = write_file(
            dir,
            &rollout_name(CHILD_UUID),
            &[
                vec![
                    forked_session_meta(CHILD_UUID, PARENT_UUID),
                    turn_context("gpt-5.6"),
                ],
                replayed.to_vec(),
                vec![token_count(400, 0, 40), token_count(500, 0, 50)],
            ]
            .concat(),
        );
        (parent, child)
    }

    fn total_input(parse: &FileParse) -> i64 {
        parse.events.iter().map(|e| e.input_tokens).sum()
    }

    /// 核心回归：重放段按父线程的真实用量数值识别，不依赖任何标志事件。
    #[test]
    fn fork_replay_is_stripped_by_parent_signature_without_marker_events() {
        let dir = tmp_dir("fork-signature");
        let (parent, child) = parent_and_child(&dir);
        let index = build_rollout_index(&[parent, child.clone()]);

        let parse = parse_codex_file(&child, None, &index).unwrap().unwrap();

        // 只有子线程自己的两次调用计费：400→500 与 300→400 各 100 input。
        assert_eq!(
            total_input(&parse),
            200,
            "重放段没被剔除：父线程已计过的用量被子线程又算了一遍（events={:?}）",
            parse.events.len()
        );
        assert_eq!(parse.events.len(), 2, "只应计入子线程自己的两次调用");

        let _ = fs::remove_dir_all(&dir);
    }

    /// 父 rollout 不在扫描范围时整份跳过——宁可少计，不可把父线程的账再记一遍。
    #[test]
    fn fork_without_reachable_parent_is_skipped_entirely() {
        let dir = tmp_dir("fork-orphan");
        let (_parent, child) = parent_and_child(&dir);
        // 索引里故意不放父文件，模拟父 rollout 被删/归档到别处。
        let index = build_rollout_index(std::slice::from_ref(&child));

        let parse = parse_codex_file(&child, None, &index).unwrap().unwrap();

        assert!(
            parse.events.is_empty(),
            "找不到父 rollout 时不得计费，否则整段重放历史会被当成新用量"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// 非 fork 会话完全不受这套机制影响（不能矫枉过正吃掉正常用量）。
    #[test]
    fn plain_session_is_unaffected_by_replay_stripping() {
        let dir = tmp_dir("fork-plain");
        let path = write_file(
            &dir,
            &rollout_name(PARENT_UUID),
            &[
                session_meta(PARENT_UUID),
                turn_context("gpt-5.6"),
                token_count(100, 0, 10),
                token_count(200, 0, 20),
            ],
        );
        let index = build_rollout_index(std::slice::from_ref(&path));

        let parse = parse_codex_file(&path, None, &index).unwrap().unwrap();

        assert_eq!(parse.events.len(), 2);
        assert_eq!(total_input(&parse), 200, "普通会话的用量不得被削减");

        let _ = fs::remove_dir_all(&dir);
    }

    // ---------- 多 lane 交错计数（对齐 cc-switch #5854） ----------

    /// 同一请求带精确 last 时直用 last，不做累计相减——多 lane 的累计值
    /// 各自独立推进，相邻相减会把请求算多遍。
    #[test]
    fn interleaved_lanes_prefer_exact_last_usage() {
        let dir = tmp_dir("lanes-last");
        let path = write_file(
            &dir,
            "rollout-a.jsonl",
            &[
                session_meta("t"),
                turn_context("gpt-5.6"),
                // lane A 累计到 1000；lane B 的累计从自己的口径 600 起——
                // 若按相邻相减，两条各算一遍全量。last 才是真实增量。
                token_count_lane("codex", (1000, 0, 100), Some((1000, 0, 100))),
                token_count_lane("codex_bengalfox", (600, 0, 60), Some((200, 0, 20))),
            ],
        );
        let parse = parse_codex_file(&path, None, &RolloutIndex::new())
            .unwrap()
            .unwrap();
        assert_eq!(parse.events.len(), 2);
        assert_eq!(parse.events[0].input_tokens, 1000);
        assert_eq!(parse.events[1].input_tokens, 200, "直用 last，不做交错相减");

        let _ = fs::remove_dir_all(&dir);
    }

    /// 限流刷新换 lane 重发相同快照：跨源重复与紧邻前一事件比对后不计费。
    ///
    /// 重发快照连 `last` 一起原样带回——去重不生效时 last 优先路径会把同一
    /// 笔增量再计一遍（这正是去重的受力点；纯 total 场景 high-water 本身就
    /// 兜住了，测试必须带 last 才能压出这条防线）。
    #[test]
    fn cross_lane_snapshot_replay_is_not_double_counted() {
        let dir = tmp_dir("lanes-dup");
        let path = write_file(
            &dir,
            "rollout-a.jsonl",
            &[
                session_meta("t"),
                token_count_lane("codex", (1000, 200, 100), Some((1000, 200, 100))),
                // 换 lane 重发完全相同的快照（限流刷新通知，last 原样带回）。
                token_count_lane("codex_bengalfox", (1000, 200, 100), Some((1000, 200, 100))),
                // 同源第二次重复也不计费。
                token_count_lane("codex", (1000, 200, 100), Some((1000, 200, 100))),
                // 真实推进：只计 last 的 100。
                token_count_lane("codex", (1100, 200, 120), Some((100, 0, 20))),
            ],
        );
        let parse = parse_codex_file(&path, None, &RolloutIndex::new())
            .unwrap()
            .unwrap();
        assert_eq!(parse.events.len(), 2, "重发快照不得经 last 路径二次计费");
        assert_eq!(parse.events[0].input_tokens, 1000);
        assert_eq!(parse.events[1].input_tokens, 100);
        assert_eq!(parse.events[1].output_tokens, 20);

        let _ = fs::remove_dir_all(&dir);
    }

    /// lane 切换导致累计值回退再涨：high-water 基线只对超出历史最大值的
    /// 部分计费（旧的单一 prev 基线会把回退后的再涨整段重复计）。
    #[test]
    fn lane_regression_bills_only_beyond_high_water() {
        let dir = tmp_dir("lanes-hw");
        let path = write_file(
            &dir,
            "rollout-a.jsonl",
            &[
                session_meta("t"),
                token_count_lane("codex", (1000, 0, 100), None),
                // 另一 lane 的累计口径较小（回退）：不计费，但也不拉低基线。
                token_count_lane("codex_bengalfox", (400, 0, 40), None),
                // 回到主 lane 推进到 1200：只有超出 1000 的 200 计费。
                // 旧实现以 400 为基线会计 800——同一段用量被算两遍。
                token_count_lane("codex", (1200, 0, 130), None),
            ],
        );
        let parse = parse_codex_file(&path, None, &RolloutIndex::new())
            .unwrap()
            .unwrap();
        let total: i64 = parse.events.iter().map(|e| e.input_tokens).sum();
        assert_eq!(total, 1200, "合计不得超过最大累计值（重复计费信号）");
        assert_eq!(parse.events.last().unwrap().input_tokens, 200);

        let _ = fs::remove_dir_all(&dir);
    }

    /// 文件名尾部的 UUID 就是线程 id——索引靠它零 IO 建立，认错即全盘失效。
    #[test]
    fn thread_id_comes_from_rollout_filename_tail() {
        assert_eq!(
            thread_id_from_filename(Path::new(&rollout_name(CHILD_UUID))).as_deref(),
            Some(CHILD_UUID)
        );
        // 非 rollout 命名不得误判出 id。
        assert_eq!(thread_id_from_filename(Path::new("notes.txt")), None);
        assert_eq!(thread_id_from_filename(Path::new("rollout-a.jsonl")), None);
    }
}
