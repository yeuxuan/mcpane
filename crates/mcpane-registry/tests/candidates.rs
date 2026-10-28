//! 条目 → 候选：用真实抓取的 Registry 条目断言生成的命令、参数、环境变量与提示。
#![allow(clippy::unwrap_used)]

mod common;

use mcpane_registry::candidate::candidates_from_entry;
use mcpane_registry::{Candidate, Install, RemoteTransport, Runtime};

fn candidates(name: &str) -> Vec<Candidate> {
    candidates_from_entry(&common::entry(name), true)
}

fn stdio(c: &Candidate) -> (Runtime, &str, &[String]) {
    match &c.install {
        Install::Stdio {
            runtime,
            command,
            args,
            ..
        } => (*runtime, command, args),
        other => panic!("应为 stdio：{other:?}"),
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn npm_package_is_pinned_and_secret_env_is_kept() {
    let all = candidates("io.github.upstash/context7");
    assert_eq!(all.len(), 2, "远程一张、npm 一张，mcpb 不单列");
    let npm = all
        .iter()
        .find(|c| matches!(c.install, Install::Stdio { .. }))
        .unwrap();
    let (runtime, command, args) = stdio(npm);
    assert_eq!(runtime, Runtime::Node);
    assert_eq!(command, "npx");
    assert_eq!(args, strings(&["-y", "@upstash/context7-mcp@4.1.1"]));
    let Install::Stdio { env, .. } = &npm.install else {
        unreachable!()
    };
    assert_eq!(env.len(), 1);
    assert_eq!(env[0].name, "CONTEXT7_API_KEY");
    assert!(env[0].secret && !env[0].required);
    assert_eq!(env[0].value, "", "密钥不预填值");
    assert_eq!(npm.name, "context7");
    assert_eq!(npm.namespace.as_deref(), Some("io.github.upstash"));
}

#[test]
fn remote_prefers_streamable_http_and_drops_the_sse_twin() {
    let all = candidates("com.notion/mcp");
    assert_eq!(all.len(), 1, "同一服务器的 sse 不再单列");
    let Install::Remote { transport, url, .. } = &all[0].install else {
        panic!("{:?}", all[0].install)
    };
    assert_eq!(*transport, RemoteTransport::Http);
    assert_eq!(url, "https://mcp.notion.com/mcp");
    assert_eq!(all[0].name, "notion", "太泛的 mcp 改用命名空间里的 notion");
}

#[test]
fn authorization_header_template_becomes_a_todo_not_a_prefilled_row() {
    let all = candidates("ai.smithery/arjunkmrm-fetch");
    let Install::Remote { headers, .. } = &all[0].install else {
        panic!()
    };
    assert!(headers.is_empty(), "Authorization 不预填成请求头行");
    assert!(
        all[0].todo.iter().any(|t| t.contains("Bearer Token")),
        "{:?}",
        all[0].todo
    );
}

#[test]
fn pypi_uses_uvx_with_the_pinned_version() {
    let all = candidates("io.github.adelaidasofia/linear-mcp");
    let (runtime, command, args) = stdio(&all[0]);
    assert_eq!((runtime, command), (Runtime::Uv, "uvx"));
    assert_eq!(args, strings(&["adelaidasofia-linear-mcp@0.3.2"]));
    let Install::Stdio { env, .. } = &all[0].install else {
        unreachable!()
    };
    assert!(env[0].required && env[0].secret);
    assert_eq!(all.len(), 1, "mcpb 不单列");
}

/// 条目用 `--from <包==版本> <脚本>` 指定安装方式：不能再多塞一个 `<包>@<版本>`。
#[test]
fn uvx_from_runtime_argument_replaces_the_package_spec() {
    let all = candidates("io.github.Jackxiaozhiren/datasentry");
    let (_, command, args) = stdio(&all[0]);
    assert_eq!(command, "uvx");
    assert_eq!(
        args,
        strings(&["--from", "datasentry-ai==1.0.4", "datasentry", "mcp"])
    );
}

/// 同一个包登记了 stdio / sse / streamable-http 三种变体：只有 stdio 能「添加即用」，
/// 其余要先在本机起 HTTP 服务；缺值的必填具名参数记入待办，不预填空值。
#[test]
fn only_stdio_variant_is_built_and_required_named_arg_becomes_todo() {
    let all = candidates("io.github.bytedance/mcp-server-filesystem");
    assert_eq!(all.len(), 1, "{all:#?}");
    let (_, _, args) = stdio(&all[0]);
    assert_eq!(
        args,
        strings(&["-y", "@agent-infra/mcp-server-filesystem@latest"])
    );
    assert!(
        all[0]
            .todo
            .iter()
            .any(|t| t.contains("--allowed-directories")),
        "具名参数要补上 -- 前缀：{:?}",
        all[0].todo
    );
}

#[test]
fn optional_env_and_valueless_positional_are_not_prefilled() {
    let all = candidates("io.github.j0hanz/filesystem-mcp");
    assert_eq!(all.len(), 1, "oci 不单列");
    let (_, _, args) = stdio(&all[0]);
    assert_eq!(args, strings(&["-y", "@j0hanz/filesystem-mcp@2.7.1"]));
    assert!(all[0].todo.is_empty(), "{:?}", all[0].todo);
}

#[test]
fn unsupported_package_types_never_produce_a_command() {
    for (name, kind) in [
        ("io.github.fieldcure/filesystem", "NuGet"),
        ("io.github.rghsoftware/linux-filesystem", "MCPB"),
    ] {
        let all = candidates(name);
        assert_eq!(all.len(), 1);
        let Install::Unsupported { reason } = &all[0].install else {
            panic!("{name}：{:?}", all[0].install)
        };
        assert!(reason.contains(kind), "{reason}");
    }
}

#[test]
fn oci_alongside_a_supported_package_is_skipped_not_shown() {
    // com.mcparmory/sentry：pypi + oci → 只出 pypi 一张
    let all = candidates("com.mcparmory/sentry");
    assert_eq!(all.len(), 1);
    assert_eq!(stdio(&all[0]).1, "uvx");
}

#[test]
fn non_https_remote_is_rejected_but_loopback_http_is_allowed() {
    use mcpane_registry::candidate::is_safe_remote;
    assert!(is_safe_remote("https://mcp.example.com/mcp"));
    assert!(is_safe_remote("http://127.0.0.1:8080/mcp"));
    assert!(is_safe_remote("http://localhost:3000/mcp"));
    assert!(!is_safe_remote("http://mcp.example.com/mcp"));
    assert!(!is_safe_remote("ftp://mcp.example.com/mcp"));
    assert!(!is_safe_remote("http://169.254.169.254/latest"));
}

#[test]
fn third_party_text_is_clipped_and_non_https_links_are_dropped() {
    let mut entry = common::entry("io.github.upstash/context7");
    entry.server.description = Some("x".repeat(1000));
    entry.server.website_url = Some("javascript:alert(1)".to_owned());
    let all = candidates_from_entry(&entry, false);
    assert!(all[0].description.as_ref().unwrap().chars().count() <= 241);
    assert_eq!(all[0].website_url, None);
}
