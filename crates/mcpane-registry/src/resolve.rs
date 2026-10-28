//! 把「用户粘贴的地址」变成候选列表（ADR-025）。
//!
//! 流程：地址 → 检索词（Registry 只搜名称的一部分，见 `address`）→ 并发检索 → 用规范化后的
//! 地址做**精确匹配**（置顶，标「地址一致」）→ 其余作「相近结果」→ Registry 里没有时，
//! npm / PyPI 包名与远程 URL 按字面意思生成「直接候选」，GitHub 仓库则如实告知没有。

use std::collections::HashSet;
use std::future::Future;

use serde::Serialize;

use crate::address::{Address, normalize_repo, normalize_url_str};
use crate::candidate::{
    Candidate, Install, RemoteTransport, Runtime, Source, candidates_from_entry, is_safe_remote,
};
use crate::client::RegistryError;
use crate::model::Entry;

/// 「相近结果」最多留几条（精确匹配不受限）。
const MAX_SIMILAR: usize = 8;

/// 给界面的提示（带稳定代号，文案由前端 i18n）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Notice {
    /// 官方 Registry 里没有这个 GitHub 仓库。
    RepositoryNotListed,
    /// 没有登记，按字面意思直接生成了候选（来源未经 Registry 校验）。
    DirectCandidate,
    /// 有一路检索失败，结果可能不全。
    PartialFailure,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Resolution {
    pub address: Address,
    pub candidates: Vec<Candidate>,
    pub notices: Vec<Notice>,
}

/// 最多并发几个检索词。
const MAX_TERMS: usize = 3;

/// 去掉包名 / 仓库名里常见的 `mcp` 修饰：`context7-mcp` → `context7`。
/// Registry 的服务器名常是 `io.github.upstash/context7`，而 npm 包叫 `@upstash/context7-mcp`，
/// 只搜包名会漏掉。
fn strip_generic(name: &str) -> String {
    let mut out = name;
    for prefix in ["mcp-server-", "server-", "mcp-"] {
        if let Some(rest) = out.strip_prefix(prefix) {
            out = rest;
            break;
        }
    }
    for suffix in ["-mcp-server", "-server", "-mcp"] {
        if let Some(rest) = out.strip_suffix(suffix) {
            out = rest;
            break;
        }
    }
    out.to_owned()
}

/// 地址 → 检索词（去重，最多 `MAX_TERMS` 个，并发发出）。
pub fn search_terms(address: &Address) -> Vec<String> {
    let mut terms = match address {
        Address::Github { owner, repo } => vec![repo.clone(), strip_generic(repo), owner.clone()],
        Address::Npm { name } => {
            let bare = name.rsplit('/').next().unwrap_or(name);
            vec![bare.to_owned(), strip_generic(bare)]
        }
        Address::Pypi { name } => vec![name.clone(), strip_generic(name)],
        Address::Remote { url } => remote_label(url).into_iter().collect(),
        Address::Words { text } => vec![text.clone()],
    };
    let mut seen = HashSet::new();
    terms.retain(|term| !term.is_empty() && seen.insert(term.clone()));
    terms.truncate(MAX_TERMS);
    terms
}

/// 远程 URL 的主机名里最有辨识度的一段：`mcp.notion.com` → `notion`。
fn remote_label(url: &str) -> Option<String> {
    let host = url::Url::parse(url).ok()?.host_str()?.to_owned();
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() >= 2 {
        Some(labels[labels.len() - 2].to_owned())
    } else {
        None
    }
}

/// 条目是否与地址精确对应。
fn is_exact(address: &Address, entry: &Entry) -> bool {
    let server = &entry.server;
    match address {
        Address::Github { owner, repo } => server
            .repository
            .as_ref()
            .and_then(|r| normalize_repo(&r.url))
            .is_some_and(|(o, r)| &o == owner && &r == repo),
        Address::Npm { name } => server
            .packages
            .iter()
            .any(|p| p.registry_type == "npm" && p.identifier.eq_ignore_ascii_case(name)),
        Address::Pypi { name } => server
            .packages
            .iter()
            .any(|p| p.registry_type == "pypi" && p.identifier.eq_ignore_ascii_case(name)),
        Address::Remote { url } => normalize_url_str(url).is_some_and(|wanted| {
            server
                .remotes
                .iter()
                .filter_map(|r| normalize_url_str(&r.url))
                .any(|u| u == wanted)
        }),
        // 文字只在「名称完全一致」时算精确：`context7` 对 `io.github.upstash/context7`。
        Address::Words { text } => server
            .name
            .rsplit('/')
            .next()
            .is_some_and(|last| last.eq_ignore_ascii_case(text)),
    }
}

/// 没有登记时，按字面意思生成的候选。
fn direct_candidate(address: &Address) -> Option<Candidate> {
    let (name, install) = match address {
        Address::Npm { name } => (
            name.rsplit('/').next().unwrap_or(name).to_owned(),
            Install::Stdio {
                runtime: Runtime::Node,
                command: "npx".to_owned(),
                args: vec!["-y".to_owned(), name.clone()],
                env: Vec::new(),
            },
        ),
        Address::Pypi { name } => (
            name.clone(),
            Install::Stdio {
                runtime: Runtime::Uv,
                command: "uvx".to_owned(),
                args: vec![name.clone()],
                env: Vec::new(),
            },
        ),
        Address::Remote { url } => {
            if !is_safe_remote(url) {
                return None;
            }
            (
                remote_label(url).unwrap_or_else(|| "remote".to_owned()),
                Install::Remote {
                    transport: RemoteTransport::Http,
                    url: url.clone(),
                    headers: Vec::new(),
                },
            )
        }
        Address::Github { .. } | Address::Words { .. } => return None,
    };
    Some(Candidate {
        id: "direct".to_owned(),
        source: Source::Direct,
        name,
        title: None,
        description: None,
        namespace: None,
        repository_url: None,
        website_url: None,
        exact: false,
        install,
        todo: Vec::new(),
    })
}

/// 解析并检索。`fetch` 注入（生产走 Registry 客户端，测试给假数据）。
///
/// 全部检索失败才返回错误；只失败一路则用另一路的结果并带上 `PartialFailure`。
pub async fn resolve<F, Fut>(address: Address, fetch: F) -> Result<Resolution, RegistryError>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<Vec<Entry>, RegistryError>>,
{
    let terms = search_terms(&address);
    // 检索词并发：官方 Registry 单次响应在 5–30 秒之间波动，串行会把等待叠加。
    let run = |index: usize| {
        let term = terms.get(index).cloned();
        let fetch = &fetch;
        async move {
            match term {
                Some(term) => Some(fetch(term).await),
                None => None,
            }
        }
    };
    let (a, b, c) = tokio::join!(run(0), run(1), run(2));
    let results: Vec<_> = [a, b, c].into_iter().flatten().collect();
    let mut entries: Vec<Entry> = Vec::new();
    let mut seen = HashSet::new();
    let mut failures = 0usize;
    let mut last_error = None;
    for result in results.iter() {
        match result {
            Ok(list) => {
                for entry in list {
                    if seen.insert(entry.server.name.clone()) {
                        entries.push(entry.clone());
                    }
                }
            }
            Err(error) => {
                failures += 1;
                last_error = Some(error.clone());
            }
        }
    }
    if !results.is_empty()
        && failures == results.len()
        && let Some(error) = last_error
    {
        return Err(error);
    }

    let mut notices = Vec::new();
    if failures > 0 {
        notices.push(Notice::PartialFailure);
    }
    let (exact, similar): (Vec<&Entry>, Vec<&Entry>) =
        entries.iter().partition(|entry| is_exact(&address, entry));

    let mut candidates = Vec::new();
    if exact.is_empty() {
        match direct_candidate(&address) {
            Some(direct) => {
                candidates.push(direct);
                notices.push(Notice::DirectCandidate);
            }
            None if matches!(address, Address::Github { .. }) => {
                notices.push(Notice::RepositoryNotListed);
            }
            None => {}
        }
    }
    for entry in &exact {
        candidates.extend(candidates_from_entry(entry, true));
    }
    for entry in similar.iter().take(MAX_SIMILAR) {
        candidates.extend(candidates_from_entry(entry, false));
    }
    Ok(Resolution {
        address,
        candidates,
        notices,
    })
}
