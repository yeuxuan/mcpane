//! MCP Server 导出为通用 `{"mcpServers": {...}}` JSON（ADR-027）。导出文件会离开本机，
//! 按 AGENTS.md 第 8 条把疑似凭据换成 `${变量名}` 占位符，导入时请用户补填。

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use mcpane_storage::{McpServerDef, ServerTransport};
use serde_json::{Map, Value};

use super::import::{is_var_name, placeholders_of};
use super::servers::server_repo;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerExport {
    /// 导出的 JSON 文本（凭据已替换为占位符）。
    pub json: String,
    /// 被替换的位置，导出框逐条列给用户核对。
    pub redactions: Vec<Redaction>,
}

#[derive(Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Redaction {
    pub server: String,
    pub field: RedactedField,
    pub variable: String,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RedactedField {
    Bearer,
    Env,
    Headers,
    Url,
    Args,
}

/// 生成导出内容（导出框预览与「复制」用）。
#[tauri::command]
pub async fn servers_export(ids: Vec<String>) -> Result<ServerExport, String> {
    build_export(&selected_defs(&ids).await?)
}

/// 导出到「下载」文件夹并在文件管理器里定位，返回文件路径。
/// 内容按 id 在这里重新生成而不收前端文本：不带凭据的保证只在后端成立。
#[tauri::command]
pub async fn servers_export_save(
    app: tauri::AppHandle,
    ids: Vec<String>,
) -> Result<String, String> {
    use tauri::Manager;
    use tauri_plugin_opener::OpenerExt;

    let export = build_export(&selected_defs(&ids).await?)?;
    let dir = app
        .path()
        .download_dir()
        .map_err(|e| format!("找不到「下载」文件夹：{e}"))?;
    let path = write_new_file(&dir, "mcpane-servers", &export.json)
        .map_err(|e| format!("写入 {} 失败：{e}", dir.display()))?;
    // 定位失败不影响导出本身：文件已经写好，路径会显示在界面上。
    if let Err(error) = app.opener().reveal_item_in_dir(&path) {
        eprintln!("[mcpane] 在文件管理器中定位导出文件失败：{error}");
    }
    Ok(path.display().to_string())
}

async fn selected_defs(ids: &[String]) -> Result<Vec<McpServerDef>, String> {
    if ids.is_empty() {
        return Err("未选择要导出的服务器".into());
    }
    let all = server_repo()
        .await?
        .list()
        .await
        .map_err(|e| e.to_string())?;
    // 按服务器页的列表顺序导出，而不是勾选顺序。
    let defs: Vec<McpServerDef> = all.into_iter().filter(|d| ids.contains(&d.id)).collect();
    if let Some(missing) = ids.iter().find(|id| !defs.iter().any(|d| &d.id == *id)) {
        return Err(format!("服务器不存在: {missing}"));
    }
    Ok(defs)
}

/// 同名文件已存在时按浏览器下载的习惯追加 ` (2)`；`create_new` 保证不覆盖已有文件。
fn write_new_file(dir: &Path, stem: &str, content: &str) -> std::io::Result<PathBuf> {
    use std::io::Write;

    for n in 1..1000 {
        let name = if n == 1 {
            format!("{stem}.json")
        } else {
            format!("{stem} ({n}).json")
        };
        let path = dir.join(name);
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        if let Err(e) = file.write_all(content.as_bytes()) {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
        return Ok(path);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "同名导出文件过多",
    ))
}

fn build_export(defs: &[McpServerDef]) -> Result<ServerExport, String> {
    let mut servers = Map::new();
    let mut redactions = Vec::new();
    for def in defs {
        // 编辑会保留 slug 而允许改名，两个 Server 可能同名；JSON 对象的键不能重复。
        let mut key = def.name.clone();
        let mut n = 2;
        while servers.contains_key(&key) {
            key = format!("{} ({n})", def.name);
            n += 1;
        }
        let mut redactor = Redactor::new(def);
        let entry = export_entry(def, &mut redactor);
        // 明细按 JSON 里的键登记：同名的两个 Server 在导出框里才分得清。
        redactions.extend(
            redactor
                .found
                .into_iter()
                .map(|(field, variable)| Redaction {
                    server: key.clone(),
                    field,
                    variable,
                }),
        );
        servers.insert(key, Value::Object(entry));
    }
    let mut root = Map::new();
    root.insert("mcpServers".into(), Value::Object(servers));
    let json = serde_json::to_string_pretty(&Value::Object(root)).map_err(|e| e.to_string())?;
    Ok(ServerExport { json, redactions })
}

/// 字段写法沿用编辑弹窗的「复制配置」；远程条目补 `type`，贴进 Claude Code 也能直接用。
fn export_entry(def: &McpServerDef, redactor: &mut Redactor) -> Map<String, Value> {
    let mut entry = Map::new();
    let text = |s: String| Value::String(s);
    match def.transport {
        ServerTransport::Stdio => {
            if let Some(command) = &def.command {
                entry.insert("command".into(), text(command.clone()));
            }
            if !def.args.is_empty() {
                entry.insert("args".into(), redactor.args(&def.args).into());
            }
            if !def.env.is_empty() {
                entry.insert("env".into(), redactor.map(RedactedField::Env, &def.env));
            }
            if let Some(cwd) = &def.cwd {
                entry.insert("cwd".into(), text(cwd.clone()));
            }
        }
        ServerTransport::Http | ServerTransport::Sse => {
            let kind = if def.transport == ServerTransport::Sse {
                "sse"
            } else {
                "http"
            };
            entry.insert("type".into(), text(kind.into()));
            if let Some(url) = &def.url {
                entry.insert("url".into(), text(redactor.url(RedactedField::Url, url)));
            }
            if !def.headers.is_empty() {
                entry.insert(
                    "headers".into(),
                    redactor.map(RedactedField::Headers, &def.headers),
                );
            }
            if let Some(bearer) = &def.bearer {
                let value = redactor.value(RedactedField::Bearer, "bearer token", bearer);
                entry.insert("bearer".into(), text(value));
            }
            if let Some(bearer_env) = &def.bearer_env {
                entry.insert("bearerEnv".into(), text(bearer_env.clone()));
            }
            // 远程条目的 env 供 bearerEnv 就地取值。
            if !def.env.is_empty() {
                entry.insert("env".into(), redactor.map(RedactedField::Env, &def.env));
            }
        }
    }
    if !def.enabled {
        entry.insert("enabled".into(), Value::Bool(false));
    }
    entry
}

/// 单个条目的凭据替换器：登记每处替换，并保证本条目内占位符变量名不重复
/// （导入时同名变量会被填成同一个值）。
struct Redactor {
    used: HashSet<String>,
    found: Vec<(RedactedField, String)>,
    /// bearerEnv 指向的 env 键：它的值就是 Bearer Token，不论名字像不像凭据。
    bearer_env: Option<String>,
}

impl Redactor {
    fn new(def: &McpServerDef) -> Self {
        // 条目里原有的占位符也算占用，新占位符不能与之撞名。
        Self {
            used: placeholders_of(def).into_iter().collect(),
            found: Vec::new(),
            bearer_env: def.bearer_env.clone(),
        }
    }

    fn placeholder(&mut self, field: RedactedField, base: &str) -> String {
        let mut name = base.to_owned();
        let mut n = 2;
        while !self.used.insert(name.clone()) {
            name = format!("{base}_{n}");
            n += 1;
        }
        self.found.push((field, name.clone()));
        format!("${{{name}}}")
    }

    fn map(&mut self, field: RedactedField, values: &BTreeMap<String, String>) -> Value {
        values
            .iter()
            .map(|(name, value)| {
                let forced =
                    field == RedactedField::Env && self.bearer_env.as_deref() == Some(name);
                let value = self.redact(field, name, value, forced);
                (name.clone(), Value::String(value))
            })
            .collect::<Map<String, Value>>()
            .into()
    }

    /// `name` 是这个值的名字（env 键 / 请求头名 / 参数 flag），没有名字传空串。
    fn value(&mut self, field: RedactedField, name: &str, value: &str) -> String {
        self.redact(field, name, value, false)
    }

    fn redact(&mut self, field: RedactedField, name: &str, value: &str, forced: bool) -> String {
        // 开关字面量（SKIP_AUTH=true）不是秘密，替换了只会逼收件人补填一个本来就知道的值。
        if value.trim().is_empty()
            || is_placeholder_value(value)
            || (!forced && is_switch_literal(value))
        {
            return value.to_owned();
        }
        let sensitive = forced || is_sensitive_name(name);
        // 名字看不出是凭据的 URL 只换掉其中的凭据部分，保留主机等结构，收件人才知道填什么。
        if is_url(value) && !sensitive {
            return self.url(field, value);
        }
        if sensitive || looks_like_secret(value) {
            let (scheme, _) = split_scheme(value);
            let base = var_name(name).unwrap_or_else(|| "SECRET".into());
            return format!("{scheme}{}", self.placeholder(field, &base));
        }
        value.to_owned()
    }

    /// 换掉 URL 里的密码、凭据类查询参数与形如密钥的路径段（如 Firecrawl 的 `/fc-…/`）。
    fn url(&mut self, field: RedactedField, url: &str) -> String {
        let (main, fragment) = match url.split_once('#') {
            Some((main, fragment)) => (main, Some(fragment)),
            None => (url, None),
        };
        let (base, query) = match main.split_once('?') {
            Some((base, query)) => (base, Some(query)),
            None => (main, None),
        };
        let mut out = String::with_capacity(url.len());
        match base.split_once("://") {
            Some((scheme, rest)) => {
                let (authority, path) = match rest.find('/') {
                    Some(at) => rest.split_at(at),
                    None => (rest, ""),
                };
                out.push_str(scheme);
                out.push_str("://");
                out.push_str(&self.authority(field, authority));
                out.push_str(&self.path(field, path));
            }
            None => out.push_str(&self.path(field, base)),
        }
        if let Some(query) = query {
            out.push('?');
            let pairs: Vec<String> = query
                .split('&')
                .map(|pair| self.query_pair(field, pair))
                .collect();
            out.push_str(&pairs.join("&"));
        }
        if let Some(fragment) = fragment {
            out.push('#');
            out.push_str(fragment);
        }
        out
    }

    fn authority(&mut self, field: RedactedField, authority: &str) -> String {
        let Some((userinfo, host)) = authority.rsplit_once('@') else {
            return authority.to_owned();
        };
        let userinfo = match userinfo.split_once(':') {
            Some((user, password)) if !password.is_empty() && !is_pure_placeholder(password) => {
                format!("{user}:{}", self.placeholder(field, "PASSWORD"))
            }
            Some(_) => userinfo.to_owned(),
            // 只有用户名的写法常拿 Token 当用户名（https://TOKEN@host）。
            None if looks_like_secret(userinfo) => self.placeholder(field, "TOKEN"),
            None => userinfo.to_owned(),
        };
        format!("{userinfo}@{host}")
    }

    fn path(&mut self, field: RedactedField, path: &str) -> String {
        path.split('/')
            .map(|segment| {
                if looks_like_secret(segment) {
                    self.placeholder(field, "API_KEY")
                } else {
                    segment.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("/")
    }

    fn query_pair(&mut self, field: RedactedField, pair: &str) -> String {
        let Some((raw_key, raw_value)) = pair.split_once('=') else {
            return pair.to_owned();
        };
        let (key, value) = url::form_urlencoded::parse(pair.as_bytes())
            .next()
            .unwrap_or_default();
        if value.trim().is_empty() || is_pure_placeholder(raw_value) {
            return pair.to_owned();
        }
        if is_sensitive_name(&key) || looks_like_secret(&value) {
            let base = var_name(&key).unwrap_or_else(|| "SECRET".into());
            return format!("{raw_key}={}", self.placeholder(field, &base));
        }
        pair.to_owned()
    }

    fn args(&mut self, args: &[String]) -> Vec<Value> {
        let mut out = Vec::with_capacity(args.len());
        // 上一个参数是凭据类 flag（`--api-key VALUE` 写法）时，记下变量名基底。
        let mut pending: Option<String> = None;
        for arg in args {
            if let Some(base) = pending.take()
                && !arg.starts_with('-')
                && !arg.trim().is_empty()
                && !is_placeholder_value(arg)
            {
                out.push(Value::String(self.placeholder(RedactedField::Args, &base)));
                continue;
            }
            out.push(Value::String(self.arg(arg, &mut pending)));
        }
        out
    }

    fn arg(&mut self, arg: &str, pending: &mut Option<String>) -> String {
        if let Some(flag) = arg.strip_prefix('-').map(|a| a.trim_start_matches('-')) {
            return match flag.split_once('=') {
                // `--api-key=VALUE`：flag 名就说明是凭据，取值整段替换。
                Some((name, value)) if is_sensitive_name(name) => {
                    let prefix = &arg[..arg.len() - value.len()];
                    format!("{prefix}{}", self.value(ARG, name, value))
                }
                // `--header=Authorization: Bearer …`、`--env=KEY=VALUE`：取值按独立参数再认一遍。
                Some((_, value)) => {
                    let prefix = &arg[..arg.len() - value.len()];
                    format!("{prefix}{}", self.arg_value(value))
                }
                None => {
                    // `--no-auth` 之类的开关不带取值。
                    if is_sensitive_name(flag) && !flag.starts_with("no-") {
                        *pending = var_name(flag);
                    }
                    arg.to_owned()
                }
            };
        }
        self.arg_value(arg)
    }

    /// 不带 flag 的参数（或 flag 的取值部分）：URL、`KEY=VALUE`、`Header: value`、裸值。
    fn arg_value(&mut self, text: &str) -> String {
        if is_url(text) {
            return self.url(ARG, text);
        }
        // docker 的 `-e KEY=VALUE`。
        if let Some((name, value)) = text.split_once('=')
            && is_var_name(name)
        {
            return format!("{name}={}", self.value(ARG, name, value));
        }
        // mcp-remote 的 `--header "Authorization: Bearer …"`；没替换就原样返回，
        // 不能把 `C:\path`、`localhost:8080` 改写成别的样子。
        if let Some((name, value)) = text.split_once(':')
            && !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            let value = value.trim_start();
            let redacted = self.value(ARG, name, value);
            return if redacted == value {
                text.to_owned()
            } else {
                format!("{}{redacted}", &text[..text.len() - value.len()])
            };
        }
        self.value(ARG, "", text)
    }
}

const ARG: RedactedField = RedactedField::Args;

fn is_switch_literal(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "false" | "yes" | "no" | "on" | "off" | "0" | "1"
    )
}

/// 名字像凭据：token / secret / password 等词出现在任意位置即算；key / auth 等
/// 宽泛词只在末尾才算（API_KEY、X-Auth 算，KEYBOARD_LAYOUT、AUTH_TYPE 不算）。
fn is_sensitive_name(name: &str) -> bool {
    const ANYWHERE: &[&str] = &[
        "token",
        "secret",
        "secrets",
        "password",
        "passwd",
        "passphrase",
        "credential",
        "credentials",
        "cookie",
        "bearer",
        "authorization",
        "apikey",
        "jwt",
        // Slack / Discord 的 Webhook 地址本身就是凭据。
        "webhook",
    ];
    // 连写的形态：ACCESSTOKEN、CLIENTSECRET。
    const GLUED: &[&str] = &["token", "secret", "password"];
    const LAST: &[&str] = &["key", "auth", "pat", "pass", "pwd", "dsn"];
    let words = split_words(name);
    let anywhere = words.iter().any(|w| {
        ANYWHERE.contains(&w.as_str()) || GLUED.iter().any(|s| w.len() > s.len() && w.ends_with(s))
    });
    let last = words.last().is_some_and(|w| {
        // 单独一个 PWD / PASS / PAT 不是凭据（PWD 是工作目录变量）。
        let lone = words.len() == 1 && matches!(w.as_str(), "pwd" | "pass" | "pat");
        !lone && (LAST.contains(&w.as_str()) || w.ends_with("key"))
    });
    anywhere || last
}

/// 按分隔符与驼峰拆词并转小写：`exaApiKey` / `X-API-Key` / `GITHUB_TOKEN` 同等对待。
fn split_words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    for part in name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|p| !p.is_empty())
    {
        let chars: Vec<char> = part.chars().collect();
        let mut word = String::new();
        for (i, &c) in chars.iter().enumerate() {
            let boundary = i > 0
                && c.is_ascii_uppercase()
                && (!chars[i - 1].is_ascii_uppercase()
                    || chars.get(i + 1).is_some_and(char::is_ascii_lowercase));
            if boundary && !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            word.push(c.to_ascii_lowercase());
        }
        if !word.is_empty() {
            words.push(word);
        }
    }
    words
}

/// 由名字派生占位符变量名：env 键本身合法就原样用，其余转成大写下划线。
fn var_name(name: &str) -> Option<String> {
    if is_var_name(name) && name.chars().all(|c| !c.is_ascii_lowercase()) {
        return Some(name.to_owned());
    }
    let words = split_words(name);
    if words.is_empty() {
        return None;
    }
    let joined = words.join("_").to_ascii_uppercase();
    Some(if joined.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{joined}")
    } else {
        joined
    })
}

/// 值本身像凭据：Bearer/Basic 认证串、带密码的 URL、常见服务的密钥前缀、JWT。
/// 名字看不出来的（DATABASE_URL、位置参数里的 Key）靠这一层兜底。
fn looks_like_secret(value: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "sk-",
        "sk_live_",
        "sk_test_",
        "rk_live_",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "glpat-",
        "xoxb-",
        "xoxp-",
        "xoxa-",
        "xoxs-",
        "xapp-",
        "AKIA",
        "ASIA",
        "AIza",
        "ya29.",
        "fc-",
        "tvly-",
        "pplx-",
        "gsk_",
        "hf_",
        "r8_",
        "npm_",
        "pcsk_",
        "sntrys_",
        "lin_api_",
        "ntn_",
        "secret_",
        "shpat_",
        "dop_v1_",
        "glsa_",
        "SG.",
    ];
    let value = value.trim();
    if value.is_empty() || is_placeholder_value(value) {
        return false;
    }
    let (scheme, credential) = split_scheme(value);
    if matches!(
        scheme.trim().to_ascii_lowercase().as_str(),
        "bearer" | "basic"
    ) && !credential.trim().is_empty()
    {
        return true;
    }
    if url_has_password(value) {
        return true;
    }
    if value.chars().any(char::is_whitespace) {
        return false;
    }
    // 前缀之后至少 12 位，避免 `fc-` 之类的短前缀误伤普通参数。
    PREFIXES
        .iter()
        .any(|prefix| value.len() >= prefix.len() + 12 && value.starts_with(prefix))
        || (value.starts_with("eyJ") && value.len() > 30 && value.split('.').count() == 3)
}

/// 认证方案前缀（含其后空格）与凭据本体；没有方案前缀时前者为空串。
fn split_scheme(value: &str) -> (&str, &str) {
    const SCHEMES: &[&str] = &["bearer", "basic", "token", "bot"];
    if let Some((scheme, rest)) = value.split_once(' ')
        && SCHEMES.contains(&scheme.to_ascii_lowercase().as_str())
    {
        let credential = rest.trim_start();
        return (&value[..value.len() - credential.len()], credential);
    }
    ("", value)
}

fn is_url(value: &str) -> bool {
    value.split_once("://").is_some_and(|(scheme, _)| {
        !scheme.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}

fn url_has_password(value: &str) -> bool {
    let Some((_, rest)) = value.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    authority
        .rsplit_once('@')
        .and_then(|(userinfo, _)| userinfo.split_once(':'))
        .is_some_and(|(_, password)| !password.is_empty() && !is_pure_placeholder(password))
}

fn is_pure_placeholder(value: &str) -> bool {
    value
        .trim()
        .strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
        .is_some_and(is_var_name)
}

/// 已经是占位符（可带认证方案前缀，如 `Bearer ${TOKEN}`）的值不再替换。
fn is_placeholder_value(value: &str) -> bool {
    is_pure_placeholder(split_scheme(value).1)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::collections::BTreeMap;

    use mcpane_storage::{McpServerDef, ServerTransport};

    use super::{RedactedField, build_export, is_sensitive_name, split_words, write_new_file};
    use crate::commands::import::{parse_mcp_servers_json, placeholders_of};

    fn def(name: &str, transport: ServerTransport) -> McpServerDef {
        McpServerDef {
            id: format!("srv_{name}"),
            slug: mcpane_hub_protocol::normalize_slug(name),
            name: name.into(),
            transport,
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            url: None,
            bearer: None,
            bearer_env: None,
            headers: BTreeMap::new(),
            runtime_policy: "shared".into(),
            enabled: true,
        }
    }

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// 不含凭据的配置导出再导入，每个字段都要原样回来（含停用状态与 SSE 传输）。
    #[test]
    fn export_then_import_round_trips_every_field() {
        let mut stdio = def("memory", ServerTransport::Stdio);
        stdio.command = Some("npx".into());
        stdio.args = vec!["-y".into(), "@modelcontextprotocol/server-memory".into()];
        stdio.env = map(&[("MEMORY_FILE_PATH", "/tmp/memory.json")]);
        stdio.cwd = Some("/tmp".into());
        let mut http = def("Remote MCP", ServerTransport::Http);
        http.url = Some("https://example.com/mcp".into());
        http.bearer_env = Some("MY_MCP_TOKEN".into());
        http.headers = map(&[("X-Region", "us")]);
        let mut sse = def("legacy", ServerTransport::Sse);
        sse.url = Some("https://example.com/sse".into());
        sse.enabled = false;
        let originals = vec![stdio, http, sse];

        let export = build_export(&originals).unwrap();
        assert!(export.redactions.is_empty(), "{:?}", export.redactions);
        let parsed = parse_mcp_servers_json(&export.json).unwrap();
        assert!(parsed.invalid.is_empty());
        assert_eq!(parsed.defs.len(), originals.len());
        for (back, original) in parsed.defs.iter().zip(&originals) {
            assert_eq!(back.name, original.name);
            assert_eq!(back.slug, original.slug);
            assert_eq!(back.transport, original.transport);
            assert_eq!(back.command, original.command);
            assert_eq!(back.args, original.args);
            assert_eq!(back.env, original.env);
            assert_eq!(back.cwd, original.cwd);
            assert_eq!(back.url, original.url);
            assert_eq!(back.bearer, original.bearer);
            assert_eq!(back.bearer_env, original.bearer_env);
            assert_eq!(back.headers, original.headers);
            assert_eq!(back.enabled, original.enabled);
        }
        // 远程条目带 type，贴进 Claude Code 才认得出是 HTTP。
        let value: serde_json::Value = serde_json::from_str(&export.json).unwrap();
        assert_eq!(value["mcpServers"]["Remote MCP"]["type"], "http");
        assert_eq!(value["mcpServers"]["legacy"]["type"], "sse");
    }

    /// 各种位置的凭据都换成占位符；明文一个字节都不能出现在导出文本里。
    #[test]
    fn export_replaces_credentials_everywhere() {
        const SECRETS: [&str; 10] = [
            "ghp_0123456789abcdefABCDEF",
            "pg-secret-pass",
            "bearer-direct-value",
            "hdr-api-key-value",
            "auth-header-value",
            "exa-query-key-value",
            "fc-0123456789abcdef0123",
            "sk-flag-0123456789abcdef",
            "inline-token-value",
            "sk-ant-0123456789abcdef",
        ];
        let mut stdio = def("tools", ServerTransport::Stdio);
        stdio.command = Some("npx".into());
        stdio.args = vec![
            "-y".into(),
            "@upstash/context7-mcp".into(),
            "--max-old-space-size=4096".into(),
            "--api-key".into(),
            SECRETS[7].into(),
            format!("--token={}", SECRETS[8]),
            "-e".into(),
            format!("GITHUB_TOKEN={}", SECRETS[0]),
            "--header".into(),
            format!("Authorization: Bearer {}", SECRETS[9]),
            "--no-auth".into(),
            "server.js".into(),
            "C:\\data\\mcp".into(),
            "localhost:8080".into(),
        ];
        stdio.env = map(&[
            ("GITHUB_PERSONAL_ACCESS_TOKEN", SECRETS[0]),
            (
                "DATABASE_URL",
                &format!("postgres://app:{}@db.local/app", SECRETS[1]),
            ),
            ("MEMORY_FILE_PATH", "/tmp/memory.json"),
        ]);
        let mut http = def("remote", ServerTransport::Http);
        http.url = Some(format!(
            "https://mcp.example.com/{}/v2/mcp?exaApiKey={}&region=us",
            SECRETS[6], SECRETS[5]
        ));
        http.bearer = Some(SECRETS[2].into());
        http.headers = map(&[
            ("X-API-Key", SECRETS[3]),
            ("Authorization", &format!("Bearer {}", SECRETS[4])),
            ("X-Region", "us"),
        ]);

        let export = build_export(&[stdio, http]).unwrap();
        for secret in SECRETS {
            assert!(
                !export.json.contains(secret),
                "{secret} 泄漏：\n{}",
                export.json
            );
        }
        let value: serde_json::Value = serde_json::from_str(&export.json).unwrap();
        let tools = &value["mcpServers"]["tools"];
        assert_eq!(
            tools["args"],
            serde_json::json!([
                "-y",
                "@upstash/context7-mcp",
                "--max-old-space-size=4096",
                "--api-key",
                "${API_KEY}",
                "--token=${TOKEN}",
                "-e",
                "GITHUB_TOKEN=${GITHUB_TOKEN}",
                "--header",
                "Authorization: Bearer ${AUTHORIZATION}",
                "--no-auth",
                "server.js",
                "C:\\data\\mcp",
                "localhost:8080"
            ])
        );
        assert_eq!(
            tools["env"]["GITHUB_PERSONAL_ACCESS_TOKEN"],
            "${GITHUB_PERSONAL_ACCESS_TOKEN}"
        );
        // 名字看不出来的 URL 只换密码，主机与库名留给收件人看。
        assert_eq!(
            tools["env"]["DATABASE_URL"],
            "postgres://app:${PASSWORD}@db.local/app"
        );
        assert_eq!(tools["env"]["MEMORY_FILE_PATH"], "/tmp/memory.json");

        let remote = &value["mcpServers"]["remote"];
        assert_eq!(
            remote["url"],
            "https://mcp.example.com/${API_KEY}/v2/mcp?exaApiKey=${EXA_API_KEY}&region=us"
        );
        assert_eq!(remote["bearer"], "${BEARER_TOKEN}");
        assert_eq!(remote["headers"]["X-API-Key"], "${X_API_KEY}");
        assert_eq!(
            remote["headers"]["Authorization"],
            "Bearer ${AUTHORIZATION}"
        );
        assert_eq!(remote["headers"]["X-Region"], "us");
    }

    /// 导出登记的替换与导入侧扫出的待填变量必须一一对应——两半各写各的就会漂移。
    #[test]
    fn redactions_match_what_import_asks_for() {
        let mut server = def("GitHub", ServerTransport::Stdio);
        server.command = Some("npx".into());
        // 原有占位符要保留，新占位符不能和它撞名。
        server.env = map(&[
            ("TOKEN", "${API_KEY}"),
            ("API_KEY", "ghp_0123456789abcdefABCDEF"),
        ]);
        let export = build_export(&[server]).unwrap();
        let variables: Vec<&str> = export
            .redactions
            .iter()
            .map(|r| r.variable.as_str())
            .collect();
        assert_eq!(variables, vec!["API_KEY_2"]);
        assert_eq!(export.redactions[0].field, RedactedField::Env);

        let back = parse_mcp_servers_json(&export.json).unwrap().defs;
        let mut asked = placeholders_of(&back[0]);
        asked.sort();
        assert_eq!(asked, vec!["API_KEY", "API_KEY_2"]);
    }

    /// 改过名的两个 Server 可能同名，JSON 键要去重而不是互相覆盖；
    /// 替换明细按去重后的键登记，导出框里才分得清是哪一个。
    #[test]
    fn duplicate_names_get_distinct_keys() {
        let mut a = def("Docs", ServerTransport::Stdio);
        a.command = Some("a".into());
        a.env = map(&[("API_KEY", "key-a")]);
        let mut b = def("Docs", ServerTransport::Stdio);
        b.command = Some("b".into());
        b.env = map(&[("API_KEY", "key-b")]);
        let export = build_export(&[a, b]).unwrap();
        let value: serde_json::Value = serde_json::from_str(&export.json).unwrap();
        assert_eq!(value["mcpServers"]["Docs"]["command"], "a");
        assert_eq!(value["mcpServers"]["Docs (2)"]["command"], "b");
        let servers: Vec<&str> = export
            .redactions
            .iter()
            .map(|r| r.server.as_str())
            .collect();
        assert_eq!(servers, vec!["Docs", "Docs (2)"]);
    }

    /// bearerEnv 指向的 env 值就是 Bearer Token：名字再普通也要替换。
    #[test]
    fn env_behind_bearer_env_is_always_replaced() {
        let mut remote = def("remote", ServerTransport::Http);
        remote.url = Some("https://x.dev/mcp".into());
        remote.bearer_env = Some("MCP_ACCESS".into());
        remote.env = map(&[("MCP_ACCESS", "a1b2c3d4e5f6"), ("REGION", "us")]);
        let export = build_export(&[remote]).unwrap();
        assert!(!export.json.contains("a1b2c3d4e5f6"), "{}", export.json);
        let value: serde_json::Value = serde_json::from_str(&export.json).unwrap();
        let entry = &value["mcpServers"]["remote"];
        assert_eq!(entry["env"]["MCP_ACCESS"], "${MCP_ACCESS}");
        assert_eq!(entry["env"]["REGION"], "us");
        assert_eq!(entry["bearerEnv"], "MCP_ACCESS");
    }

    /// `--flag=value` 的取值也要按独立参数的写法再认一遍；开关字面量不当凭据。
    #[test]
    fn flag_values_are_inspected_and_switches_kept() {
        let mut stdio = def("remote-bridge", ServerTransport::Stdio);
        stdio.command = Some("npx".into());
        stdio.args = vec![
            "mcp-remote".into(),
            "--header=Authorization: Bearer abc123def".into(),
            "--env=API_TOKEN=xyz789".into(),
            "--disable-auth=true".into(),
        ];
        stdio.env = map(&[
            ("SKIP_AUTH", "true"),
            (
                "SLACK_WEBHOOK_URL",
                "https://hooks.slack.com/services/T000/B000/XXXXXXXX",
            ),
        ]);
        let export = build_export(&[stdio]).unwrap();
        for leaked in ["abc123def", "xyz789", "XXXXXXXX"] {
            assert!(
                !export.json.contains(leaked),
                "{leaked} 泄漏：\n{}",
                export.json
            );
        }
        let value: serde_json::Value = serde_json::from_str(&export.json).unwrap();
        let entry = &value["mcpServers"]["remote-bridge"];
        assert_eq!(
            entry["args"],
            serde_json::json!([
                "mcp-remote",
                "--header=Authorization: Bearer ${AUTHORIZATION}",
                "--env=API_TOKEN=${API_TOKEN}",
                "--disable-auth=true"
            ])
        );
        assert_eq!(entry["env"]["SKIP_AUTH"], "true");
        assert_eq!(entry["env"]["SLACK_WEBHOOK_URL"], "${SLACK_WEBHOOK_URL}");
    }

    #[test]
    fn sensitive_names() {
        for name in [
            "GITHUB_PERSONAL_ACCESS_TOKEN",
            "BRAVE_API_KEY",
            "OPENAI_KEY",
            "AWS_SECRET_ACCESS_KEY",
            "PGPASSWORD",
            "DB_PASS",
            "SENTRY_DSN",
            "Authorization",
            "X-API-Key",
            "x-goog-api-key",
            "Cookie",
            "exaApiKey",
            "accessToken",
            "PRIVATEKEY",
            "GITHUB_PAT",
            "X-Auth",
        ] {
            assert!(is_sensitive_name(name), "{name} 应识别为凭据");
        }
        for name in [
            "MEMORY_FILE_PATH",
            "MAX_TOKENS",
            "AUTH_TYPE",
            "NEXTAUTH_URL",
            "CLIENT_ID",
            "KEYBOARD_LAYOUT",
            "PWD",
            "X-Region",
            "",
        ] {
            assert!(!is_sensitive_name(name), "{name} 不应识别为凭据");
        }
    }

    #[test]
    fn split_words_handles_camel_and_acronyms() {
        assert_eq!(split_words("APIKey"), vec!["api", "key"]);
        assert_eq!(split_words("exaApiKey"), vec!["exa", "api", "key"]);
        assert_eq!(split_words("X-API-Key"), vec!["x", "api", "key"]);
        assert_eq!(split_words("GITHUB_TOKEN"), vec!["github", "token"]);
    }

    /// 同名文件存在时追加序号，绝不覆盖用户已有的文件。
    #[test]
    fn write_new_file_never_overwrites() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("mcpane-export-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        let first = write_new_file(&dir, "mcpane-servers", "one").unwrap();
        let second = write_new_file(&dir, "mcpane-servers", "two").unwrap();
        assert_eq!(first.file_name().unwrap(), "mcpane-servers.json");
        assert_eq!(second.file_name().unwrap(), "mcpane-servers (2).json");
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "one");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "two");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
