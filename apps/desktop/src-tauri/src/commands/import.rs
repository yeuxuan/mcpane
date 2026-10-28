//! 从客户端 / 粘贴的 JSON 导入 MCP 配置（TASK-503）。

use std::collections::BTreeMap;
use std::path::PathBuf;

use mcpane_config::ConfigFormat;

use super::connect::user_home;
use super::servers::server_repo;
use mcpane_hub_protocol::normalize_slug;

// ---------- 从客户端导入 MCP 配置（TASK-503） ----------

#[derive(serde::Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ImportSummary {
    pub imported: Vec<String>,
    pub skipped: Vec<String>,
    /// 非致命异常（某份配置读取/解析失败等）。此前这些情况被静默跳过，
    /// 用户只看到「导入 0 条」而无从归因。Hub 重载在后台执行，不进此通道。
    pub warnings: Vec<String>,
    /// 已入库但仍有 `${VAR}` 占位符没填、按停用导入的服务器（只有 JSON 导入会产生）。
    pub incomplete: Vec<String>,
}

/// 读取一份客户端配置并取出 servers map。
/// 文件不存在是常态（客户端未安装），不告警；读失败/解析失败/形状不对
/// 记入 warnings 后跳过该客户端。
fn read_servers_map(
    label: &str,
    file: &std::path::Path,
    format: ConfigFormat,
    key: &str,
    warnings: &mut Vec<String>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let content = match std::fs::read_to_string(file) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            warnings.push(format!("{label}：读取 {} 失败：{e}", file.display()));
            return None;
        }
    };
    let doc = match mcpane_config::ConfigDocument::parse(&content, format) {
        Ok(doc) => doc,
        Err(e) => {
            warnings.push(format!("{label}：配置解析失败，已跳过：{e}"));
            return None;
        }
    };
    match doc.get(&[key]) {
        Some(serde_json::Value::Object(servers)) => Some(servers),
        Some(_) => {
            warnings.push(format!("{label}：{key} 字段不是对象，已跳过"));
            None
        }
        // 键不存在 = 该客户端没配任何 MCP server，常态。
        None => None,
    }
}

/// 从 Claude Code 与 Codex 的现有配置导入 MCP Server 定义。
/// 跳过 `mcpane` 自身与已存在的同名 slug；导入后通知 Hub reload。
#[tauri::command]
pub async fn import_from_clients(app: tauri::AppHandle) -> Result<ImportSummary, String> {
    let mut candidates: Vec<mcpane_storage::McpServerDef> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // Claude：~/.claude.json 顶层 mcpServers。
    let claude_paths = mcpane_adapter_claude::discover_config_paths(&user_home(), None);
    if let Some(servers) = read_servers_map(
        "Claude Code",
        &claude_paths.user_config,
        ConfigFormat::Json,
        "mcpServers",
        &mut warnings,
    ) {
        for (name, entry) in servers {
            if name == "mcpane" {
                continue;
            }
            if let Some(def) = json_entry_to_def(&name, &entry) {
                candidates.push(def);
            }
        }
    }

    // Codex：config.toml 的 [mcp_servers.*]。
    let codex_home = std::env::var("CODEX_HOME").ok().map(PathBuf::from);
    let codex_paths =
        mcpane_adapter_codex::discover_config_paths(&user_home(), codex_home.as_deref(), None);
    if let Some(servers) = read_servers_map(
        "Codex",
        &codex_paths.user_config,
        ConfigFormat::Toml,
        "mcp_servers",
        &mut warnings,
    ) {
        for (name, entry) in servers {
            if name == "mcpane" {
                continue;
            }
            // Codex 的 bearer_token_env_var 由 json_entry_to_def 统一识别。
            if let Some(def) = json_entry_to_def(&name, &entry) {
                candidates.push(def);
            }
        }
    }

    // Gemini CLI：~/.gemini/settings.json 顶层 mcpServers。
    // 传输语义靠字段名（无 type 字段）：httpUrl=Streamable HTTP（归一在
    // json_entry_to_def）、裸 url=SSE（Gemini 特有语义，这里覆盖）、command=stdio。
    let gemini_paths = mcpane_adapter_gemini::discover_config_paths(&user_home());
    if let Some(servers) = read_servers_map(
        "Gemini CLI",
        &gemini_paths.settings,
        ConfigFormat::Json,
        "mcpServers",
        &mut warnings,
    ) {
        for (name, entry) in servers {
            if name == "mcpane" {
                continue;
            }
            if let Some(mut def) = json_entry_to_def(&name, &entry) {
                if entry.get("httpUrl").is_none()
                    && entry.get("url").is_some()
                    && def.transport == mcpane_storage::ServerTransport::Http
                {
                    def.transport = mcpane_storage::ServerTransport::Sse;
                }
                candidates.push(def);
            }
        }
    }

    import_defs(&app, candidates, warnings).await
}

/// 导入面板的预览：逐条列出解析结果、前缀冲突与待补填的占位符，不落库。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub entries: Vec<ImportPreviewEntry>,
    /// 既无 command 也无 url、认不出传输方式的条目名。
    pub invalid: Vec<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreviewEntry {
    pub server: mcpane_storage::McpServerDef,
    /// 库里已占用同一工具前缀的服务器名；None = 可导入。批内互撞取决于勾选，由前端随勾选判定。
    pub conflict: Option<String>,
    /// 待补填的 `${VAR}` 占位符变量名。
    pub variables: Vec<String>,
}

/// 解析用户粘贴/选择的 JSON，供导入面板逐条确认。
#[tauri::command]
pub async fn servers_import_preview(json: String) -> Result<ImportPreview, String> {
    let parsed = parse_mcp_servers_json(&json)?;
    let existing = server_repo()
        .await?
        .list()
        .await
        .map_err(|e| e.to_string())?;
    Ok(build_preview(parsed, &existing))
}

/// 导入预览里勾选的条目（`selected` 为条目名，`values` 按条目名给占位符取值）。
/// 仍有占位符没填的按停用入库：带着 `${VAR}` 字面量跑起来只会换来看不出原因的鉴权失败。
#[tauri::command]
pub async fn servers_import_json(
    app: tauri::AppHandle,
    json: String,
    selected: Vec<String>,
    values: BTreeMap<String, BTreeMap<String, String>>,
) -> Result<ImportSummary, String> {
    let parsed = parse_mcp_servers_json(&json)?;
    let (candidates, incomplete) = select_and_fill(parsed.defs, &selected, &values);
    if candidates.is_empty() {
        return Err("没有选中可导入的服务器".into());
    }
    let mut summary = import_defs(&app, candidates, Vec::new()).await?;
    summary.incomplete = incomplete
        .into_iter()
        .filter(|name| summary.imported.contains(name))
        .collect();
    Ok(summary)
}

pub(super) struct ParsedServers {
    pub(super) defs: Vec<mcpane_storage::McpServerDef>,
    pub(super) invalid: Vec<String>,
}

/// 解析 mcpServers JSON 文本为 Server 定义列表（纯函数，便于单测）。
/// 支持 `{"mcpServers": {...}}` 包裹层与裸 `{"名称": {...}}` map 两种形态。
pub(super) fn parse_mcp_servers_json(json: &str) -> Result<ParsedServers, String> {
    let value: serde_json::Value =
        serde_json::from_str(json.trim()).map_err(|e| format!("JSON 解析失败: {e}"))?;
    let servers = match value.get("mcpServers") {
        Some(serde_json::Value::Object(map)) => map,
        Some(_) => return Err("mcpServers 字段应为对象".into()),
        None => match &value {
            serde_json::Value::Object(map) => map,
            _ => return Err("顶层应为 JSON 对象".into()),
        },
    };
    let mut parsed = ParsedServers {
        defs: Vec::new(),
        invalid: Vec::new(),
    };
    // mcpane 是 Hub 自己的入口（客户端配置里常见），不是下游，静默略过。
    for (name, entry) in servers.iter().filter(|(name, _)| name.as_str() != "mcpane") {
        match json_entry_to_def(name, entry) {
            Some(def) => parsed.defs.push(def),
            None => parsed.invalid.push(name.clone()),
        }
    }
    Ok(parsed)
}

fn build_preview(
    parsed: ParsedServers,
    existing: &[mcpane_storage::McpServerDef],
) -> ImportPreview {
    let owners: std::collections::HashMap<&str, &str> = existing
        .iter()
        .map(|def| (def.slug.as_str(), def.name.as_str()))
        .collect();
    let entries = parsed
        .defs
        .into_iter()
        .map(|server| {
            let conflict = owners
                .get(server.slug.as_str())
                .map(|owner| (*owner).to_owned());
            let variables = placeholders_of(&server);
            ImportPreviewEntry {
                server,
                conflict,
                variables,
            }
        })
        .collect();
    ImportPreview {
        entries,
        invalid: parsed.invalid,
    }
}

/// 取出勾选的条目并代入占位符取值；返回候选与「仍缺取值」的条目名。
fn select_and_fill(
    defs: Vec<mcpane_storage::McpServerDef>,
    selected: &[String],
    values: &BTreeMap<String, BTreeMap<String, String>>,
) -> (Vec<mcpane_storage::McpServerDef>, Vec<String>) {
    let no_values = BTreeMap::new();
    let mut incomplete = Vec::new();
    let candidates = defs
        .into_iter()
        .filter(|def| selected.contains(&def.name))
        .map(|mut def| {
            let entry_values = values.get(&def.name).unwrap_or(&no_values);
            if !fill_placeholders(&mut def, entry_values) {
                def.enabled = false;
                incomplete.push(def.name.clone());
            }
            def
        })
        .collect();
    (candidates, incomplete)
}

/// 字符串里的 `${VAR}` 占位符变量名（VAR 须是合法环境变量名；与 Claude Code
/// `.mcp.json` 的变量展开同一写法，MCPane 导出时也用它替换凭据）。
pub(super) fn placeholder_names(text: &str) -> Vec<&str> {
    placeholder_spans(text)
        .into_iter()
        .map(|(_, name)| name)
        .collect()
}

/// 每个占位符在原文中的字节区间（含 `${` 与 `}`）与变量名。
fn placeholder_spans(text: &str) -> Vec<(std::ops::Range<usize>, &str)> {
    let mut spans = Vec::new();
    let mut offset = 0;
    while let Some(found) = text[offset..].find("${") {
        let start = offset + found;
        let name_start = start + 2;
        match text[name_start..].find('}') {
            Some(len) if is_var_name(&text[name_start..name_start + len]) => {
                let end = name_start + len + 1;
                spans.push((start..end, &text[name_start..name_start + len]));
                offset = end;
            }
            _ => offset = name_start,
        }
    }
    spans
}

pub(super) fn is_var_name(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 可能承载占位符的字符串字段。env / headers 只看值：键是名字，不是取值。
fn string_fields(def: &mcpane_storage::McpServerDef) -> impl Iterator<Item = &String> {
    def.command
        .iter()
        .chain(&def.args)
        .chain(def.env.values())
        .chain(&def.cwd)
        .chain(&def.url)
        .chain(&def.bearer)
        .chain(def.headers.values())
}

fn string_fields_mut(def: &mut mcpane_storage::McpServerDef) -> impl Iterator<Item = &mut String> {
    def.command
        .iter_mut()
        .chain(&mut def.args)
        .chain(def.env.values_mut())
        .chain(&mut def.cwd)
        .chain(&mut def.url)
        .chain(&mut def.bearer)
        .chain(def.headers.values_mut())
}

/// 条目里的占位符变量名（按出现顺序去重）。
pub(super) fn placeholders_of(def: &mcpane_storage::McpServerDef) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for text in string_fields(def) {
        for name in placeholder_names(text) {
            if !names.iter().any(|seen| seen == name) {
                names.push(name.to_owned());
            }
        }
    }
    names
}

/// 代入占位符取值（空白取值视同未填）；返回是否已全部填上。
/// 是否填全按代入前判断：用户的取值里恰好含 `${…}` 字面量，不能算没填。
fn fill_placeholders(
    def: &mut mcpane_storage::McpServerDef,
    values: &BTreeMap<String, String>,
) -> bool {
    let filled = |name: &str| {
        values
            .get(name)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
    };
    let complete = placeholders_of(def)
        .iter()
        .all(|name| filled(name).is_some());
    for text in string_fields_mut(def) {
        // 单次扫描拼接：代入的值不会被下一个变量再替换一轮。
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for (span, name) in placeholder_spans(text) {
            out.push_str(&text[last..span.start]);
            out.push_str(filled(name).unwrap_or(&text[span.clone()]));
            last = span.end;
        }
        out.push_str(&text[last..]);
        *text = out;
    }
    complete
}

/// 按 slug 去重后批量入库，导入过则通知 Hub reload。
pub(super) async fn import_defs(
    app: &tauri::AppHandle,
    candidates: Vec<mcpane_storage::McpServerDef>,
    warnings: Vec<String>,
) -> Result<ImportSummary, String> {
    let repo = server_repo().await?;
    // 可变：本批已认领的 slug 也要计入。此前这里是只读快照，同一批里两个条目
    // 撞 slug 会双双入库（id 为空 → 各生成新行），而 slug 是工具前缀的来源——
    // Catalog 构建时后者的**全部**工具会因暴露名冲突被静默跳过，用户看到的是
    // 一张「0 个工具」且没有任何错因的卡片。手动新建走 server_upsert 有这道
    // 校验，导入路径此前绕开了它。
    //
    // 撞车来源都很常规：Claude 配了 `context7`、Codex 配了 `Context7`；
    // 粘贴的 JSON 里同时有 `my-server` 与 `My Server`。
    let mut existing: std::collections::HashSet<String> = repo
        .list()
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|d| d.slug)
        .collect();

    let (accepted, skipped) = partition_by_slug(candidates, &mut existing);
    let mut summary = ImportSummary {
        skipped,
        ..ImportSummary::default()
    };
    for def in accepted {
        repo.upsert(def.clone()).await.map_err(|e| e.to_string())?;
        summary.imported.push(def.name);
    }
    if !summary.imported.is_empty() {
        crate::auto_sync::notify_changed();
        // Hub 重载放后台：ReloadServers 串行重连下游（单个超时 10s），同步等它
        // 会让导入界面卡住整个启动期。启动进度由服务器页轮询诊断呈现（启动中→
        // 运行中/失败），通知失败时新下游同样以「启动中不转绿」可见，这里只记日志。
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = super::hub::notify_hub_reload(&app).await {
                eprintln!("[mcpane] 导入后通知 Hub 重载失败：{error}");
            }
        });
    }
    summary.warnings = warnings;
    Ok(summary)
}

/// 按 slug 把候选划成「可入库」与「跳过（名字）」两拨。`taken` 传入时是库里
/// 已有的 slug 集，返回时已并入本批认领的——**批内互撞也算撞**，这正是纯函数
/// 抽出来的原因（跑不到 DB 的单测里也能验）。
fn partition_by_slug(
    candidates: Vec<mcpane_storage::McpServerDef>,
    taken: &mut std::collections::HashSet<String>,
) -> (Vec<mcpane_storage::McpServerDef>, Vec<String>) {
    let mut accepted = Vec::new();
    let mut skipped = Vec::new();
    for def in candidates {
        // insert 返回 false = 该 slug 已被占（库里已有，或本批前面的条目刚认领）。
        if taken.insert(def.slug.clone()) {
            accepted.push(def);
        } else {
            skipped.push(def.name);
        }
    }
    (accepted, skipped)
}

/// 通用条目 → Server 定义（Claude JSON 与 Codex TOML 结构基本同构）。
pub(super) fn json_entry_to_def(
    name: &str,
    entry: &serde_json::Value,
) -> Option<mcpane_storage::McpServerDef> {
    // 空白字符串视同缺失：放行的话错误会推迟到 Hub spawn 时才炸。
    let text = |key: &str| {
        entry
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
    };
    // Gemini CLI 的 Streamable HTTP 字段是 httpUrl（该字段只在 Gemini 配置
    // 出现，语义恒 Streamable HTTP，可无歧义归一）。
    let http_url = text("httpUrl");
    let url = http_url.or_else(|| text("url"));
    let command = text("command");
    // 客户端配置里 SSE 远程 server 通常显式标注 "type"/"transport": "sse"
    // （Claude Code / Cline 等的写法）；未标注的远程 URL 一律按 Streamable HTTP。
    let declared_type = text("type").or_else(|| text("transport"));
    let (transport, runtime_policy) = if url.is_some() {
        let transport = if http_url.is_none() && declared_type == Some("sse") {
            mcpane_storage::ServerTransport::Sse
        } else {
            mcpane_storage::ServerTransport::Http
        };
        (transport, "shared")
    } else if command.is_some() {
        // Hub 侧 runtime_policy 尚未实现 per-client；导入一律 shared。
        (mcpane_storage::ServerTransport::Stdio, "shared")
    } else {
        return None;
    };
    let args: Vec<String> = entry
        .get("args")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let string_map = |key: &str| -> std::collections::BTreeMap<String, String> {
        entry
            .get(key)
            .and_then(|v| v.as_object())
            .map(|map| {
                map.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                    .collect()
            })
            .unwrap_or_default()
    };
    let env = string_map("env");
    // Claude Code 远程配置的自定义请求头（如 Authorization / X-API-Key）。
    let headers = string_map("headers");
    let cwd = text("cwd").map(str::to_owned);
    // 远程认证：MCPane 的导出 / 复制配置写 bearer、bearerEnv，Codex 写
    // bearer_token_env_var。此前一律丢弃，复制出去再粘回来 Token 配置就没了。
    let remote = |value: Option<&str>| value.filter(|_| url.is_some()).map(str::to_owned);
    let bearer = remote(text("bearer"));
    let bearer_env = remote(
        text("bearerEnv")
            .or_else(|| text("bearer_env"))
            .or_else(|| text("bearer_token_env_var")),
    );
    let enabled = entry
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    Some(mcpane_storage::McpServerDef {
        id: String::new(),
        slug: normalize_slug(name),
        name: name.to_owned(),
        transport,
        command: command.map(str::to_owned),
        args,
        env,
        cwd,
        url: url.map(str::to_owned),
        bearer,
        bearer_env,
        headers,
        runtime_policy: runtime_policy.to_owned(),
        enabled,
    })
}
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::collections::BTreeMap;

    use super::{
        build_preview, parse_mcp_servers_json, partition_by_slug, placeholder_names,
        select_and_fill,
    };

    /// 同一批候选里两个条目撞 slug 时，只能进去一个。
    ///
    /// 撞车是常态而非边角：slug 只保留 ASCII 字母数字并统一小写，`Context7`
    /// 与 `context7`（Claude 与 Codex 各写各的大小写）、`my-server` 与
    /// `my_server` 都会归一到同一个值。两条都入库的话，slug 是工具前缀的来源，
    /// Catalog 构建会因暴露名冲突把后者的**全部**工具静默跳过——用户看到一张
    /// 「0 个工具」且没有任何错因的卡片。
    #[test]
    fn same_batch_slug_collision_keeps_only_the_first() {
        let json = r#"{
          "context7": { "command": "npx", "args": ["-y", "@upstash/context7-mcp"] },
          "Context7": { "command": "npx", "args": ["-y", "other"] },
          "memory": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-memory"] }
        }"#;
        let defs = parse_mcp_servers_json(json).unwrap().defs;
        assert_eq!(defs.len(), 3, "前提：解析阶段三条都在");
        assert_eq!(
            defs.iter().filter(|d| d.slug == "context7").count(),
            2,
            "前提：两条确实归一到同一个 slug（否则这个测试测不到东西）"
        );

        let mut taken = std::collections::HashSet::new();
        let (accepted, skipped) = partition_by_slug(defs, &mut taken);
        assert_eq!(accepted.len(), 2);
        assert_eq!(skipped, vec!["Context7"], "后到的那条被跳过并报给用户");
        let slugs: Vec<&str> = accepted.iter().map(|d| d.slug.as_str()).collect();
        assert_eq!(slugs, vec!["context7", "memory"], "无关条目不受牵连");
    }

    /// 库里已有的 slug 仍然照常跳过（原有契约不能被批内去重改掉）。
    #[test]
    fn slug_already_in_db_is_still_skipped() {
        let json = r#"{ "memory": { "command": "npx" } }"#;
        let defs = parse_mcp_servers_json(json).unwrap().defs;
        let mut taken = std::collections::HashSet::from(["memory".to_owned()]);
        let (accepted, skipped) = partition_by_slug(defs, &mut taken);
        assert!(accepted.is_empty());
        assert_eq!(skipped, vec!["memory"]);
    }

    #[test]
    fn parse_wrapped_mcp_servers_with_env() {
        let json = r#"{
          "mcpServers": {
            "puppeteer": {
              "command": "npx",
              "args": ["-y", "@modelcontextprotocol/server-puppeteer"],
              "env": { "PUPPETEER_LAUNCH_OPTIONS": "{ \"headless\": false }" }
            }
          }
        }"#;
        let defs = parse_mcp_servers_json(json).unwrap().defs;
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "puppeteer");
        assert_eq!(defs[0].command.as_deref(), Some("npx"));
        assert_eq!(defs[0].args.len(), 2);
        assert_eq!(
            defs[0]
                .env
                .get("PUPPETEER_LAUNCH_OPTIONS")
                .map(String::as_str),
            Some("{ \"headless\": false }")
        );
    }

    #[test]
    fn parse_bare_map_and_http_entry() {
        let json = r#"{
          "context7": { "command": "npx", "args": ["-y", "@upstash/context7-mcp"] },
          "remote": { "url": "https://example.com/mcp" }
        }"#;
        let defs = parse_mcp_servers_json(json).unwrap().defs;
        assert_eq!(defs.len(), 2);
        assert!(
            defs.iter()
                .any(|d| d.url.as_deref() == Some("https://example.com/mcp"))
        );
    }

    #[test]
    fn parse_skips_mcpane_and_invalid_entries() {
        let json = r#"{
          "mcpServers": {
            "mcpane": { "command": "mcpane" },
            "broken": { "note": "既无 command 也无 url" }
          }
        }"#;
        let parsed = parse_mcp_servers_json(json).unwrap();
        assert!(parsed.defs.is_empty());
        // 认不出的条目要报给预览；mcpane 是 Hub 自身入口，不算无效。
        assert_eq!(parsed.invalid, vec!["broken"]);
    }

    #[test]
    fn parse_gemini_http_url_field_as_streamable_http() {
        // Gemini CLI 的 Streamable HTTP 条目用 httpUrl 字段（无 type），
        // 必须归一为 Http 传输并保留 headers。
        let json = r#"{
          "mcpServers": {
            "gemini-remote": {
              "httpUrl": "http://127.0.0.1:24860/mcp",
              "headers": { "Authorization": "Bearer $MCPANE_GEMINI_TOKEN" }
            }
          }
        }"#;
        let defs = parse_mcp_servers_json(json).unwrap().defs;
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].transport, mcpane_storage::ServerTransport::Http);
        assert_eq!(defs[0].url.as_deref(), Some("http://127.0.0.1:24860/mcp"));
        assert_eq!(
            defs[0].headers.get("Authorization").map(String::as_str),
            Some("Bearer $MCPANE_GEMINI_TOKEN")
        );
    }

    #[test]
    fn parse_preserves_cwd_and_enabled() {
        let json = r#"{
          "disabled": {
            "command": "./server",
            "cwd": "/tmp/project",
            "enabled": false
          },
          "default-enabled": {
            "command": "npx",
            "args": ["demo"]
          }
        }"#;
        let defs = parse_mcp_servers_json(json).unwrap().defs;
        let disabled = defs.iter().find(|d| d.name == "disabled").unwrap();
        assert!(!disabled.enabled);
        assert_eq!(disabled.cwd.as_deref(), Some("/tmp/project"));
        let default = defs.iter().find(|d| d.name == "default-enabled").unwrap();
        assert!(default.enabled);
        assert!(default.cwd.is_none());
    }

    /// 显式标注 type/transport 为 sse 的远程条目按 SSE 导入；未标注的仍按 HTTP。
    #[test]
    fn parse_detects_sse_by_declared_type() {
        let json = r#"{
          "legacy": { "url": "https://example.com/sse", "type": "sse" },
          "alt-key": { "url": "https://alt.example.com/sse", "transport": "sse" },
          "modern": { "url": "https://example.com/mcp" }
        }"#;
        let defs = parse_mcp_servers_json(json).unwrap().defs;
        let by_name = |n: &str| defs.iter().find(|d| d.name == n).unwrap();
        assert_eq!(
            by_name("legacy").transport,
            mcpane_storage::ServerTransport::Sse
        );
        assert_eq!(
            by_name("alt-key").transport,
            mcpane_storage::ServerTransport::Sse
        );
        assert_eq!(
            by_name("modern").transport,
            mcpane_storage::ServerTransport::Http
        );
        assert_eq!(by_name("legacy").runtime_policy, "shared");
    }

    #[test]
    fn parse_stdio_uses_shared_runtime_policy() {
        let json = r#"{ "demo": { "command": "npx", "args": ["-y", "demo"] } }"#;
        let defs = parse_mcp_servers_json(json).unwrap().defs;
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].transport, mcpane_storage::ServerTransport::Stdio);
        assert_eq!(defs[0].runtime_policy, "shared");
    }

    /// 「复制配置」/导出写的 bearer、bearerEnv 与 Codex 的 bearer_token_env_var
    /// 都要读进来（此前一律丢弃，复制再粘回来 Token 配置就没了）。
    #[test]
    fn parse_reads_remote_bearer_fields() {
        let json = r#"{
          "direct": { "type": "http", "url": "https://a.dev/mcp", "bearer": "tok" },
          "camel": { "url": "https://b.dev/mcp", "bearerEnv": "B_TOKEN" },
          "codex": { "url": "https://c.dev/mcp", "bearer_token_env_var": "C_TOKEN" },
          "local": { "command": "npx", "bearer": "ignored" }
        }"#;
        let defs = parse_mcp_servers_json(json).unwrap().defs;
        let by_name = |n: &str| defs.iter().find(|d| d.name == n).unwrap();
        assert_eq!(by_name("direct").bearer.as_deref(), Some("tok"));
        assert_eq!(by_name("camel").bearer_env.as_deref(), Some("B_TOKEN"));
        assert_eq!(by_name("codex").bearer_env.as_deref(), Some("C_TOKEN"));
        // stdio 不走 HTTP 认证，带了也不入库。
        assert!(by_name("local").bearer.is_none());
    }

    /// 空白的 command / url 视同缺失，进 invalid 而不是入库后 spawn 时才炸。
    #[test]
    fn parse_treats_blank_command_and_url_as_missing() {
        let json = r#"{ "blank": { "command": "  " }, "empty-url": { "url": "" } }"#;
        let parsed = parse_mcp_servers_json(json).unwrap();
        assert!(parsed.defs.is_empty());
        assert_eq!(parsed.invalid, vec!["blank", "empty-url"]);
    }

    #[test]
    fn placeholder_names_only_accept_env_var_names() {
        assert_eq!(placeholder_names("Bearer ${API_TOKEN}"), vec!["API_TOKEN"]);
        assert_eq!(placeholder_names("${A}-${_b2}"), vec!["A", "_b2"]);
        // 非法变量名、没闭合、缺花括号都不算。
        assert!(placeholder_names("${1X} ${A B} ${OPEN $PLAIN").is_empty());
        // 前面一段没闭合，不能吞掉后面合法的那个。
        assert_eq!(placeholder_names("${bad ${GOOD}"), vec!["GOOD"]);
    }

    /// 预览：只标库里已占用的前缀；占位符逐条列出。
    #[test]
    fn preview_marks_conflicts_and_placeholders() {
        let json = r#"{
          "GitHub": {
            "command": "npx",
            "env": { "GITHUB_TOKEN": "${GITHUB_TOKEN}", "MODE": "${MODE}-x" }
          },
          "memory": { "command": "npx" },
          "Memory": { "command": "npx" },
          "broken": {}
        }"#;
        let mut existing = parse_mcp_servers_json(r#"{ "My GitHub": { "command": "x" } }"#)
            .unwrap()
            .defs;
        existing[0].slug = "github".into();
        let preview = build_preview(parse_mcp_servers_json(json).unwrap(), &existing);
        let conflicts: Vec<Option<&str>> = preview
            .entries
            .iter()
            .map(|e| e.conflict.as_deref())
            .collect();
        // 批内互撞（memory / Memory）取决于勾选，后端预览不判，交给前端。
        assert_eq!(conflicts, vec![Some("My GitHub"), None, None]);
        assert_eq!(preview.entries[0].variables, vec!["GITHUB_TOKEN", "MODE"]);
        assert!(preview.entries[1].variables.is_empty());
        assert_eq!(preview.invalid, vec!["broken"]);
    }

    /// 只导入勾选的条目；填了的代入，没填全的按停用入库并报出来。
    #[test]
    fn select_and_fill_substitutes_values_and_disables_incomplete() {
        let json = r#"{
          "remote": {
            "type": "http",
            "url": "https://x.dev/mcp?key=${KEY}",
            "headers": { "Authorization": "Bearer ${TOKEN}" }
          },
          "partial": { "command": "npx", "env": { "A": "${A}", "B": "${B}" } },
          "unselected": { "command": "npx" }
        }"#;
        let defs = parse_mcp_servers_json(json).unwrap().defs;
        let values = BTreeMap::from([
            (
                "remote".to_owned(),
                BTreeMap::from([
                    ("KEY".to_owned(), "k1".to_owned()),
                    ("TOKEN".to_owned(), " t1 \n".to_owned()),
                ]),
            ),
            (
                "partial".to_owned(),
                BTreeMap::from([
                    ("A".to_owned(), "a".to_owned()),
                    ("B".to_owned(), "   ".to_owned()),
                ]),
            ),
        ]);
        let selected = vec!["remote".to_owned(), "partial".to_owned()];
        let (candidates, incomplete) = select_and_fill(defs, &selected, &values);
        assert_eq!(candidates.len(), 2, "没勾选的不进候选");
        let remote = &candidates[0];
        assert_eq!(remote.url.as_deref(), Some("https://x.dev/mcp?key=k1"));
        assert_eq!(
            remote.headers.get("Authorization").map(String::as_str),
            Some("Bearer t1"),
            "取值首尾空白要去掉"
        );
        assert!(remote.enabled);
        let partial = &candidates[1];
        assert_eq!(partial.env.get("A").map(String::as_str), Some("a"));
        assert_eq!(
            partial.env.get("B").map(String::as_str),
            Some("${B}"),
            "空白取值视同未填，占位符原样保留"
        );
        assert!(!partial.enabled, "没填全的按停用入库");
        assert_eq!(incomplete, vec!["partial"]);
    }

    /// 取值里恰好含 `${…}` 字面量：不能被别的变量再代入一轮，也不能被当成没填。
    #[test]
    fn fill_is_single_pass() {
        let json = r#"{ "s": { "command": "npx", "env": { "A": "${A}", "B": "${B}" } } }"#;
        let defs = parse_mcp_servers_json(json).unwrap().defs;
        let values = BTreeMap::from([(
            "s".to_owned(),
            BTreeMap::from([
                ("A".to_owned(), "p${B}q".to_owned()),
                ("B".to_owned(), "zz".to_owned()),
            ]),
        )]);
        let (candidates, incomplete) = select_and_fill(defs, &["s".to_owned()], &values);
        assert_eq!(
            candidates[0].env.get("A").map(String::as_str),
            Some("p${B}q")
        );
        assert_eq!(candidates[0].env.get("B").map(String::as_str), Some("zz"));
        assert!(incomplete.is_empty());
        assert!(candidates[0].enabled);
    }

    #[test]
    fn parse_rejects_invalid_json() {
        assert!(parse_mcp_servers_json("not json").is_err());
        assert!(parse_mcp_servers_json("[1, 2]").is_err());
        assert!(parse_mcp_servers_json(r#"{ "mcpServers": [] }"#).is_err());
    }
}
