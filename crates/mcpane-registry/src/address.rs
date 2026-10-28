//! 用户粘贴的「地址」解析：GitHub 仓库、npm / PyPI 包、远程 URL，或一段普通文字。
//!
//! 官方 Registry 的搜索只匹配服务器**名称的一部分**（实测：`context7` 命中，只写 owner 的
//! `crystaldba` 与带空格的 `postgres mcp` 都是 0 条），所以粘贴 URL 不能原样去搜，得先拆成
//! 检索词、再用规范化后的地址精确匹配。规范化的原则：宁可不匹配，也不让一个裸主机或某人的
//! 主页「代表」所有仓库。

use url::Url;

/// 解析结果。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Address {
    /// GitHub 仓库 `owner/repo`（已转小写、去 `.git`）。
    Github { owner: String, repo: String },
    /// npm 包（含 `@scope/name`）。
    Npm { name: String },
    /// PyPI 包。
    Pypi { name: String },
    /// 其他 https 链接：可能是远程 MCP 端点，也可能只是产品网页。
    Remote { url: String },
    /// 普通文字：按名称搜索。
    Words { text: String },
}

/// 解析用户输入。空串返回 `None`。
pub fn parse_address(input: &str) -> Option<Address> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    if let Some(rest) = input.strip_prefix("git@github.com:") {
        return github_from_path(rest);
    }
    if input.contains(char::is_whitespace) {
        return Some(Address::Words {
            text: input.to_owned(),
        });
    }
    if let Some(url) = parse_url(input) {
        return Some(from_url(&url, input));
    }
    // 没有协议头：`host/path` 形态（github.com/a/b、npmjs.com/package/x）也认。
    if let Some(first) = input.split('/').next()
        && first.contains('.')
        && !first.starts_with('@')
        && let Some(url) = parse_url(&format!("https://{input}"))
    {
        return Some(from_url(&url, input));
    }
    // `@scope/name` 一定是 npm；`owner/repo` 当 GitHub 仓库；其余裸词按名称搜索。
    if input.starts_with('@') && input.matches('/').count() == 1 {
        return Some(Address::Npm {
            name: input.to_lowercase(),
        });
    }
    if input.matches('/').count() == 1 {
        return github_from_path(input);
    }
    Some(Address::Words {
        text: input.to_owned(),
    })
}

fn parse_url(raw: &str) -> Option<Url> {
    let url = Url::parse(raw).ok()?;
    matches!(url.scheme(), "http" | "https")
        .then_some(url)
        .filter(|u| u.host_str().is_some())
}

fn host_of(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default().to_lowercase();
    host.strip_prefix("www.").unwrap_or(&host).to_owned()
}

fn from_url(url: &Url, original: &str) -> Address {
    let host = host_of(url);
    let segments: Vec<&str> = url
        .path_segments()
        .map(|s| s.filter(|p| !p.is_empty()).collect())
        .unwrap_or_default();
    match host.as_str() {
        "github.com" => {
            if segments.len() >= 2 {
                return github_from_path(&format!("{}/{}", segments[0], segments[1]))
                    .unwrap_or_else(|| Address::Words {
                        text: original.to_owned(),
                    });
            }
            // 只有 owner 的主页：不代表任何一个仓库。
            Address::Words {
                text: original.to_owned(),
            }
        }
        "npmjs.com" | "registry.npmjs.org" => {
            let rest: &[&str] = match segments.first() {
                Some(&"package") => &segments[1..],
                _ => &segments[..],
            };
            let name = match rest {
                [scope, name, ..] if scope.starts_with('@') => format!("{scope}/{name}"),
                [name, ..] => (*name).to_owned(),
                [] => {
                    return Address::Words {
                        text: original.to_owned(),
                    };
                }
            };
            Address::Npm {
                name: name.to_lowercase(),
            }
        }
        "pypi.org" => match segments.as_slice() {
            ["project", name, ..] => Address::Pypi {
                name: name.to_lowercase(),
            },
            _ => Address::Words {
                text: original.to_owned(),
            },
        },
        _ => Address::Remote {
            url: connect_url(url),
        },
    }
}

fn github_from_path(path: &str) -> Option<Address> {
    let mut parts = path.trim_matches('/').split('/');
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let repo = parts.next().filter(|s| !s.is_empty())?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    Some(Address::Github {
        owner: owner.to_lowercase(),
        repo: repo.to_lowercase(),
    })
}

/// URL 规范化（用于「精确匹配」比较）：小写主机、去 `www.`、userinfo、query、fragment、尾斜杠。
/// 协议不参与比较（http / https 同一资源），统一成 https 便于展示。
pub fn normalize_url(url: &Url) -> String {
    let path = url.path().trim_end_matches('/');
    let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
    format!("https://{}{port}{path}", host_of(url))
}

/// 可直接连接的形态：保留协议与主机原样（`www.` 不能擅自去掉），只丢 userinfo / query / fragment
/// 与尾斜杠。与 `normalize_url` 不同，它会被预填进表单，不能改写用户给的地址语义。
fn connect_url(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default().to_lowercase();
    let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
    format!(
        "{}://{host}{port}{}",
        url.scheme(),
        url.path().trim_end_matches('/')
    )
}

/// 规范化任意 URL 字符串；解析不了返回 `None`。
pub fn normalize_url_str(raw: &str) -> Option<String> {
    parse_url(raw.trim()).map(|u| normalize_url(&u))
}

/// 规范化仓库地址（Registry 里 `repository.url` 的写法五花八门：带 `.git`、`/tree/…`、`git+https://`）。
pub fn normalize_repo(raw: &str) -> Option<(String, String)> {
    let raw = raw.trim();
    let raw = raw.strip_prefix("git+").unwrap_or(raw);
    match parse_address(raw)? {
        Address::Github { owner, repo } => Some((owner, repo)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github(owner: &str, repo: &str) -> Option<Address> {
        Some(Address::Github {
            owner: owner.into(),
            repo: repo.into(),
        })
    }

    #[test]
    fn github_forms_all_collapse_to_owner_and_repo() {
        for input in [
            "https://github.com/Upstash/context7",
            "https://github.com/upstash/context7.git",
            "https://www.github.com/upstash/context7/",
            "https://github.com/upstash/context7/tree/main/packages/mcp",
            "https://github.com/upstash/context7?tab=readme#install",
            "github.com/upstash/context7",
            "git@github.com:upstash/context7.git",
            "upstash/context7",
        ] {
            assert_eq!(
                parse_address(input),
                github("upstash", "context7"),
                "{input}"
            );
        }
    }

    #[test]
    fn an_owner_page_or_bare_host_never_stands_for_a_repository() {
        for input in [
            "https://github.com/upstash",
            "https://github.com/",
            "github.com",
        ] {
            assert!(
                !matches!(parse_address(input), Some(Address::Github { .. })),
                "{input}"
            );
        }
    }

    #[test]
    fn npm_and_pypi_packages() {
        let npm = |n: &str| Some(Address::Npm { name: n.into() });
        assert_eq!(
            parse_address("@upstash/context7-mcp"),
            npm("@upstash/context7-mcp")
        );
        assert_eq!(
            parse_address("https://www.npmjs.com/package/@upstash/context7-mcp"),
            npm("@upstash/context7-mcp")
        );
        assert_eq!(
            parse_address("https://www.npmjs.com/package/some-server"),
            npm("some-server")
        );
        assert_eq!(
            parse_address("npmjs.com/package/some-server/v/1.2.3"),
            npm("some-server")
        );
        assert_eq!(
            parse_address("https://pypi.org/project/mcp-server-time/"),
            Some(Address::Pypi {
                name: "mcp-server-time".into()
            })
        );
        assert!(matches!(
            parse_address("https://pypi.org/"),
            Some(Address::Words { .. })
        ));
    }

    #[test]
    fn other_links_are_remote_endpoints_with_a_normalized_form() {
        assert_eq!(
            parse_address("https://MCP.Notion.com/mcp/?utm=x#frag"),
            Some(Address::Remote {
                url: "https://mcp.notion.com/mcp".into()
            })
        );
        assert_eq!(
            parse_address("http://127.0.0.1:8080/mcp"),
            Some(Address::Remote {
                url: "http://127.0.0.1:8080/mcp".into()
            }),
            "预填用的地址保留协议，不能把 http 悄悄改成 https"
        );
        assert_eq!(
            parse_address("https://www.example.com/mcp"),
            Some(Address::Remote {
                url: "https://www.example.com/mcp".into()
            }),
            "www. 不能擅自去掉"
        );
    }

    #[test]
    fn words_and_blank_input() {
        assert_eq!(parse_address("   "), None);
        assert_eq!(parse_address(""), None);
        assert_eq!(
            parse_address("postgres mcp"),
            Some(Address::Words {
                text: "postgres mcp".into()
            })
        );
        assert_eq!(
            parse_address("context7"),
            Some(Address::Words {
                text: "context7".into()
            })
        );
    }

    #[test]
    fn repository_urls_from_the_registry_normalize_the_same_way() {
        for raw in [
            "https://github.com/upstash/context7",
            "git+https://github.com/upstash/context7.git",
            "https://github.com/upstash/context7/tree/main/packages/mcp",
        ] {
            assert_eq!(
                normalize_repo(raw),
                Some(("upstash".into(), "context7".into())),
                "{raw}"
            );
        }
        assert_eq!(normalize_repo("https://gitlab.com/a/b"), None);
    }

    #[test]
    fn url_normalization_ignores_scheme_case_query_and_trailing_slash() {
        assert_eq!(
            normalize_url_str("HTTP://Www.Example.com:8443/mcp/?a=1#x"),
            Some("https://example.com:8443/mcp".into())
        );
        assert_eq!(normalize_url_str("not a url"), None);
        assert_eq!(normalize_url_str("ftp://example.com/mcp"), None);
    }
}
