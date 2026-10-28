//! 解析流程：注入假的检索函数，不碰网络。
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};

use mcpane_registry::model::Entry;
use mcpane_registry::{Install, Notice, RegistryError, Source, parse_address, resolve};

/// 假 Registry：按检索词子串匹配服务器名（与真实行为一致：只搜名称的一部分），并记录收到的词。
fn fake(
    log: Arc<Mutex<Vec<String>>>,
) -> impl Fn(String) -> std::future::Ready<Result<Vec<Entry>, RegistryError>> {
    move |term: String| {
        log.lock().unwrap().push(term.clone());
        let hits = common::entries()
            .into_values()
            .filter(|e| e.server.name.to_lowercase().contains(&term.to_lowercase()))
            .collect();
        std::future::ready(Ok(hits))
    }
}

async fn run(input: &str) -> (mcpane_registry::Resolution, Vec<String>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let resolution = resolve(parse_address(input).unwrap(), fake(log.clone()))
        .await
        .unwrap();
    let terms = log.lock().unwrap().clone();
    (resolution, terms)
}

#[tokio::test]
async fn github_repo_searches_repo_then_owner_and_pins_the_exact_match_first() {
    let (resolution, mut terms) = run("https://github.com/upstash/context7").await;
    terms.sort();
    assert_eq!(
        terms,
        ["context7", "upstash"],
        "去掉 mcp 修饰后与原词相同时不重复检索"
    );
    let first = &resolution.candidates[0];
    assert!(first.exact, "与地址一致的置顶");
    assert_eq!(first.namespace.as_deref(), Some("io.github.upstash"));
    assert!(resolution.notices.is_empty());
}

#[tokio::test]
async fn similar_results_follow_the_exact_ones_and_are_not_marked_exact() {
    // 搜 filesystem 命中四个条目，按名称排序时精确匹配（fieldcure/filesystem）排在 bytedance 之后：
    // 不做「精确置顶」的话它会落在相近结果后面。
    let (resolution, _) = run("filesystem").await;
    assert!(resolution.candidates.len() > 1);
    let first = &resolution.candidates[0];
    assert!(first.exact);
    assert_eq!(first.namespace.as_deref(), Some("io.github.fieldcure"));
    assert!(
        resolution.candidates[1..].iter().all(|c| !c.exact),
        "其余是相近结果"
    );
}

/// 真实案例：npm 包 `@upstash/context7-mcp`，Registry 里叫 `io.github.upstash/context7`，
/// 名称里没有 `-mcp`。只搜包名会找不到，还会错发一个「未登记」的直接候选。
#[tokio::test]
async fn npm_name_with_an_mcp_suffix_still_finds_the_registered_server() {
    let (resolution, mut terms) = run("@upstash/context7-mcp").await;
    terms.sort();
    assert_eq!(terms, ["context7", "context7-mcp"]);
    assert!(
        resolution.candidates[0].exact,
        "{:#?}",
        resolution.candidates
    );
    assert!(
        resolution
            .candidates
            .iter()
            .all(|c| c.source == Source::Registry),
        "已登记就不该再给直接候选"
    );
    assert!(!resolution.notices.contains(&Notice::DirectCandidate));
}

#[tokio::test]
async fn remote_url_matches_by_normalized_host_and_path() {
    let (resolution, terms) = run("https://MCP.Notion.com/mcp/?utm=x#frag").await;
    assert_eq!(terms, ["notion"]);
    let first = &resolution.candidates[0];
    assert!(first.exact);
    let Install::Remote { url, .. } = &first.install else {
        panic!()
    };
    assert_eq!(url, "https://mcp.notion.com/mcp");
    assert_eq!(first.source, Source::Registry);
}

#[tokio::test]
async fn unlisted_github_repo_reports_not_listed_and_never_guesses_a_command() {
    let (resolution, _) = run("https://github.com/crystaldba/postgres-mcp").await;
    assert!(resolution.notices.contains(&Notice::RepositoryNotListed));
    assert!(
        resolution
            .candidates
            .iter()
            .all(|c| c.source == Source::Registry && !c.exact),
        "没有登记就不生成直接候选"
    );
}

#[tokio::test]
async fn unlisted_npm_package_becomes_a_direct_candidate_flagged_as_unregistered() {
    let (resolution, _) = run("@someone/never-registered-mcp").await;
    assert!(resolution.notices.contains(&Notice::DirectCandidate));
    let direct = &resolution.candidates[0];
    assert_eq!(direct.source, Source::Direct);
    assert!(!direct.exact);
    let Install::Stdio { command, args, .. } = &direct.install else {
        panic!()
    };
    assert_eq!(command, "npx");
    assert_eq!(args, &["-y", "@someone/never-registered-mcp"]);
}

#[tokio::test]
async fn unlisted_plain_http_remote_gets_no_direct_candidate() {
    let (resolution, _) = run("http://unlisted.example.com/mcp").await;
    assert!(
        resolution
            .candidates
            .iter()
            .all(|c| c.source != Source::Direct),
        "明文 http 的远程不生成直接候选"
    );
}

#[tokio::test]
async fn one_failed_search_still_returns_the_other_with_a_partial_notice() {
    let calls = Arc::new(Mutex::new(0));
    let counter = calls.clone();
    let fetch = move |term: String| {
        let mut n = counter.lock().unwrap();
        *n += 1;
        let result = if term == "upstash" {
            Err(RegistryError::Network("超时".into()))
        } else {
            Ok(vec![common::entry("io.github.upstash/context7")])
        };
        std::future::ready(result)
    };
    let address = parse_address("https://github.com/upstash/context7").unwrap();
    let resolution = resolve(address, fetch).await.unwrap();
    assert_eq!(*calls.lock().unwrap(), 2);
    assert!(resolution.notices.contains(&Notice::PartialFailure));
    assert!(resolution.candidates[0].exact);
}

#[tokio::test]
async fn all_searches_failing_is_an_error() {
    let fetch =
        |_: String| std::future::ready(Err::<Vec<Entry>, _>(RegistryError::Network("断网".into())));
    let address = parse_address("https://github.com/upstash/context7").unwrap();
    assert!(resolve(address, fetch).await.is_err());
}

#[tokio::test]
async fn duplicate_entries_from_two_searches_are_folded() {
    // repo 词与 owner 词都会命中 context7：同一条目只出现一次。
    let (resolution, _) = run("https://github.com/upstash/context7").await;
    let ids: Vec<&str> = resolution
        .candidates
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(ids.len(), unique.len(), "{ids:?}");
}

/// 联网回归（手动）：`cargo test -p mcpane-registry -- --ignored`。
#[tokio::test]
#[ignore = "需要联网访问官方 MCP Registry"]
async fn live_registry_knows_context7() {
    let resolution = mcpane_registry::discover("https://github.com/upstash/context7")
        .await
        .unwrap()
        .unwrap();
    assert!(
        resolution.candidates.iter().any(|c| c.exact),
        "{resolution:#?}"
    );
}

/// 整个结果要能序列化给前端：内部标签的枚举不能带「裸字符串」变体（运行时才报错）。
#[tokio::test]
async fn every_address_kind_serializes_for_the_frontend() {
    for input in [
        "https://github.com/upstash/context7",
        "@upstash/context7-mcp",
        "mcp-server-time",
        "https://pypi.org/project/mcp-server-time",
        "https://mcp.notion.com/mcp",
        "context7",
    ] {
        let (resolution, _) = run(input).await;
        let json =
            serde_json::to_value(&resolution).unwrap_or_else(|e| panic!("{input} 序列化失败：{e}"));
        assert!(json["address"]["kind"].is_string(), "{input}：{json}");
    }
}
