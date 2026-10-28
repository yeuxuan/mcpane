//! 官方 MCP Registry（`/v0/servers`）的响应模型。
//!
//! 只解我们用到的字段，且处处容错：Registry 的 schema 还在演进（条目里同时见过
//! `2025-07-09` 与 `2025-10-17` 两版 `$schema`），未知字段忽略、缺失数组按空、可选标志缺省为 false。

use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchResponse {
    #[serde(default)]
    pub servers: Vec<Entry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    pub server: Server,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    /// 反向域名命名空间 + 名称，如 `io.github.upstash/context7`、`com.notion/mcp`。
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub website_url: Option<String>,
    #[serde(default)]
    pub repository: Option<Repository>,
    #[serde(default)]
    pub packages: Vec<Package>,
    #[serde(default)]
    pub remotes: Vec<Remote>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Repository {
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Package {
    /// `npm` / `pypi` / `oci` / `nuget` / `mcpb`。
    pub registry_type: String,
    pub identifier: String,
    #[serde(default)]
    pub version: Option<String>,
    /// 缺省按 stdio。`sse` / `streamable-http` 表示这个包自己要在本机监听一个端口。
    #[serde(default)]
    pub transport: Option<Transport>,
    #[serde(default)]
    pub environment_variables: Vec<EnvVar>,
    /// 传给包本身的参数。
    #[serde(default)]
    pub package_arguments: Vec<Argument>,
    /// 传给运行时（npx / uvx / docker）的参数。
    #[serde(default)]
    pub runtime_arguments: Vec<Argument>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Transport {
    #[serde(rename = "type", default)]
    pub kind: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvVar {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub is_secret: bool,
    #[serde(default)]
    pub is_required: bool,
    #[serde(default)]
    pub default: Option<String>,
    /// 固定值（非空时无需用户填）。
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Argument {
    /// `positional` / `named`。
    #[serde(rename = "type", default)]
    pub kind: String,
    /// named 参数的名字（带横线，如 `--port`）。
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub value_hint: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub is_required: bool,
    #[serde(default)]
    pub default: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Remote {
    /// `streamable-http` / `sse`。
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub headers: Vec<Header>,
    /// URL 里 `{占位}` 的变量表。
    #[serde(default)]
    pub variables: HashMap<String, Variable>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Header {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub is_secret: bool,
    #[serde(default)]
    pub is_required: bool,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Variable {
    #[serde(default)]
    pub default: Option<String>,
}
