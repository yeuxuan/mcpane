//! 工具检索（Tool Discovery 模式的查询端）。
//!
//! 经典 BM25（k1=1.2, b=0.75），文档 = 单个工具的 name + description +
//! server slug 词袋（name 词计双份权重——按名字搜是主路径）。规模假设：
//! 一个 Workspace 至多几百工具，线性打分即可，不建倒排索引（YAGNI）。
//!
//! 词法：小写化、按非字母数字切分；查询词长 ≥3 时允许前缀命中
//! （`scr` 命中 `screenshot`——工具名常被截半输入）。

use std::collections::BTreeMap;

use crate::CatalogTool;

const K1: f32 = 1.2;
const B: f32 = 0.75;
/// 前缀命中的最短查询词长：太短的前缀（如 `a`）会命中一切，只做全词匹配。
const MIN_PREFIX_LEN: usize = 3;

/// 单段文本的最多取词数。给查询与文档字段都设上限，杜绝
/// 「已认证客户端传数 MB query → BM25 的 O(terms×docs×tokens) CPU 放大」。
const MAX_TOKENS_PER_FIELD: usize = 64;

fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .take(MAX_TOKENS_PER_FIELD)
        .map(str::to_ascii_lowercase)
        .collect()
}

/// 单个工具的词袋（name 双倍权重；含参数名/参数描述以补召回）。
fn tool_tokens(tool: &CatalogTool) -> Vec<String> {
    let mut tokens = tokenize(&tool.original_name);
    tokens.extend(tokenize(&tool.original_name));
    tokens.extend(tokenize(&tool.server_slug));
    if let Some(description) = &tool.description {
        tokens.extend(tokenize(description));
    }
    // input_schema 的参数名与参数描述：一个只靠参数语义匹配的查询
    // （工具名/描述都不含关键词、但某参数叫 repository）否则搜不到。
    if let Some(props) = tool
        .input_schema
        .get("properties")
        .and_then(|p| p.as_object())
    {
        for (param_name, param) in props.iter().take(MAX_TOKENS_PER_FIELD) {
            tokens.extend(tokenize(param_name));
            if let Some(desc) = param.get("description").and_then(|d| d.as_str()) {
                tokens.extend(tokenize(desc));
            }
        }
    }
    tokens
}

/// 查询词是否命中文档词（全词，或长查询词的前缀）。
fn matches(term: &str, token: &str) -> bool {
    token == term || (term.len() >= MIN_PREFIX_LEN && token.starts_with(term))
}

/// 在给定工具集内按 BM25 检索，返回按分数降序的前 `limit` 个。
/// 空查询/无命中返回空；「永不为空」的兜底（降级全量目录）在 hub 层做
/// （ADR-020）——检索层保持纯函数语义，方便 describe 的候选提示等场景复用。
pub fn search_tools<'a>(
    tools: &[&'a CatalogTool],
    query: &str,
    limit: usize,
) -> Vec<&'a CatalogTool> {
    let terms = tokenize(query);
    if terms.is_empty() || tools.is_empty() || limit == 0 {
        return Vec::new();
    }

    let docs: Vec<Vec<String>> = tools.iter().map(|t| tool_tokens(t)).collect();
    let total_docs = docs.len() as f32;
    let avg_len = docs.iter().map(Vec::len).sum::<usize>() as f32 / total_docs;

    // 每个查询词的文档频次（含前缀命中）。
    let mut doc_freq: BTreeMap<&str, f32> = BTreeMap::new();
    for term in &terms {
        let df = docs
            .iter()
            .filter(|tokens| tokens.iter().any(|tok| matches(term, tok)))
            .count() as f32;
        doc_freq.insert(term.as_str(), df);
    }

    let mut scored: Vec<(f32, usize)> = docs
        .iter()
        .enumerate()
        .filter_map(|(i, tokens)| {
            let doc_len = tokens.len() as f32;
            let mut score = 0.0f32;
            for term in &terms {
                let tf = tokens.iter().filter(|tok| matches(term, tok)).count() as f32;
                if tf == 0.0 {
                    continue;
                }
                let df = doc_freq[term.as_str()];
                let idf = ((total_docs - df + 0.5) / (df + 0.5) + 1.0).ln();
                score += idf * (tf * (K1 + 1.0)) / (tf + K1 * (1.0 - B + B * doc_len / avg_len));
            }
            (score > 0.0).then_some((score, i))
        })
        .collect();
    // 同分按暴露名字典序，结果稳定可复现。
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| tools[a.1].exposed_name.cmp(&tools[b.1].exposed_name))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, i)| tools[i])
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::{CatalogOverrides, ServerTools, build_revision};

    fn tool(name: &str, description: &str) -> rmcp::model::Tool {
        rmcp::model::Tool::new(
            name.to_owned(),
            description.to_owned(),
            std::sync::Arc::new(serde_json::Map::from_iter([(
                "type".to_owned(),
                serde_json::json!("object"),
            )])),
        )
    }

    fn sample() -> crate::CatalogRevision {
        build_revision(
            "ws1",
            &[
                ServerTools {
                    server_id: "s1".into(),
                    server_slug: "github".into(),
                    tools: vec![
                        tool("create_issue", "Create a new issue in a repository"),
                        tool("search_code", "Search source code across repositories"),
                    ],
                    ..Default::default()
                },
                ServerTools {
                    server_id: "s2".into(),
                    server_slug: "browser".into(),
                    tools: vec![
                        tool("screenshot", "Take a screenshot of the current page"),
                        tool("navigate", "Navigate the browser to a URL"),
                    ],
                    ..Default::default()
                },
            ],
            &CatalogOverrides::default(),
        )
        .unwrap()
    }

    #[test]
    fn finds_by_name_description_and_prefix() {
        let revision = sample();
        let tools: Vec<&CatalogTool> = revision.tools.iter().collect();

        let hits = search_tools(&tools, "screenshot", 5);
        assert_eq!(hits[0].exposed_name, "browser__screenshot");

        // 描述词也能命中。
        let hits = search_tools(&tools, "repository", 5);
        assert!(
            hits.iter()
                .any(|t| t.exposed_name == "github__create_issue")
        );

        // 前缀：截半的工具名（≥3 字符）。
        let hits = search_tools(&tools, "scr", 5);
        assert!(hits.iter().any(|t| t.exposed_name == "browser__screenshot"));

        // 太短的前缀不命中（防止 `a` 命中一切）。
        assert!(search_tools(&tools, "sc", 5).is_empty());
    }

    #[test]
    fn name_hits_outrank_description_hits_and_limit_applies() {
        let revision = sample();
        let tools: Vec<&CatalogTool> = revision.tools.iter().collect();

        // "search" 在 search_code 是名字词（双倍权重），在别处最多是描述词。
        let hits = search_tools(&tools, "search", 5);
        assert_eq!(hits[0].exposed_name, "github__search_code");

        assert_eq!(search_tools(&tools, "search code repository", 1).len(), 1);
        assert!(search_tools(&tools, "", 5).is_empty());
        assert!(search_tools(&tools, "quantum", 5).is_empty(), "无命中即空");
    }

    /// 召回：工具名/描述都不含关键词，但某参数名/描述含——应能搜到
    /// （对齐竞品 BM25F 索引参数 schema）。
    #[test]
    fn recall_via_input_schema_params() {
        let schema = std::sync::Arc::new(serde_json::Map::from_iter([
            ("type".to_owned(), serde_json::json!("object")),
            (
                "properties".to_owned(),
                serde_json::json!({
                    "owner": { "type": "string", "description": "GitHub organization slug" }
                }),
            ),
        ]));
        let opaque = rmcp::model::Tool::new("run", "does a thing", schema);
        let revision = build_revision(
            "ws1",
            &[ServerTools {
                server_id: "s1".into(),
                server_slug: "svc".into(),
                tools: vec![opaque],
                ..Default::default()
            }],
            &CatalogOverrides::default(),
        )
        .unwrap();
        let tools: Vec<&CatalogTool> = revision.tools.iter().collect();
        // 参数名命中。
        assert_eq!(search_tools(&tools, "owner", 5).len(), 1);
        // 参数描述命中。
        assert_eq!(search_tools(&tools, "organization", 5).len(), 1);
    }

    /// 加固：超长 query 不引发失控开销（token 数被 MAX_TOKENS_PER_FIELD 封顶）。
    #[test]
    fn oversized_query_is_bounded() {
        let revision = sample();
        let tools: Vec<&CatalogTool> = revision.tools.iter().collect();
        let huge = "search ".repeat(100_000);
        // 不 panic、正常返回；tokenize 只取前 MAX_TOKENS_PER_FIELD 个词。
        let hits = search_tools(&tools, &huge, 5);
        assert!(!hits.is_empty(), "首个词仍命中");
    }
}
