//! Codex 会话解析的**整目录**集成测试。
//!
//! 单测都在单文件粒度上验（父子两层的重放剥离、跨 lane 去重、high-water 回退）。
//! 真实同步跑的却是「collect → build_index → 逐个 parse」，而 Codex 的用量虚高
//! 恰恰是跨文件问题：fork 会逐字重放父线程历史，一层判错就把上游全部用量再算
//! 一遍。模块注释记着本机实测的最坏形态是**四层级联 fork**，把根线程 862 万
//! input token 重复计了 3~4 倍——而级联至今没有测试。
//!
//! 本文件用一条可验算的不变量来卡总量：级联里每一层都在同一个累计计数器上
//! 继续，所以**全目录计费 input 之和必须恰好等于最深那层的累计值**。重放段
//! 漏剥一层，这个等式立刻破。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use mcpane_usage::{
    SyncCursor, UsageEvent, build_rollout_index, collect_codex_files, parse_codex_file,
};

const A: &str = "019f02eb-53a2-7093-be4d-000000000001";
const B: &str = "019f02eb-53a2-7093-be4d-000000000002";
const C: &str = "019f02eb-53a2-7093-be4d-000000000003";
const D: &str = "019f02eb-53a2-7093-be4d-000000000004";

fn rollout_name(uuid: &str) -> String {
    format!("rollout-2026-06-26T15-53-24-{uuid}.jsonl")
}

fn session_meta(thread_id: &str) -> String {
    format!(
        r#"{{"timestamp":"2026-07-10T03:00:00Z","type":"session_meta","payload":{{"id":"{thread_id}"}}}}"#
    )
}

fn forked_session_meta(thread_id: &str, forked_from: &str) -> String {
    format!(
        r#"{{"timestamp":"2026-07-10T03:00:00Z","type":"session_meta","payload":{{"id":"{thread_id}","forked_from_id":"{forked_from}"}}}}"#
    )
}

fn turn_context(model: &str) -> String {
    format!(
        r#"{{"timestamp":"2026-07-10T03:00:01Z","type":"turn_context","payload":{{"model":"{model}"}}}}"#
    )
}

/// 累计快照（total_token_usage）。
fn token_count(input: i64, cached: i64, output: i64) -> String {
    format!(
        r#"{{"timestamp":"2026-07-10T03:00:02Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{input},"cached_input_tokens":{cached},"output_tokens":{output}}}}}}}}}"#
    )
}

/// 写入 `<codex_dir>/sessions/` —— `collect_codex_files` 只认这层布局
///（外加 `archived_sessions/`），直接放根目录扫不到。
fn write(codex_dir: &Path, uuid: &str, lines: &[String]) -> PathBuf {
    let sessions = codex_dir.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let path = sessions.join(rollout_name(uuid));
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
    path
}

/// 走真实同步路径：collect → build_index → 逐文件 parse，汇总全部事件。
fn parse_all(dir: &Path, cursors: &mut HashMap<PathBuf, SyncCursor>) -> Vec<UsageEvent> {
    let files = collect_codex_files(dir);
    let index = build_rollout_index(&files);
    let mut all = Vec::new();
    for file in &files {
        let prev = cursors.get(file).copied();
        if let Some(parse) = parse_codex_file(file, prev.as_ref(), &index).unwrap() {
            cursors.insert(file.clone(), parse.cursor);
            all.extend(parse.events);
        }
    }
    all
}

fn sum_input(events: &[UsageEvent]) -> i64 {
    events.iter().map(|e| e.input_tokens).sum()
}

/// 四层级联 fork：A → B → C → D，每层都逐字重放上游全部历史再接自己的调用。
///
/// 这是模块注释里记的最坏形态。计费总量必须恰好等于最深一层的累计值——
/// 任何一层重放没剥干净，总量都会成倍上去。
#[test]
fn four_level_fork_cascade_bills_each_call_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();

    // A 自己的三次调用（累计推进到 400）。
    let a_calls = [
        token_count(100, 0, 10),
        token_count(250, 0, 25),
        token_count(400, 0, 40),
    ];
    // B fork 自 A：重放 A 的三条，再接两条（累计推进到 800）。
    let b_calls = [token_count(600, 0, 60), token_count(800, 0, 80)];
    // C fork 自 B：重放 A+B 全部五条，再接一条（累计到 1000）。
    let c_calls = [token_count(1000, 0, 100)];
    // D fork 自 C：重放全部六条，再接一条（累计到 1300）。
    let d_calls = [token_count(1300, 0, 130)];

    write(
        d,
        A,
        &[
            vec![session_meta(A), turn_context("gpt-5.6")],
            a_calls.to_vec(),
        ]
        .concat(),
    );
    write(
        d,
        B,
        &[
            vec![forked_session_meta(B, A), turn_context("gpt-5.6")],
            a_calls.to_vec(),
            b_calls.to_vec(),
        ]
        .concat(),
    );
    write(
        d,
        C,
        &[
            vec![forked_session_meta(C, B), turn_context("gpt-5.6")],
            a_calls.to_vec(),
            b_calls.to_vec(),
            c_calls.to_vec(),
        ]
        .concat(),
    );
    write(
        d,
        D,
        &[
            vec![forked_session_meta(D, C), turn_context("gpt-5.6")],
            a_calls.to_vec(),
            b_calls.to_vec(),
            c_calls.to_vec(),
            d_calls.to_vec(),
        ]
        .concat(),
    );

    let events = parse_all(d, &mut HashMap::new());

    assert_eq!(
        sum_input(&events),
        1300,
        "级联 fork 的计费总量必须等于最深一层的累计值（重放段一条都不能重复计）：{:?}",
        events
            .iter()
            .map(|e| (e.session_id.clone(), e.input_tokens))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        events.iter().map(|e| e.output_tokens).sum::<i64>(),
        130,
        "output 同理"
    );

    // 去重主键必须全局唯一——否则落库时会互相覆盖，或在没有唯一约束的
    // 聚合里被算两遍。
    let mut ids: Vec<&str> = events.iter().map(|e| e.request_id.as_str()).collect();
    ids.sort_unstable();
    let before = ids.len();
    ids.dedup();
    assert_eq!(before, ids.len(), "request_id 必须全局唯一");

    // 每一层都该有自己的计费事件，不能因为剥过头而整层归零。
    for (uuid, expected) in [(A, 400), (B, 400), (C, 200), (D, 300)] {
        let got: i64 = events
            .iter()
            .filter(|e| e.session_id.as_deref() == Some(uuid))
            .map(|e| e.input_tokens)
            .sum();
        assert_eq!(got, expected, "线程 {uuid} 的自有增量应为 {expected}");
    }
}

/// 同一个父线程分出两个兄弟 fork：各自剥掉同一段重放历史，互不影响。
#[test]
fn sibling_forks_each_strip_the_shared_parent_history() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();

    let a_calls = [token_count(100, 0, 10), token_count(300, 0, 30)];
    write(
        d,
        A,
        &[
            vec![session_meta(A), turn_context("gpt-5.6")],
            a_calls.to_vec(),
        ]
        .concat(),
    );
    // 两个兄弟各自从 A 的累计基线继续。
    write(
        d,
        B,
        &[
            vec![forked_session_meta(B, A), turn_context("gpt-5.6")],
            a_calls.to_vec(),
            vec![token_count(500, 0, 50)],
        ]
        .concat(),
    );
    write(
        d,
        C,
        &[
            vec![forked_session_meta(C, A), turn_context("gpt-5.6")],
            a_calls.to_vec(),
            vec![token_count(400, 0, 40)],
        ]
        .concat(),
    );

    let events = parse_all(d, &mut HashMap::new());
    // A 自己 300；B 在 A 基线上再 200；C 在 A 基线上再 100。
    assert_eq!(
        sum_input(&events),
        300 + 200 + 100,
        "兄弟 fork 各自只计自己的增量：{:?}",
        events
            .iter()
            .map(|e| (e.session_id.clone(), e.input_tokens))
            .collect::<Vec<_>>()
    );
}

/// 整目录增量重扫：文件追加后再跑一轮，已计过的事件不得重复产出，
/// 且同一逻辑事件的 request_id 必须保持不变（否则落库会变成两行）。
#[test]
fn incremental_rescan_over_the_whole_directory_does_not_double_bill() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();

    let a_calls = vec![token_count(100, 0, 10), token_count(250, 0, 25)];
    let a_head = vec![session_meta(A), turn_context("gpt-5.6")];
    let b_head = vec![forked_session_meta(B, A), turn_context("gpt-5.6")];
    let b_calls = vec![token_count(400, 0, 40)];

    write(d, A, &[a_head.clone(), a_calls.clone()].concat());
    write(
        d,
        B,
        &[b_head.clone(), a_calls.clone(), b_calls.clone()].concat(),
    );

    let mut cursors = HashMap::new();
    let first = parse_all(d, &mut cursors);
    assert_eq!(sum_input(&first), 400, "首轮：A 250 + B 150");

    // 两个文件都继续增长：A 又跑了一次，B 也又跑了一次。
    // mtime 需要真的变化，直接重写文件即可（tempdir 内的新写入 mtime 必然更新）。
    let a_more = vec![token_count(500, 0, 50)];
    let b_more = vec![token_count(700, 0, 70)];
    write(d, A, &[a_head, a_calls.clone(), a_more].concat());
    write(d, B, &[b_head, a_calls, b_calls, b_more].concat());

    let second = parse_all(d, &mut cursors);

    // 第二轮只应产出新增部分：A 从 250→500 = 250；B 从 400→700 = 300。
    assert_eq!(
        sum_input(&second),
        550,
        "增量轮只计新增：{:?}",
        second
            .iter()
            .map(|e| (e.request_id.clone(), e.input_tokens))
            .collect::<Vec<_>>()
    );

    // 两轮合起来仍然是一套互不重复的主键——落库按 request_id 去重后
    // 总量必须等于真实累计（A 500 + B 在 A 基线上的 450）。
    let mut by_id: HashMap<String, i64> = HashMap::new();
    for event in first.iter().chain(second.iter()) {
        by_id.insert(event.request_id.clone(), event.input_tokens);
    }
    assert_eq!(
        by_id.values().sum::<i64>(),
        500 + 450,
        "按 request_id 去重后的总量必须是真实用量"
    );
}

/// fork 的父 rollout 不在扫描范围时整份跳过，且**游标不推进**——
/// 父文件日后恢复后必须还能重新参与计算，不能被当成"已处理"永久跳过。
#[test]
fn fork_without_parent_is_skipped_but_stays_reprocessable() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();

    let a_calls = vec![token_count(100, 0, 10), token_count(300, 0, 30)];
    write(
        d,
        B,
        &[
            vec![forked_session_meta(B, A), turn_context("gpt-5.6")],
            a_calls.clone(),
            vec![token_count(500, 0, 50)],
        ]
        .concat(),
    );

    let mut cursors = HashMap::new();
    let events = parse_all(d, &mut cursors);
    assert!(
        events.is_empty(),
        "父 rollout 不可达时整份跳过（宁可少计不多计）：{events:?}"
    );
    let cursor = cursors.values().next().copied().unwrap_or_default();
    assert_eq!(
        cursor,
        SyncCursor::default(),
        "跳过时游标一格都不能进，否则父文件恢复后这份永远不会被重算"
    );

    // 父文件恢复到扫描范围：这一份必须重新参与计算，并且只计自己的增量。
    write(
        d,
        A,
        &[vec![session_meta(A), turn_context("gpt-5.6")], a_calls].concat(),
    );
    let events = parse_all(d, &mut cursors);
    assert_eq!(
        sum_input(&events),
        300 + 200,
        "父文件恢复后：A 自己 300，B 在其基线上再 200：{:?}",
        events
            .iter()
            .map(|e| (e.session_id.clone(), e.input_tokens))
            .collect::<Vec<_>>()
    );
}
