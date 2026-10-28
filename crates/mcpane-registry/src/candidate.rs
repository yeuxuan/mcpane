//! Registry 条目 → 可预填进「添加服务器」表单的安装候选。
//!
//! **这里只翻译，不执行**：候选交给界面预填表单，用户核对完整命令后自己保存。
//! v1 支持 npm、pypi 与远程端点；其余包类型明确标「暂不支持」而不是瞎拼一条命令。

use serde::Serialize;

use crate::model::{Argument, Entry, Package, Remote, Server};

/// 候选来自哪里。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    /// 官方 Registry 登记的条目。
    Registry,
    /// 用户粘贴的包名 / 远程地址没有登记，按其字面意思直接生成。
    Direct,
}

/// stdio 服务器需要的运行时（界面据此提示「需要 Node.js / uv」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Runtime {
    Node,
    Uv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RemoteTransport {
    Http,
    Sse,
}

/// 表单里要出现的一行（环境变量或请求头）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    pub name: String,
    /// 固定值 / 默认值；空串表示要用户自己填。
    pub value: String,
    pub secret: bool,
    pub required: bool,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Install {
    Stdio {
        runtime: Runtime,
        command: String,
        args: Vec<String>,
        env: Vec<Field>,
    },
    Remote {
        transport: RemoteTransport,
        url: String,
        headers: Vec<Field>,
    },
    Unsupported {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    /// 稳定标识（同一条目的多个安装方式靠 `#序号` 区分），前端用作 key。
    pub id: String,
    pub source: Source,
    /// 建议填进表单的服务器名称。
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    /// 反向域名命名空间（`io.github.upstash`）。原样展示，由用户判断来源。
    pub namespace: Option<String>,
    pub repository_url: Option<String>,
    pub website_url: Option<String>,
    /// 与用户粘贴的地址一致。
    pub exact: bool,
    pub install: Install,
    /// 预填后用户还得自己补的东西（缺值的必填参数、URL 里的 `{占位}`）。
    pub todo: Vec<String>,
}

/// 文案过长时截断（Registry 文字是第三方输入，界面只当纯文本显示）。
fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let mut out: String = text.chars().take(max).collect();
        out.push('…');
        out
    }
}

fn clip_opt(text: &Option<String>, max: usize) -> Option<String> {
    text.as_deref()
        .map(|t| clip(t, max))
        .filter(|t| !t.is_empty())
}

/// 命名空间 = `/` 之前的部分。
fn namespace_of(name: &str) -> Option<String> {
    name.split_once('/').map(|(ns, _)| ns.to_owned())
}

/// 太泛、做不了服务器名的最后一段。
const GENERIC_NAMES: &[&str] = &["mcp", "server", "mcp-server", "app", "api", "default"];

/// 建议的服务器名：最后一段；太泛时（`com.notion/mcp`）换成命名空间里最有辨识度的一段（`notion`）。
fn suggested_name(server: &Server) -> String {
    let last = server
        .name
        .rsplit('/')
        .next()
        .unwrap_or(&server.name)
        .to_owned();
    if !GENERIC_NAMES.contains(&last.as_str()) {
        return last;
    }
    // 命名空间 `com.notion` / `io.github.upstash`：取最后一段，且不是 github 这类平台名。
    namespace_of(&server.name)
        .and_then(|ns| ns.rsplit('.').next().map(str::to_owned))
        .filter(|seg| !matches!(seg.as_str(), "github" | "com" | "io" | "ai" | "dev"))
        .unwrap_or(last)
}

fn field_from_env(env: &crate::model::EnvVar) -> Field {
    Field {
        name: env.name.clone(),
        value: env
            .value
            .clone()
            .or_else(|| env.default.clone())
            .unwrap_or_default(),
        secret: env.is_secret,
        required: env.is_required,
        description: clip_opt(&env.description, 160),
    }
}

/// 一个参数 → argv 片段。缺值的必填参数不预填，记进 `todo`。
fn push_argument(arg: &Argument, args: &mut Vec<String>, todo: &mut Vec<String>) {
    let value = arg.value.clone().or_else(|| arg.default.clone());
    let hint = || {
        arg.value_hint
            .clone()
            .or_else(|| arg.name.clone())
            .or_else(|| arg.description.clone())
            .map(|h| clip(&h, 60))
            .unwrap_or_else(|| "参数".to_owned())
    };
    if arg.kind == "named" {
        let Some(name) = arg.name.clone() else { return };
        // Registry 里既有 `--port` 也有裸的 `allowed-directories`：补成命令行能用的 flag。
        let name = if name.starts_with('-') {
            name
        } else {
            format!("--{name}")
        };
        match value {
            Some(value) if !value.is_empty() => {
                args.push(name);
                args.push(value);
            }
            _ if arg.is_required => todo.push(format!("参数 {name} 需要填写")),
            // 无值的可选具名参数在规范里是开关；没有 value 就不带，免得带出一个空 flag。
            _ => {}
        }
        return;
    }
    match value {
        Some(value) if !value.is_empty() => args.push(value),
        _ if arg.is_required => todo.push(format!("需要填写参数：{}", hint())),
        _ => {}
    }
}

fn contains_placeholder(text: &str) -> bool {
    text.contains('{') && text.contains('}')
}

/// 包 → 安装方式。`None` 表示这个包类型不属于 v1 的支持范围。
fn install_from_package(package: &Package, todo: &mut Vec<String>) -> Result<Install, String> {
    // 包自己要监听端口（sse / streamable-http）：得先在本机起一个 HTTP 服务，不是「添加即用」。
    if let Some(transport) = &package.transport
        && !transport.kind.is_empty()
        && transport.kind != "stdio"
    {
        return Err(format!(
            "该包以 {} 方式提供服务，需要先在本机启动，暂不支持",
            transport.kind
        ));
    }
    let (runtime, command, spec) = match package.registry_type.as_str() {
        "npm" => (
            Runtime::Node,
            "npx",
            match &package.version {
                Some(version) => format!("{}@{version}", package.identifier),
                None => package.identifier.clone(),
            },
        ),
        "pypi" => (
            Runtime::Uv,
            "uvx",
            match &package.version {
                Some(version) => format!("{}@{version}", package.identifier),
                None => package.identifier.clone(),
            },
        ),
        other => {
            return Err(format!(
                "暂不支持 {} 类型的包",
                match other {
                    "oci" => "Docker（oci）",
                    "nuget" => "NuGet",
                    "mcpb" => "MCPB",
                    unknown => unknown,
                }
            ));
        }
    };
    let mut args = Vec::new();
    if runtime == Runtime::Node {
        args.push("-y".to_owned());
    }
    // 运行时参数里已经带了 -y / --yes 的不再重复。
    for arg in &package.runtime_arguments {
        if matches!(arg.value.as_deref(), Some("-y" | "--yes")) {
            continue;
        }
        push_argument(arg, &mut args, todo);
    }
    // `uvx --from <包==版本> <脚本名>`：条目用 `--from` 指定包、用位置参数指定要跑的控制台脚本时，
    // 包标识符本身不是可执行名，再塞一个 `<包>@<版本>` 会让 uvx 去跑一个不存在的东西。
    let installs_via_from = package
        .runtime_arguments
        .iter()
        .any(|a| a.name.as_deref() == Some("--from"));
    if !installs_via_from {
        args.push(spec);
    }
    for arg in &package.package_arguments {
        push_argument(arg, &mut args, todo);
    }
    for value in &args {
        if contains_placeholder(value) {
            todo.push(format!("参数里有待补全的占位：{}", clip(value, 60)));
        }
    }
    Ok(Install::Stdio {
        runtime,
        command: command.to_owned(),
        args,
        env: package
            .environment_variables
            .iter()
            .map(field_from_env)
            .collect(),
    })
}

/// 远程端点 → 安装方式。URL 里的 `{变量}` 用登记的默认值替换，剩下的记进 `todo`。
fn install_from_remote(remote: &Remote, todo: &mut Vec<String>) -> Result<Install, String> {
    let transport = match remote.kind.as_str() {
        "streamable-http" => RemoteTransport::Http,
        "sse" => RemoteTransport::Sse,
        other => return Err(format!("暂不支持 {other} 类型的远程端点")),
    };
    let mut url = remote.url.trim().to_owned();
    for (name, variable) in &remote.variables {
        if let Some(default) = &variable.default {
            url = url.replace(&format!("{{{name}}}"), default);
        }
    }
    if contains_placeholder(&url) {
        todo.push("远程地址里有待补全的占位，请在 URL 里改成实际值".to_owned());
    }
    if !is_safe_remote(&url) {
        return Err("远程地址不是 https，已忽略".to_owned());
    }
    // 认证头不预填成一行（交给 OAuth / Bearer Token 栏），但条目明说要 Token 的要提醒用户去填。
    for header in &remote.headers {
        if header.name.eq_ignore_ascii_case("authorization")
            && (header.is_required || header.value.as_deref().is_some_and(contains_placeholder))
        {
            todo.push(match clip_opt(&header.description, 80) {
                Some(description) => format!("需要在「Bearer Token」里填写：{description}"),
                None => "需要在「Bearer Token」里填写访问令牌".to_owned(),
            });
        }
    }
    let headers = remote
        .headers
        .iter()
        .filter(|h| !h.name.eq_ignore_ascii_case("authorization"))
        .map(|h| Field {
            name: h.name.clone(),
            value: h.value.clone().unwrap_or_default(),
            secret: h.is_secret,
            required: h.is_required,
            description: clip_opt(&h.description, 160),
        })
        .collect();
    Ok(Install::Remote {
        transport,
        url,
        headers,
    })
}

/// 只收 https；本机回环（本地开发服务器）允许 http。
pub fn is_safe_remote(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        // 带 `{占位}` 的模板解析不了：不算不安全，交给 todo 提示用户补全。
        return url.starts_with("https://");
    };
    match parsed.scheme() {
        "https" => true,
        "http" => match parsed.host() {
            Some(url::Host::Domain(domain)) => domain == "localhost",
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        },
        _ => false,
    }
}

/// 一个条目展开成候选：每个受支持的安装方式一张卡；全都不支持时留一张「暂不支持」卡说明原因。
pub fn candidates_from_entry(entry: &Entry, exact: bool) -> Vec<Candidate> {
    let server = &entry.server;
    let mut built: Vec<(Install, Vec<String>)> = Vec::new();
    let mut unsupported: Vec<String> = Vec::new();

    // 远程：有 streamable-http 就不再列同一服务器的 sse（旧传输，新协议不定义）。
    let has_http = server.remotes.iter().any(|r| r.kind == "streamable-http");
    for remote in &server.remotes {
        if has_http && remote.kind == "sse" {
            continue;
        }
        let mut todo = Vec::new();
        match install_from_remote(remote, &mut todo) {
            Ok(install) => built.push((install, todo)),
            Err(reason) => unsupported.push(reason),
        }
    }
    for package in &server.packages {
        let mut todo = Vec::new();
        match install_from_package(package, &mut todo) {
            Ok(install) => built.push((install, todo)),
            Err(reason) => unsupported.push(reason),
        }
    }
    let mut unique: Vec<(Install, Vec<String>)> = Vec::new();
    for item in built {
        if !unique.iter().any(|(install, _)| *install == item.0) {
            unique.push(item);
        }
    }
    let mut built = unique;
    if built.is_empty() {
        built.push((
            Install::Unsupported {
                reason: unsupported
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| "条目里没有可用的安装方式".to_owned()),
            },
            Vec::new(),
        ));
    }
    built
        .into_iter()
        .enumerate()
        .map(|(index, (install, todo))| Candidate {
            id: format!("{}#{index}", server.name),
            source: Source::Registry,
            name: suggested_name(server),
            title: clip_opt(&server.title, 80),
            description: clip_opt(&server.description, 240),
            namespace: namespace_of(&server.name),
            repository_url: server
                .repository
                .as_ref()
                .map(|r| r.url.trim().to_owned())
                .filter(|u| u.starts_with("https://")),
            website_url: server
                .website_url
                .as_deref()
                .map(str::trim)
                .filter(|u| u.starts_with("https://"))
                .map(str::to_owned),
            exact,
            install,
            todo,
        })
        .collect()
}
