//! Claude Desktop（桌面 GUI）落盘与路由层。
//!
//! Desktop 内置「自定义推理网关」，把 `Claude-3p/configLibrary/<profile>.json`
//! 指向 Anthropic 兼容端点即可切换第三方。两种模式：
//! - **direct**：供应商直接卖 claude-* 模型，profile 直指供应商；模型强制透传
//!   （Desktop 侧无映射能力，route_id 必须等于上游模型名且 claude-safe）；
//! - **proxy**：非 Claude 厂商，profile 指向本地代理（mcpane-desktop-proxy），
//!   由代理做 route_id → 上游真实模型的映射后 anthropic 直通转发。
//!
//! 落盘 = 4 文件协同（失败快照回滚）：normal + threep 两份
//! `claude_desktop_config.json` 写 `deploymentMode`（"3p" 启用 / "1p" 恢复官方），
//! configLibrary 下 profile 与 `_meta.json`（entries + appliedId）。
//!
//! fail-all 校验器角色白名单 sonnet/opus/haiku/fable 为 Desktop 1.12603.1+ 实测；
//! 版本敏感，升级 Desktop 后需复核。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::domain::{ClientApp, DesktopRoute};

/// 我们在 configLibrary 中的固定 profile 身份。
/// 刻意与 cc-switch 的 `…157210` 不同：双装场景两边的条目在 `_meta.json`
/// entries 中可共存，互不覆盖。
pub const PROFILE_ID: &str = "00000000-0000-4000-8000-0000004d4350";
pub const PROFILE_NAME: &str = "MCPane";

const CONFIG_FILE: &str = "claude_desktop_config.json";
const CONFIG_LIBRARY_DIR: &str = "configLibrary";
const DEFAULT_CREATED_AT: &str = "2024-01-01T00:00:00Z";

/// Desktop 模型菜单识别的 route ID 前缀。
pub const CLAUDE_ROUTE_PREFIX: &str = "claude-";
pub const ANTHROPIC_CLAUDE_ROUTE_PREFIX: &str = "anthropic/claude-";
/// Claude Code env 用 `[1M]` 后缀声明 1M 上下文；Desktop schema 不认此后缀，
/// 边界处翻译为 `supports1m` 字段（大小写不敏感）。
pub const ONE_M_CONTEXT_MARKER: &str = "[1m]";

const CURRENT_OPUS_ROUTE_ID: &str = "claude-opus-5";
const LEGACY_OPUS_ROUTE_ID: &str = "claude-opus-4-8";

/// MiMo 上游的 thinking 历史占位符（Desktop 会发 redacted/空 thinking 块，
/// MiMo 的 anthropic 端点拒收，需在映射边界改写）。
const MIMO_REDACTED_THINKING_PLACEHOLDER: &str = "[redacted thinking]";
const MIMO_TOOL_CALL_THINKING_PLACEHOLDER: &str = "tool call";

/// proxy 模式四档默认 route ID。物理顺序 sonnet→opus→haiku→fable：
/// `next_catalog_safe_route_id` 给非安全 route 借用合法角色名时按此序分配，
/// fable 置尾避免无关品牌模型被借成 fable 档名（对齐 cc-switch 注释语义）。
pub const DEFAULT_PROXY_ROUTE_IDS: [&str; 4] = [
    "claude-sonnet-5",
    CURRENT_OPUS_ROUTE_ID,
    "claude-haiku-4-5",
    "claude-fable-5",
];

/// profile 中由 MCPane 全量管理的键（live 配置查看时隐藏——其中含明文 key）。
pub const PROFILE_MANAGED_KEYS: [&str; 7] = [
    "coworkEgressAllowedHosts",
    "disableDeploymentModeChooser",
    "inferenceGatewayApiKey",
    "inferenceGatewayAuthScheme",
    "inferenceGatewayBaseUrl",
    "inferenceProvider",
    "inferenceModels",
];

/// 恢复官方时从 threep config `enterpriseConfig` 清除的网关键
/// （历史版本 Desktop 会把 profile 网关键搬进 enterpriseConfig，同样处理）。
const ENTERPRISE_GATEWAY_KEYS: [&str; 5] = [
    "disableDeploymentModeChooser",
    "inferenceGatewayApiKey",
    "inferenceGatewayAuthScheme",
    "inferenceGatewayBaseUrl",
    "inferenceProvider",
];

// ---------- 路径 ----------

/// Desktop 相关文件路径组（pub 字段，测试与命令层可注入）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeDesktopPaths {
    /// 正常实例配置（与 mcp_clients 写 mcpServers 的是同一文件，互不触碰对方键）。
    pub normal_config: PathBuf,
    /// 3p 实例配置。
    pub threep_config: PathBuf,
    pub config_library: PathBuf,
    /// 我们的 profile（`configLibrary/{PROFILE_ID}.json`）。
    pub profile: PathBuf,
    /// `configLibrary/_meta.json`。
    pub meta: PathBuf,
}

/// 由 normal / threep 两目录推导全部路径。
pub fn paths_from_dirs(normal_dir: &Path, threep_dir: &Path) -> ClaudeDesktopPaths {
    let config_library = threep_dir.join(CONFIG_LIBRARY_DIR);
    let profile = config_library.join(format!("{PROFILE_ID}.json"));
    let meta = config_library.join("_meta.json");
    ClaudeDesktopPaths {
        normal_config: normal_dir.join(CONFIG_FILE),
        threep_config: threep_dir.join(CONFIG_FILE),
        config_library,
        profile,
        meta,
    }
}

pub fn is_supported_platform() -> bool {
    cfg!(any(target_os = "macos", windows))
}

/// 当前平台的 Desktop 路径。Linux 等平台无 Desktop 应用，返回 Err。
pub fn current_platform_paths(home: &Path) -> Result<ClaudeDesktopPaths, String> {
    #[cfg(target_os = "macos")]
    {
        let app_support = home.join("Library").join("Application Support");
        Ok(paths_from_dirs(
            &app_support.join("Claude"),
            &app_support.join("Claude-3p"),
        ))
    }
    #[cfg(windows)]
    {
        let base = windows_instance_base(home);
        Ok(paths_from_dirs(
            &base.join("Claude"),
            &base.join("Claude-3p"),
        ))
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = home;
        Err("当前平台暂不支持 Claude Desktop 3P 配置写入".to_owned())
    }
}

/// 路径发现的不失败版本（adapter/discovery 用）：不支持的平台回落 macOS 布局
/// （路径仅作展示占位；切换入口会先经 `is_supported_platform` 拦截）。
pub fn discover_paths(home: &Path) -> ClaudeDesktopPaths {
    current_platform_paths(home).unwrap_or_else(|_| {
        let app_support = home.join("Library").join("Application Support");
        paths_from_dirs(&app_support.join("Claude"), &app_support.join("Claude-3p"))
    })
}

/// Claude Desktop 的用户数据目录（`claude_desktop_config.json` 所在）。
///
/// 与 `mcp_clients` 写 `mcpServers` 的是同一个文件，两处必须走这一个函数——
/// 早先各算各的，Windows 上一个用 `%APPDATA%` 一个用 `%LOCALAPPDATA%`，谁也没写对。
pub fn user_data_dir(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library")
            .join("Application Support")
            .join("Claude")
    }
    #[cfg(windows)]
    {
        windows_instance_base(home).join("Claude")
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        home.join(".config").join("Claude")
    }
}

/// Claude / Claude-3p 两个实例目录的公共父目录。
///
/// Desktop 现以 MSIX 分发（官方 .exe / Store / winget 都是），文件系统虚拟化把
/// `%APPDATA%` 重定向进包私有目录，写到非虚拟化路径应用读不到——它自己的
/// "Edit Config" 按钮打开的正是读不到的那个（anthropics/claude-code#25579、#26073）。
/// 按**包目录**判定而非配置目录：后者要等应用首次启动才建（#67843）。
#[cfg(windows)]
fn windows_instance_base(home: &Path) -> PathBuf {
    match msix_package_dir(&windows_local_app_data(home)) {
        Some(pkg) => pkg.join("LocalCache").join("Roaming"),
        None => std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData").join("Roaming")),
    }
}

#[cfg(windows)]
fn windows_local_app_data(home: &Path) -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("AppData").join("Local"))
}

/// 判断「已安装」用的探测目录（存在即视为已安装）。
///
/// Windows 上刻意与配置目录不同：MSIX 包目录在应用首次启动前就存在，配置目录
/// 要等启动后才建（#67843），拿后者判定会把刚装好的 Desktop 误报成未安装。
pub fn install_probe_dir(home: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        msix_package_dir(&windows_local_app_data(home)).unwrap_or_else(|| user_data_dir(home))
    }
    #[cfg(not(windows))]
    {
        user_data_dir(home)
    }
}

/// Windows 上应用本体的安装位置：原生 `claude.exe` 优先，否则 MSIX 包目录。
///
/// 不回退到配置目录——MCP 接入也会创建 `%APPDATA%\Claude`，拿它判定会把
/// 「没装 Desktop」误报成已装。
#[allow(dead_code)] // 非 Windows 平台仅测试使用。
pub fn windows_install_path(local_app_data: &Path) -> Option<PathBuf> {
    let exe = local_app_data.join("AnthropicClaude").join("claude.exe");
    if exe.is_file() {
        return Some(exe);
    }
    msix_package_dir(local_app_data)
}

/// 应用本体是否在本机（原生安装器或 MSIX）。不看用户配置目录。
pub fn app_present() -> bool {
    app_install_path().is_some()
}

/// 应用本体路径，供「装在哪」展示。Windows 上优先原生 exe。
pub fn app_install_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let system = PathBuf::from("/Applications/Claude.app");
        if system.is_dir() {
            return Some(system);
        }
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .map(|home| home.join("Applications/Claude.app"))
            .filter(|path| path.is_dir())
    }
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .and_then(|dir| windows_install_path(&dir))
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        None
    }
}

/// 按前缀找 MSIX 包目录。不硬编码 `Claude_pzs8sxrjxfjjc`——那串 hash 由发布者
/// 证书 Subject DN 算出，换证书就会变；Anthropic 在 #25579 给的排查命令用的
/// 也是同款通配。
#[allow(dead_code)] // 非 Windows 平台仅测试使用。
fn msix_package_dir(local_app_data: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(local_app_data.join("Packages"))
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter(|path| {
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                return false;
            };
            name.starts_with("Claude_") || name.starts_with("AnthropicClaude_")
        })
        .collect();
    candidates.sort();
    candidates.into_iter().next()
}

// ---------- 模型名与路由 ----------

/// Desktop fail-all 校验器接受的模型名：`claude-` / `anthropic/claude-` 前缀 +
/// 角色（sonnet/opus/haiku/fable）+ 非空尾部；拒绝 `[1m]` 标记与退化值
/// （写入非法名会触发 Desktop 拒收整组模型）。
pub fn is_claude_safe_model_id(model: &str) -> bool {
    let normalized = model.trim().to_ascii_lowercase();
    if normalized.contains(ONE_M_CONTEXT_MARKER) {
        return false;
    }
    let Some(route_tail) = normalized
        .strip_prefix(ANTHROPIC_CLAUDE_ROUTE_PREFIX)
        .or_else(|| normalized.strip_prefix(CLAUDE_ROUTE_PREFIX))
    else {
        return false;
    };
    ["sonnet-", "opus-", "haiku-", "fable-"]
        .iter()
        .any(|prefix| {
            route_tail
                .strip_prefix(prefix)
                .is_some_and(|rest| !rest.is_empty())
        })
}

/// 剥离尾部 `[1m]` 标记（大小写不敏感）用于路由查找/上游请求。
pub fn strip_one_m_suffix(model: &str) -> &str {
    let trimmed = model.trim();
    let marker = ONE_M_CONTEXT_MARKER.as_bytes();
    let bytes = trimmed.as_bytes();
    if bytes.len() >= marker.len()
        && bytes[bytes.len() - marker.len()..].eq_ignore_ascii_case(marker)
    {
        return trimmed[..trimmed.len() - marker.len()].trim_end();
    }
    trimmed
}

/// 按角色关键词归类 Claude 模型名/route_id；未知返回 None（保持精确报错语义）。
/// 判定顺序 opus → haiku → fable → sonnet（对齐 cc-switch 前后端同序）。
fn claude_role_keyword(model: &str) -> Option<&'static str> {
    let normalized = model.to_ascii_lowercase();
    if normalized.contains("opus") {
        Some("opus")
    } else if normalized.contains("haiku") {
        Some("haiku")
    } else if normalized.contains("fable") {
        Some("fable")
    } else if normalized.contains("sonnet") {
        Some("sonnet")
    } else {
        None
    }
}

/// Opus 新旧 route 别名互认（Desktop 缓存的旧 manifest 会请求旧档名）。
fn is_compatible_opus_route_alias(route_id: &str, requested: &str) -> bool {
    matches!(
        (route_id, requested),
        (CURRENT_OPUS_ROUTE_ID, LEGACY_OPUS_ROUTE_ID)
            | (LEGACY_OPUS_ROUTE_ID, CURRENT_OPUS_ROUTE_ID)
    )
}

/// MiMo 上游识别（模型名或端点域名含 mimo）。
pub fn is_mimo_identifier(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("mimo") || value.contains("xiaomimimo")
}

/// direct 模式路由校验：route_id 必须 claude-safe，且不得映射
/// （upstream 非空时必须与 route_id 相同——Desktop 直连侧没有映射能力）。
/// 返回排序去重后的路由（同名保 supports_1m=true 变体）。
pub fn validate_direct_routes(routes: &[DesktopRoute]) -> Result<Vec<DesktopRoute>, String> {
    let mut result = Vec::new();
    for route in routes {
        let route_id = route.route_id.trim();
        if route_id.is_empty() {
            continue;
        }
        if !is_claude_safe_model_id(route_id) {
            return Err(format!(
                "Claude Desktop 直连模型必须使用 claude-* 或 anthropic/claude-* 名称: {route_id}"
            ));
        }
        let upstream = route.upstream_model.trim();
        if !upstream.is_empty() && upstream != route_id {
            return Err(format!(
                "Claude Desktop 直连模式不能映射模型: {route_id} -> {upstream}；非 Claude 官方模型请使用本地路由模式"
            ));
        }
        result.push(DesktopRoute {
            route_id: route_id.to_owned(),
            upstream_model: route_id.to_owned(),
            label_override: normalized_label(route.label_override.as_deref()),
            supports_1m: route.supports_1m,
        });
    }
    // 同名去重保 1M 变体：先按 (name, supports_1m desc) 排序再 dedup 保首个。
    result.sort_by(|a, b| {
        a.route_id
            .cmp(&b.route_id)
            .then_with(|| b.supports_1m.cmp(&a.supports_1m))
    });
    result.dedup_by(|a, b| a.route_id == b.route_id);
    Ok(result)
}

/// proxy 模式路由解析：跳过空档；非安全 route_id 借用默认档位名修复
/// （借用时菜单显示名回落为上游模型名，用户可辨认）；排序去重；空表报错。
pub fn resolve_proxy_routes(routes: &[DesktopRoute]) -> Result<Vec<DesktopRoute>, String> {
    let reserved: HashSet<String> = routes
        .iter()
        .map(|route| route.route_id.trim())
        .filter(|route_id| is_claude_safe_model_id(route_id))
        .map(str::to_owned)
        .collect();
    let mut result: Vec<DesktopRoute> = Vec::new();
    let mut entries: Vec<&DesktopRoute> = routes.iter().collect();
    entries.sort_by_key(|route| route.route_id.trim());
    for route in entries {
        let route_id = route.route_id.trim();
        let upstream = route.upstream_model.trim();
        if route_id.is_empty() || upstream.is_empty() {
            continue;
        }
        let safe = is_claude_safe_model_id(route_id);
        let repaired_id = if safe {
            route_id.to_owned()
        } else {
            next_catalog_safe_route_id(&result, &reserved)
        };
        let label = normalized_label(route.label_override.as_deref())
            .or_else(|| (!safe).then(|| upstream.to_owned()));
        result.push(DesktopRoute {
            route_id: repaired_id,
            upstream_model: upstream.to_owned(),
            label_override: label,
            supports_1m: route.supports_1m,
        });
    }
    result.sort_by(|a, b| a.route_id.cmp(&b.route_id));
    result.dedup_by(|a, b| a.route_id == b.route_id);
    if result.is_empty() {
        return Err("Claude Desktop 本地路由模式至少需要一个模型路由映射".to_owned());
    }
    Ok(result)
}

fn normalized_label(label: Option<&str>) -> Option<String> {
    label
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// 给非安全 route 分配下一个未占用的默认档位名；默认档耗尽则 `-r{n}` 递增。
fn next_catalog_safe_route_id(existing: &[DesktopRoute], reserved: &HashSet<String>) -> String {
    if let Some(default_route) = DEFAULT_PROXY_ROUTE_IDS.iter().find(|route_id| {
        !reserved.contains(**route_id) && !existing.iter().any(|route| route.route_id == **route_id)
    }) {
        return (*default_route).to_owned();
    }
    let mut index = 2usize;
    loop {
        let route_id = format!("{}-r{index}", DEFAULT_PROXY_ROUTE_IDS[0]);
        if !reserved.contains(&route_id) && !existing.iter().any(|route| route.route_id == route_id)
        {
            return route_id;
        }
        index += 1;
    }
}

// ---------- 运行时映射（proxy 数据面调用） ----------

/// Desktop 请求体的模型映射 + `[1m]` 剥离 + MiMo thinking 历史归一化。
/// `resolved` 为 `resolve_proxy_routes` 产物；`raw` 为原始存储路由
/// （Desktop 缓存旧 manifest 时可能仍请求修复前的 raw id）。
pub fn map_request_body(
    resolved: &[DesktopRoute],
    raw: &[DesktopRoute],
    endpoint_is_mimo: bool,
    mut body: Value,
) -> Result<Value, String> {
    let requested_raw = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "Claude Desktop 请求缺少 model 字段".to_owned())?;
    let requested = strip_one_m_suffix(&requested_raw);

    let upstream_model = resolved
        .iter()
        .find(|route| route.route_id == requested)
        .or_else(|| {
            resolved
                .iter()
                .find(|route| is_compatible_opus_route_alias(&route.route_id, requested))
        })
        .map(|route| route.upstream_model.clone())
        .or_else(|| {
            // 旧 manifest 回落：Desktop 仍以修复前的 raw id 请求。
            raw.iter()
                .find(|route| route.route_id.trim() == requested)
                .map(|route| route.upstream_model.trim())
                .filter(|upstream| !upstream.is_empty())
                .map(str::to_owned)
        })
        .or_else(|| {
            // 角色关键词回落：子 agent 等会请求带发布日期后缀的完整官方名
            // （claude-haiku-4-5-20251001），与 manifest 的简短 route_id 不精确
            // 相等；按角色归档，仅对 claude-safe 名回落避免误映射。
            if !is_claude_safe_model_id(requested) {
                return None;
            }
            let role = claude_role_keyword(requested)?;
            resolved
                .iter()
                .find(|route| claude_role_keyword(&route.route_id) == Some(role))
                // 只配三档的用户：fable 请求降级到 opus 档（与官方安全分类器
                // 降级方向一致）；显式配置 fable 档时上面的精确匹配已优先命中。
                .or_else(|| {
                    (role == "fable")
                        .then(|| {
                            resolved
                                .iter()
                                .find(|route| claude_role_keyword(&route.route_id) == Some("opus"))
                        })
                        .flatten()
                })
                .map(|route| route.upstream_model.clone())
        })
        .ok_or_else(|| format!("Claude Desktop 模型路由未配置: {requested_raw}"))?;

    body["model"] = json!(upstream_model);
    if is_mimo_identifier(&upstream_model) || endpoint_is_mimo {
        normalize_mimo_thinking_history(&mut body);
    }
    Ok(body)
}

/// MiMo anthropic 端点拒收空/redacted thinking 块与缺 thinking 的 tool_use 历史；
/// 在映射边界改写为占位符（逐位对齐 cc-switch normalize_mimo_anthropic_thinking_history）。
fn normalize_mimo_thinking_history(body: &mut Value) {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for message in messages {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        if !content
            .iter()
            .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
        {
            continue;
        }
        let mut has_thinking = false;
        for block in content.iter_mut() {
            match block.get("type").and_then(Value::as_str) {
                Some("thinking") => {
                    let has_non_empty = block
                        .get("thinking")
                        .and_then(Value::as_str)
                        .is_some_and(|value| !value.trim().is_empty());
                    if let Some(obj) = block.as_object_mut() {
                        obj.remove("signature");
                    }
                    if has_non_empty {
                        has_thinking = true;
                    } else if let Some(obj) = block.as_object_mut() {
                        obj.insert(
                            "thinking".to_owned(),
                            json!(MIMO_TOOL_CALL_THINKING_PLACEHOLDER),
                        );
                        has_thinking = true;
                    }
                }
                Some("redacted_thinking") => {
                    *block = json!({
                        "type": "thinking",
                        "thinking": MIMO_REDACTED_THINKING_PLACEHOLDER
                    });
                    has_thinking = true;
                }
                _ => {}
            }
        }
        if !has_thinking {
            content.insert(
                0,
                json!({
                    "type": "thinking",
                    "thinking": MIMO_TOOL_CALL_THINKING_PLACEHOLDER
                }),
            );
        }
    }
}

/// `/claude-desktop/v1/models` 响应：由解析后的路由表生成。
pub fn model_list_response(resolved: &[DesktopRoute]) -> Value {
    let data: Vec<Value> = resolved
        .iter()
        .map(|route| {
            let mut item = json!({
                "type": "model",
                "id": route.route_id,
                "created_at": DEFAULT_CREATED_AT,
            });
            if route.supports_1m {
                item["supports1m"] = json!(true);
            }
            item
        })
        .collect();
    let first_id = data.first().and_then(|item| item.get("id")).cloned();
    let last_id = data.last().and_then(|item| item.get("id")).cloned();
    json!({
        "data": data,
        "has_more": false,
        "first_id": first_id,
        "last_id": last_id,
    })
}

// ---------- profile 构造与落盘 ----------

/// 本地代理网关的 profile base_url。
pub fn proxy_gateway_base_url(listen_port: u16) -> String {
    format!("http://127.0.0.1:{listen_port}/claude-desktop")
}

/// 构造 gateway profile（键集与 cc-switch build_gateway_profile 逐字段一致；
/// scheme 恒 bearer——cc-switch 有显式测试断言 x-api-key 形态不兼容 direct）。
fn build_gateway_profile(base_url: &str, api_key: &str, model_specs: &[DesktopRoute]) -> Value {
    let mut profile = json!({
        "coworkEgressAllowedHosts": ["*"],
        "disableDeploymentModeChooser": true,
        "inferenceGatewayApiKey": api_key,
        "inferenceGatewayAuthScheme": "bearer",
        "inferenceGatewayBaseUrl": base_url,
        "inferenceProvider": "gateway"
    });
    if !model_specs.is_empty() {
        profile["inferenceModels"] =
            Value::Array(model_specs.iter().map(inference_model_json).collect());
    }
    profile
}

/// 模型条目：无 label/1M 时用纯字符串（与 cc-switch 产物逐字节一致）。
fn inference_model_json(route: &DesktopRoute) -> Value {
    if route.supports_1m || route.label_override.is_some() {
        let mut item = json!({ "name": route.route_id });
        if let Some(label) = route.label_override.as_deref() {
            item["labelOverride"] = json!(label);
        }
        if route.supports_1m {
            item["supports1m"] = json!(true);
        }
        item
    } else {
        Value::String(route.route_id.clone())
    }
}

/// direct 模式切换：profile 直指供应商。`routes` 为空则不写 inferenceModels
/// （Desktop 自读上游 /v1/models）。
pub fn apply_direct(
    paths: &ClaudeDesktopPaths,
    base_url: &str,
    api_key: &str,
    routes: &[DesktopRoute],
) -> Result<(), String> {
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        return Err("Claude Desktop 直连供应商缺少接口地址".to_owned());
    }
    if api_key.trim().is_empty() {
        return Err("Claude Desktop 直连供应商缺少 API Key".to_owned());
    }
    let specs = validate_direct_routes(routes)?;
    let profile = build_gateway_profile(base_url, api_key.trim(), &specs);
    with_rollback(paths, |paths| write_threep_activation(paths, &profile))
}

/// proxy 模式切换：profile 指向本地代理网关。
pub fn apply_proxy(
    paths: &ClaudeDesktopPaths,
    listen_port: u16,
    gateway_token: &str,
    routes: &[DesktopRoute],
) -> Result<(), String> {
    if gateway_token.trim().is_empty() {
        return Err("Claude Desktop 本地路由缺少网关 token".to_owned());
    }
    let resolved = resolve_proxy_routes(routes)?;
    let profile = build_gateway_profile(
        &proxy_gateway_base_url(listen_port),
        gateway_token.trim(),
        &resolved,
    );
    with_rollback(paths, |paths| write_threep_activation(paths, &profile))
}

fn write_threep_activation(paths: &ClaudeDesktopPaths, profile: &Value) -> Result<(), String> {
    write_deployment_mode(&paths.normal_config, "3p")?;
    write_deployment_mode(&paths.threep_config, "3p")?;
    write_json_file(&paths.profile, profile)?;
    write_meta(&paths.meta, Some(PROFILE_ID))
}

/// 恢复官方：两份 config 写回 "1p"，清 enterpriseConfig 网关键，删 profile，清 meta。
pub fn restore_official(paths: &ClaudeDesktopPaths) -> Result<(), String> {
    with_rollback(paths, |paths| {
        write_deployment_mode(&paths.normal_config, "1p")?;
        write_deployment_mode(&paths.threep_config, "1p")?;
        remove_gateway_enterprise_config(&paths.threep_config)?;
        if paths.profile.exists() {
            std::fs::remove_file(&paths.profile)
                .map_err(|e| format!("删除 {} 失败：{e}", paths.profile.display()))?;
        }
        write_meta(&paths.meta, None)
    })
}

// ---------- 快照回滚 ----------

struct FileSnapshot {
    path: PathBuf,
    content: Option<Vec<u8>>,
}

fn with_rollback<F>(paths: &ClaudeDesktopPaths, op: F) -> Result<(), String>
where
    F: FnOnce(&ClaudeDesktopPaths) -> Result<(), String>,
{
    let snapshots = snapshot_files(paths)?;
    match op(paths) {
        Ok(()) => Ok(()),
        Err(err) => match restore_snapshots(&snapshots) {
            Ok(()) => Err(err),
            Err(rollback_err) => Err(format!("{err}；回滚失败：{rollback_err}")),
        },
    }
}

fn snapshot_files(paths: &ClaudeDesktopPaths) -> Result<Vec<FileSnapshot>, String> {
    [
        &paths.normal_config,
        &paths.threep_config,
        &paths.profile,
        &paths.meta,
    ]
    .into_iter()
    .map(|path| {
        let content = if path.exists() {
            Some(std::fs::read(path).map_err(|e| format!("读取 {} 失败：{e}", path.display()))?)
        } else {
            None
        };
        Ok(FileSnapshot {
            path: path.clone(),
            content,
        })
    })
    .collect()
}

fn restore_snapshots(snapshots: &[FileSnapshot]) -> Result<(), String> {
    for snapshot in snapshots {
        match &snapshot.content {
            Some(content) => {
                ensure_parent(&snapshot.path)?;
                std::fs::write(&snapshot.path, content)
                    .map_err(|e| format!("写入 {} 失败：{e}", snapshot.path.display()))?;
            }
            None => {
                if snapshot.path.exists() {
                    std::fs::remove_file(&snapshot.path)
                        .map_err(|e| format!("删除 {} 失败：{e}", snapshot.path.display()))?;
                }
            }
        }
    }
    Ok(())
}

// ---------- JSON 文件读写 ----------

fn ensure_parent(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建目录 {} 失败：{e}", parent.display()))?;
    }
    Ok(())
}

fn read_json_or_empty(path: &Path) -> Result<Value, String> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
    let value: Value =
        serde_json::from_str(&content).map_err(|e| format!("解析 {} 失败：{e}", path.display()))?;
    if value.is_object() {
        Ok(value)
    } else {
        Ok(json!({}))
    }
}

fn write_json_file(path: &Path, value: &Value) -> Result<(), String> {
    ensure_parent(path)?;
    let body = serde_json::to_string_pretty(value)
        .map_err(|e| format!("序列化 {} 失败：{e}", path.display()))?;
    std::fs::write(path, body + "\n").map_err(|e| format!("写入 {} 失败：{e}", path.display()))
}

/// read-modify-write：只动 deploymentMode 键，保留其余（mcpServers 等他方键）。
fn write_deployment_mode(path: &Path, mode: &str) -> Result<(), String> {
    let mut value = read_json_or_empty(path)?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert("deploymentMode".to_owned(), Value::String(mode.to_owned()));
    }
    write_json_file(path, &value)
}

fn remove_gateway_enterprise_config(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let mut value = read_json_or_empty(path)?;
    let Some(obj) = value.as_object_mut() else {
        return Ok(());
    };
    let Some(enterprise) = obj
        .get_mut("enterpriseConfig")
        .and_then(Value::as_object_mut)
    else {
        return Ok(());
    };
    for key in ENTERPRISE_GATEWAY_KEYS {
        enterprise.remove(key);
    }
    if enterprise.is_empty() {
        obj.remove("enterpriseConfig");
    }
    write_json_file(path, &value)
}

/// `_meta.json` 维护：entries 去重后追加本条目 + appliedId 指向自身；
/// 清除时移除本条目，appliedId 若指向自己则改指剩余首条或删除。
fn write_meta(path: &Path, applied_profile_id: Option<&str>) -> Result<(), String> {
    let mut value = read_json_or_empty(path)?;
    // read_json_or_empty 恒返回 object；防御性兜底而非 expect（workspace 禁用）。
    let Some(obj) = value.as_object_mut() else {
        return Err(format!("{} 不是 JSON 对象", path.display()));
    };
    let mut entries = obj
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    entries.retain(|entry| entry.get("id").and_then(Value::as_str) != Some(PROFILE_ID));
    match applied_profile_id {
        Some(id) => {
            entries.push(json!({ "id": PROFILE_ID, "name": PROFILE_NAME }));
            obj.insert("appliedId".to_owned(), Value::String(id.to_owned()));
        }
        None => {
            let should_clear = obj
                .get("appliedId")
                .and_then(Value::as_str)
                .is_some_and(|id| id == PROFILE_ID);
            if should_clear {
                if let Some(next_id) = entries
                    .iter()
                    .find_map(|entry| entry.get("id").and_then(Value::as_str))
                {
                    obj.insert("appliedId".to_owned(), Value::String(next_id.to_owned()));
                } else {
                    obj.remove("appliedId");
                }
            }
        }
    }
    obj.insert("entries".to_owned(), Value::Array(entries));
    write_json_file(path, &value)
}

fn read_applied_id(path: &Path) -> Option<String> {
    read_json_or_empty(path).ok().and_then(|value| {
        value
            .get("appliedId")
            .and_then(Value::as_str)
            .map(str::to_owned)
    })
}

fn meta_has_profile_entry(path: &Path) -> bool {
    read_json_or_empty(path)
        .ok()
        .and_then(|value| value.get("entries").and_then(Value::as_array).cloned())
        .is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| entry.get("id").and_then(Value::as_str) == Some(PROFILE_ID))
        })
}

// ---------- 状态与首启收编 ----------

/// 落盘侧实况（service 组合 current provider 与代理运行态成完整 status）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveStatus {
    /// profile 存在或 meta 有我们的条目。
    pub configured: bool,
    pub applied_id: Option<String>,
    /// profile 实际指向的 base_url（与期望值不一致 = 被外部改动）。
    pub actual_base_url: Option<String>,
    /// profile 中存在非 claude-safe 模型名（新版 Desktop 会 fail-all 拒收）。
    pub stale_raw_models: bool,
}

pub fn read_live_status(paths: &ClaudeDesktopPaths) -> LiveStatus {
    let applied_id = read_applied_id(&paths.meta);
    let configured = paths.profile.exists() || meta_has_profile_entry(&paths.meta);
    let profile = read_json_or_empty(&paths.profile).unwrap_or_else(|_| json!({}));
    let actual_base_url = profile
        .get("inferenceGatewayBaseUrl")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let stale_raw_models = profile
        .get("inferenceModels")
        .and_then(Value::as_array)
        .map(|models| {
            models.iter().any(|item| {
                item.as_str()
                    .or_else(|| item.get("name").and_then(Value::as_str))
                    .is_some_and(|model| !is_claude_safe_model_id(model))
            })
        })
        .unwrap_or(false);
    LiveStatus {
        configured,
        applied_id,
        actual_base_url,
        stale_raw_models,
    }
}

/// 首启收编读到的存量三方配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveImport {
    pub base_url: String,
    pub api_key: String,
    pub routes: Vec<DesktopRoute>,
}

/// 首启收编：threep config 为 "3p" 且 meta appliedId 的 profile 可读时，
/// 提取上游配置。指向 loopback 的 profile（本机代理网关——我们或 cc-switch 的）
/// 不收编：其 key 是网关 token 而非上游凭据，收编会造出打环回的假供应商。
pub fn read_active_import(paths: &ClaudeDesktopPaths) -> Option<ActiveImport> {
    let threep = read_json_or_empty(&paths.threep_config).ok()?;
    if threep.get("deploymentMode").and_then(Value::as_str) != Some("3p") {
        return None;
    }
    let applied_id = read_applied_id(&paths.meta)?;
    let profile_path = paths.config_library.join(format!("{applied_id}.json"));
    let profile = read_json_or_empty(&profile_path).ok()?;
    let base_url = profile
        .get("inferenceGatewayBaseUrl")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_owned();
    let lowered = base_url.to_ascii_lowercase();
    if lowered.starts_with("http://127.0.0.1")
        || lowered.starts_with("http://localhost")
        || lowered.starts_with("http://[::1]")
    {
        return None;
    }
    let api_key = profile
        .get("inferenceGatewayApiKey")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_owned();
    let routes = profile
        .get("inferenceModels")
        .and_then(Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(|item| {
                    let (name, label, supports_1m) = if let Some(name) = item.as_str() {
                        (name, None, false)
                    } else {
                        (
                            item.get("name").and_then(Value::as_str)?,
                            item.get("labelOverride")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            item.get("supports1m")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        )
                    };
                    Some(DesktopRoute {
                        route_id: name.to_owned(),
                        upstream_model: name.to_owned(),
                        label_override: label,
                        supports_1m,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(ActiveImport {
        base_url,
        api_key,
        routes,
    })
}

/// 收编条目的归属 app（常量语义放这里，避免调用方散写）。
pub const IMPORT_APP: ClientApp = ClientApp::ClaudeDesktop;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn temp_paths() -> (tempfile::TempDir, ClaudeDesktopPaths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_from_dirs(&dir.path().join("Claude"), &dir.path().join("Claude-3p"));
        (dir, paths)
    }

    fn route(route_id: &str, upstream: &str) -> DesktopRoute {
        DesktopRoute {
            route_id: route_id.to_owned(),
            upstream_model: upstream.to_owned(),
            label_override: None,
            supports_1m: false,
        }
    }

    #[test]
    fn safe_model_id_matrix() {
        // 合法：两种前缀 × 四角色，尾部非空。
        for ok in [
            "claude-sonnet-5",
            "claude-opus-4-8",
            "claude-haiku-4-5",
            "claude-fable-5",
            "anthropic/claude-sonnet-5",
            "Claude-Sonnet-5", // 大小写不敏感
        ] {
            assert!(is_claude_safe_model_id(ok), "{ok} 应为合法");
        }
        // 非法：错前缀 / 空尾部 / [1m] 标记 / 非 Claude 名。
        for bad in [
            "claude-sonnet-",
            "claude-mythos-1",
            "deepseek-v4-pro",
            "claude-sonnet-5[1m]",
            "claude-sonnet-5[1M]",
            "gpt-5.6",
            "",
        ] {
            assert!(!is_claude_safe_model_id(bad), "{bad} 应为非法");
        }
    }

    #[test]
    fn one_m_suffix_strips_case_insensitively() {
        assert_eq!(strip_one_m_suffix("claude-sonnet-5[1m]"), "claude-sonnet-5");
        assert_eq!(
            strip_one_m_suffix("claude-sonnet-5 [1M]"),
            "claude-sonnet-5"
        );
        assert_eq!(strip_one_m_suffix("claude-sonnet-5"), "claude-sonnet-5");
    }

    #[test]
    fn direct_routes_reject_mapping_and_unsafe_ids() {
        // 映射被拒。
        let err =
            validate_direct_routes(&[route("claude-sonnet-5", "deepseek-v4-pro")]).unwrap_err();
        assert!(err.contains("不能映射"), "{err}");
        // 非安全 id 被拒。
        let err = validate_direct_routes(&[route("deepseek-v4-pro", "")]).unwrap_err();
        assert!(err.contains("claude-*"), "{err}");
        // 同名去重保 1M 变体。
        let mut with_1m = route("claude-sonnet-5", "claude-sonnet-5");
        with_1m.supports_1m = true;
        let out = validate_direct_routes(&[route("claude-sonnet-5", "claude-sonnet-5"), with_1m])
            .unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].supports_1m);
    }

    #[test]
    fn proxy_routes_repair_unsafe_ids_and_require_nonempty() {
        // 非安全 id 借用默认档位，label 回落上游名。
        let out = resolve_proxy_routes(&[route("deepseek", "deepseek-v4-pro")]).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].route_id, "claude-sonnet-5");
        assert_eq!(out[0].upstream_model, "deepseek-v4-pro");
        assert_eq!(out[0].label_override.as_deref(), Some("deepseek-v4-pro"));
        // 全空表报错。
        assert!(resolve_proxy_routes(&[route("", "")]).is_err());
        // 安全 id 原样保留。
        let out = resolve_proxy_routes(&[route("claude-opus-4-8", "kimi-k2.7-code")]).unwrap();
        assert_eq!(out[0].route_id, "claude-opus-4-8");
    }

    #[test]
    fn map_request_body_covers_exact_alias_legacy_and_role_fallback() {
        let resolved =
            resolve_proxy_routes(&[route("claude-sonnet-5", "deepseek-v4-pro")]).unwrap();
        let raw = vec![route("my-raw-id", "deepseek-v4-pro")];
        // 精确命中。
        let body = map_request_body(
            &resolved,
            &raw,
            false,
            serde_json::json!({"model": "claude-sonnet-5"}),
        )
        .unwrap();
        assert_eq!(body["model"], "deepseek-v4-pro");
        // [1m] 剥离后命中。
        let body = map_request_body(
            &resolved,
            &raw,
            false,
            serde_json::json!({"model": "claude-sonnet-5[1m]"}),
        )
        .unwrap();
        assert_eq!(body["model"], "deepseek-v4-pro");
        // raw id 回落。
        let body = map_request_body(
            &resolved,
            &raw,
            false,
            serde_json::json!({"model": "my-raw-id"}),
        )
        .unwrap();
        assert_eq!(body["model"], "deepseek-v4-pro");
        // 角色关键词回落：带日期后缀的完整官方名归到 sonnet 档。
        let body = map_request_body(
            &resolved,
            &raw,
            false,
            serde_json::json!({"model": "claude-sonnet-5-20260115"}),
        )
        .unwrap();
        assert_eq!(body["model"], "deepseek-v4-pro");
        // 未配置角色（haiku）且非法名不回落 → 报错。
        let err = map_request_body(
            &resolved,
            &raw,
            false,
            serde_json::json!({"model": "gpt-5.6"}),
        )
        .unwrap_err();
        assert!(err.contains("未配置"), "{err}");
    }

    #[test]
    fn fable_request_falls_back_to_opus_route() {
        let resolved = resolve_proxy_routes(&[route("claude-opus-4-8", "kimi-k2.7-code")]).unwrap();
        let body = map_request_body(
            &resolved,
            &[],
            false,
            serde_json::json!({"model": "claude-fable-5"}),
        )
        .unwrap();
        assert_eq!(body["model"], "kimi-k2.7-code");
    }

    #[test]
    fn opus_route_alias_bridges_legacy_and_current() {
        let resolved = resolve_proxy_routes(&[route("claude-opus-5", "glm-5.1")]).unwrap();
        let body = map_request_body(
            &resolved,
            &[],
            false,
            serde_json::json!({"model": "claude-opus-4-8"}),
        )
        .unwrap();
        assert_eq!(body["model"], "glm-5.1");
    }

    #[test]
    fn mimo_thinking_history_normalization() {
        let resolved = resolve_proxy_routes(&[route("claude-sonnet-5", "mimo-v2.5-pro")]).unwrap();
        let body = map_request_body(
            &resolved,
            &[],
            false,
            serde_json::json!({
                "model": "claude-sonnet-5",
                "messages": [{
                    "role": "assistant",
                    "content": [
                        {"type": "redacted_thinking", "data": "xxx"},
                        {"type": "tool_use", "id": "t1", "name": "f", "input": {}}
                    ]
                }]
            }),
        )
        .unwrap();
        let blocks = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "thinking");
        assert_eq!(blocks[0]["thinking"], MIMO_REDACTED_THINKING_PLACEHOLDER);
        // 无 thinking 块的 tool_use 历史前插占位。
        let body = map_request_body(
            &resolved,
            &[],
            false,
            serde_json::json!({
                "model": "claude-sonnet-5",
                "messages": [{
                    "role": "assistant",
                    "content": [{"type": "tool_use", "id": "t1", "name": "f", "input": {}}]
                }]
            }),
        )
        .unwrap();
        let blocks = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["thinking"], MIMO_TOOL_CALL_THINKING_PLACEHOLDER);
        // 非 MiMo 上游不改写。
        let other = resolve_proxy_routes(&[route("claude-sonnet-5", "glm-5.1")]).unwrap();
        let body = map_request_body(
            &other,
            &[],
            false,
            serde_json::json!({
                "model": "claude-sonnet-5",
                "messages": [{
                    "role": "assistant",
                    "content": [{"type": "tool_use", "id": "t1", "name": "f", "input": {}}]
                }]
            }),
        )
        .unwrap();
        let blocks = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "tool_use");
    }

    #[test]
    fn gateway_profile_shape_matches_reference() {
        let mut with_1m = route("claude-sonnet-5", "claude-sonnet-5");
        with_1m.supports_1m = true;
        let profile = build_gateway_profile(
            "https://gw.example.com",
            "sk-test",
            &[with_1m, route("claude-haiku-4-5", "claude-haiku-4-5")],
        );
        assert_eq!(profile["inferenceProvider"], "gateway");
        assert_eq!(profile["inferenceGatewayAuthScheme"], "bearer");
        assert_eq!(profile["inferenceGatewayBaseUrl"], "https://gw.example.com");
        assert_eq!(profile["inferenceGatewayApiKey"], "sk-test");
        assert_eq!(profile["disableDeploymentModeChooser"], true);
        assert_eq!(profile["coworkEgressAllowedHosts"][0], "*");
        let models = profile["inferenceModels"].as_array().unwrap();
        // 1M 变体是对象，普通条目是纯字符串。
        assert_eq!(models[0]["name"], "claude-sonnet-5");
        assert_eq!(models[0]["supports1m"], true);
        assert_eq!(models[1], "claude-haiku-4-5");
        // 空路由不写 inferenceModels。
        let empty = build_gateway_profile("https://gw.example.com", "sk", &[]);
        assert!(empty.get("inferenceModels").is_none());
    }

    #[test]
    fn apply_direct_writes_four_files_and_preserves_foreign_keys() {
        let (_dir, paths) = temp_paths();
        // normal config 预置 mcpServers（MCP 侧写入的他方键），必须原样保留。
        std::fs::create_dir_all(paths.normal_config.parent().unwrap()).unwrap();
        std::fs::write(
            &paths.normal_config,
            "{\"mcpServers\":{\"mcpane\":{\"command\":\"hub\"}}}",
        )
        .unwrap();
        apply_direct(&paths, "https://aihubmix.com/", "sk-live", &[]).unwrap();
        let normal = read_json_or_empty(&paths.normal_config).unwrap();
        assert_eq!(normal["deploymentMode"], "3p");
        assert_eq!(normal["mcpServers"]["mcpane"]["command"], "hub");
        let threep = read_json_or_empty(&paths.threep_config).unwrap();
        assert_eq!(threep["deploymentMode"], "3p");
        let profile = read_json_or_empty(&paths.profile).unwrap();
        // 尾斜杠归一。
        assert_eq!(profile["inferenceGatewayBaseUrl"], "https://aihubmix.com");
        let meta = read_json_or_empty(&paths.meta).unwrap();
        assert_eq!(meta["appliedId"], PROFILE_ID);
        assert_eq!(meta["entries"][0]["name"], PROFILE_NAME);
    }

    #[test]
    fn restore_official_cleans_up_and_repoints_meta() {
        let (_dir, paths) = temp_paths();
        apply_direct(&paths, "https://gw.example.com", "sk", &[]).unwrap();
        // 模拟历史版本把网关键搬进 enterpriseConfig + 他人条目共存。
        let mut threep = read_json_or_empty(&paths.threep_config).unwrap();
        threep["enterpriseConfig"] = json!({
            "inferenceGatewayApiKey": "sk",
            "inferenceProvider": "gateway",
            "keepMe": true
        });
        write_json_file(&paths.threep_config, &threep).unwrap();
        let mut meta = read_json_or_empty(&paths.meta).unwrap();
        meta["entries"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id": "other-tool", "name": "Other"}));
        write_json_file(&paths.meta, &meta).unwrap();

        restore_official(&paths).unwrap();
        let normal = read_json_or_empty(&paths.normal_config).unwrap();
        assert_eq!(normal["deploymentMode"], "1p");
        let threep = read_json_or_empty(&paths.threep_config).unwrap();
        assert_eq!(threep["deploymentMode"], "1p");
        // 网关键被清、非网关键保留。
        assert!(
            threep["enterpriseConfig"]
                .get("inferenceGatewayApiKey")
                .is_none()
        );
        assert_eq!(threep["enterpriseConfig"]["keepMe"], true);
        assert!(!paths.profile.exists());
        let meta = read_json_or_empty(&paths.meta).unwrap();
        // 我们的条目移除，appliedId 改指剩余首条。
        assert_eq!(meta["appliedId"], "other-tool");
        assert_eq!(meta["entries"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn restore_official_clears_applied_id_when_no_entries_left() {
        let (_dir, paths) = temp_paths();
        apply_direct(&paths, "https://gw.example.com", "sk", &[]).unwrap();
        restore_official(&paths).unwrap();
        let meta = read_json_or_empty(&paths.meta).unwrap();
        assert!(meta.get("appliedId").is_none());
        assert!(meta["entries"].as_array().unwrap().is_empty());
    }

    #[test]
    fn rollback_restores_snapshot_on_failure() {
        let (_dir, paths) = temp_paths();
        apply_direct(&paths, "https://before.example.com", "sk", &[]).unwrap();
        let before = std::fs::read_to_string(&paths.profile).unwrap();
        // 直连路由映射非法 → apply 失败；校验在快照前，落盘文件不动。
        let err = apply_direct(
            &paths,
            "https://after.example.com",
            "sk",
            &[route("claude-sonnet-5", "deepseek-v4-pro")],
        )
        .unwrap_err();
        assert!(err.contains("不能映射"));
        assert_eq!(std::fs::read_to_string(&paths.profile).unwrap(), before);
        // with_rollback 本身：op 中途失败恢复快照。
        let result = with_rollback(&paths, |paths| {
            write_deployment_mode(&paths.normal_config, "1p")?;
            Err("boom".to_owned())
        });
        assert!(result.is_err());
        let normal = read_json_or_empty(&paths.normal_config).unwrap();
        assert_eq!(normal["deploymentMode"], "3p", "回滚后应保持切换前状态");
    }

    #[test]
    fn live_status_reports_stale_raw_models_and_base_url() {
        let (_dir, paths) = temp_paths();
        let status = read_live_status(&paths);
        assert!(!status.configured);
        apply_proxy(
            &paths,
            24871,
            "mcpane-tok",
            &[route("claude-sonnet-5", "deepseek-v4-pro")],
        )
        .unwrap();
        let status = read_live_status(&paths);
        assert!(status.configured);
        assert_eq!(status.applied_id.as_deref(), Some(PROFILE_ID));
        assert_eq!(
            status.actual_base_url.as_deref(),
            Some("http://127.0.0.1:24871/claude-desktop")
        );
        assert!(!status.stale_raw_models);
        // 手工塞入非安全模型名 → stale 检出。
        let mut profile = read_json_or_empty(&paths.profile).unwrap();
        profile["inferenceModels"] = json!(["deepseek-v4-pro"]);
        write_json_file(&paths.profile, &profile).unwrap();
        assert!(read_live_status(&paths).stale_raw_models);
    }

    #[test]
    fn active_import_reads_direct_but_skips_loopback() {
        let (_dir, paths) = temp_paths();
        apply_direct(&paths, "https://gw.example.com", "sk-live", &[]).unwrap();
        let imported = read_active_import(&paths).unwrap();
        assert_eq!(imported.base_url, "https://gw.example.com");
        assert_eq!(imported.api_key, "sk-live");
        // loopback（本机代理网关）不收编。
        apply_proxy(
            &paths,
            24871,
            "mcpane-tok",
            &[route("claude-sonnet-5", "x")],
        )
        .unwrap();
        assert!(read_active_import(&paths).is_none());
        // 恢复官方（"1p"）后不收编。
        restore_official(&paths).unwrap();
        assert!(read_active_import(&paths).is_none());
    }

    #[test]
    fn active_import_reads_other_tools_applied_profile() {
        // cc-switch 等他方工具的 appliedId 也按实际生效配置收编。
        let (_dir, paths) = temp_paths();
        std::fs::create_dir_all(&paths.config_library).unwrap();
        write_json_file(&paths.threep_config, &json!({"deploymentMode": "3p"})).unwrap();
        write_json_file(
            &paths.meta,
            &json!({"appliedId": "cc-switch-id", "entries": [{"id": "cc-switch-id"}]}),
        )
        .unwrap();
        write_json_file(
            &paths.config_library.join("cc-switch-id.json"),
            &json!({
                "inferenceProvider": "gateway",
                "inferenceGatewayBaseUrl": "https://relay.example.com",
                "inferenceGatewayApiKey": "sk-other",
                "inferenceModels": ["claude-sonnet-5", {"name": "claude-opus-4-8", "supports1m": true}]
            }),
        )
        .unwrap();
        let imported = read_active_import(&paths).unwrap();
        assert_eq!(imported.base_url, "https://relay.example.com");
        assert_eq!(imported.routes.len(), 2);
        assert!(imported.routes[1].supports_1m);
    }

    #[test]
    fn model_list_response_shape() {
        let mut with_1m = route("claude-sonnet-5", "deepseek-v4-pro");
        with_1m.supports_1m = true;
        let resolved = resolve_proxy_routes(&[with_1m]).unwrap();
        let resp = model_list_response(&resolved);
        assert_eq!(resp["data"][0]["type"], "model");
        assert_eq!(resp["data"][0]["id"], "claude-sonnet-5");
        assert_eq!(resp["data"][0]["supports1m"], true);
        assert_eq!(resp["has_more"], false);
        assert_eq!(resp["first_id"], "claude-sonnet-5");
        assert_eq!(resp["last_id"], "claude-sonnet-5");
    }

    /// MSIX 包目录按前缀匹配，不认硬编码的 publisher hash。
    #[test]
    fn msix_package_dir_matches_by_prefix_not_hardcoded_hash() {
        let dir = tempfile::tempdir().unwrap();
        // 连 Packages 目录都没有 → None（非 MSIX 安装，调用方回落 %APPDATA%）。
        assert!(msix_package_dir(dir.path()).is_none());

        let packages = dir.path().join("Packages");
        std::fs::create_dir_all(packages.join("SomeOther_abc")).unwrap();
        std::fs::create_dir_all(packages.join("Claudia_xyz")).unwrap();
        assert!(
            msix_package_dir(dir.path()).is_none(),
            "只认 `Claude_` / `AnthropicClaude_` 前缀，不得被 Claudia 之类误命中"
        );

        // 真实观测到的包名是 Claude_pzs8sxrjxfjjc，但 hash 随发布者证书变，
        // 这里用另一个 hash 确认匹配靠的是前缀。
        std::fs::create_dir_all(packages.join("Claude_9y9zzz9z9z9zz")).unwrap();
        assert_eq!(
            msix_package_dir(dir.path()).unwrap(),
            packages.join("Claude_9y9zzz9z9z9zz")
        );
    }

    #[test]
    fn windows_install_path_prefers_native_exe_over_msix() {
        let dir = tempfile::tempdir().unwrap();
        let exe_dir = dir.path().join("AnthropicClaude");
        std::fs::create_dir_all(&exe_dir).unwrap();
        let exe = exe_dir.join("claude.exe");
        std::fs::write(&exe, b"stub").unwrap();
        std::fs::create_dir_all(dir.path().join("Packages").join("Claude_abc")).unwrap();
        assert_eq!(
            windows_install_path(dir.path()).as_deref(),
            Some(exe.as_path())
        );
    }

    #[test]
    fn windows_install_path_accepts_msix_without_native_exe() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("Packages").join("AnthropicClaude_xyz");
        std::fs::create_dir_all(&pkg).unwrap();
        assert_eq!(
            windows_install_path(dir.path()).as_deref(),
            Some(pkg.as_path())
        );
    }

    #[test]
    fn windows_install_path_ignores_config_dir_without_app() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Claude")).unwrap();
        assert!(windows_install_path(dir.path()).is_none());
    }
}
