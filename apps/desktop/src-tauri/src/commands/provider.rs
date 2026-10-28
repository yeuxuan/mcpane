//! Provider CRUD 与切换（TASK-502）+ 托盘支撑（TASK-602）。

use mcpane_provider::{
    ClientApp, ClientPaths, DesktopMode, JsonProviderStore, ProviderCategory, ProviderProfile,
    ProviderService, RoutingConfig, SwitchOutcome, UnifiedProvider, adapter_for, claude_desktop,
};
use mcpane_secrets::{KeyringSecretStore, Secret};

use super::connect::{engine, user_home};
use super::mcpane_home;

type Service = ProviderService<JsonProviderStore, KeyringSecretStore>;
/// desktop_proxy 托管模块引用的 service 类型名。
pub(crate) type ProviderServiceHandle = Service;

pub(super) fn provider_service() -> Service {
    ProviderService::new(JsonProviderStore::new(mcpane_home()), KeyringSecretStore)
}

/// desktop_proxy 托管模块的受限访问口（只为网关同步暴露，不泛用）。
pub(crate) fn provider_service_for_desktop_proxy() -> Service {
    provider_service()
}

/// 客户端配置目录覆盖：设置页显式目录 > CLAUDE_CONFIG_DIR（仅 Claude）。
/// `detect_clients` / 安装探测与 provider 页共用，避免两套口径。
pub(crate) fn config_dir_for_app(app: ClientApp) -> Option<std::path::PathBuf> {
    let overrides = provider_service()
        .config_dir_overrides()
        .unwrap_or_default();
    overrides
        .get(&app)
        .map(std::path::PathBuf::from)
        .or_else(|| match app {
            ClientApp::Claude => mcpane_adapter_claude::config_dir_override(),
            _ => None,
        })
}

// ---------- Provider（TASK-502 数据层） ----------

/// Provider 变更操作的全局互斥：UI 与托盘可能并发切换/编辑，
/// providers.json 的 load-modify-save 不加锁会后写覆盖前写
///（最坏情况丢掉刚写入的 Codex 官方登录备份引用）。
static PROVIDER_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(super) fn provider_write_guard() -> std::sync::MutexGuard<'static, ()> {
    PROVIDER_MUTEX.lock().unwrap_or_else(|p| p.into_inner())
}

/// 首启存量收编：把各 app 既有的 live 配置收编为可管理的 Provider 并置为当前
/// 生效项。在 setup 中**同步**调用（早于任何 IPC 与托盘构建），保证前端首帧与
/// 托盘的"当前生效项"如实反映 live（可能是中转），而非误显示官方。
/// 单条失败只记日志、不阻断启动——收编是兜底能力。
pub(crate) fn import_live_configs_on_startup() {
    let _guard = provider_write_guard();
    let svc = provider_service();
    let engine = engine();
    let paths = client_paths();
    for app in ClientApp::ALL {
        match svc.import_live_on_startup(app, &engine, &paths) {
            Ok(Some(name)) => {
                println!("[mcpane] 首启收编 {} 既有配置为「{name}」", app.as_str());
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!("[mcpane] 首启收编 {} 失败（已忽略）: {error}", app.as_str());
            }
        }
    }
}

/// 首启播种官方项：只为**本机确实装了**的客户端建一条官方 Profile。
/// 必须在 [`import_live_configs_on_startup`] 之后调用——收编要先看到"该 app
/// 还没有非官方 provider"，播种早了会让存量第三方配置漏收。
/// 单条失败只记日志、不阻断启动（与收编同口径）。
pub(crate) fn seed_official_providers_on_startup() {
    let _guard = provider_write_guard();
    let installed = mcpane_provider::installed_apps();
    match provider_service().seed_officials(&installed) {
        Ok(seeded) if !seeded.is_empty() => {
            let names: Vec<&str> = seeded.iter().map(|app| app.as_str()).collect();
            println!(
                "[mcpane] 已为已安装客户端播种官方订阅项：{}",
                names.join(", ")
            );
        }
        Ok(_) => {}
        Err(error) => eprintln!("[mcpane] 播种官方订阅项失败（已忽略）: {error}"),
    }
}

#[tauri::command]
pub async fn providers_list() -> Result<Vec<ProviderProfile>, String> {
    tauri::async_runtime::spawn_blocking(|| provider_service().list().map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())?
}

/// 每个 app 当前生效的 Provider id（未切换过为 null）。
/// 按 `ClientApp::ALL` 遍历生成，新增客户端不需要改这里。
pub type CurrentProviders = std::collections::BTreeMap<String, Option<String>>;

#[tauri::command]
pub async fn provider_current() -> Result<CurrentProviders, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let current = provider_service().current().map_err(|e| e.to_string())?;
        Ok(ClientApp::ALL
            .into_iter()
            .map(|app| (app.as_str().to_owned(), current.get(&app).cloned()))
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 各客户端的安装态（app → 是否检测到安装）。
/// 前端用它决定：首启默认显示哪些客户端页签、没装的客户端给安装引导。
pub type ClientInstallStatus = std::collections::BTreeMap<String, bool>;

#[tauri::command]
pub async fn provider_clients_installed() -> Result<ClientInstallStatus, String> {
    // 走 PATH 查可执行文件 + 应用目录判定，都是 IO，放阻塞线程池。
    tauri::async_runtime::spawn_blocking(|| {
        Ok(ClientApp::ALL
            .into_iter()
            .map(|app| {
                let config_dir = config_dir_for_app(app);
                let installed = mcpane_provider::is_installed_with_dir(app, config_dir.as_deref());
                (app.as_str().to_owned(), installed)
            })
            .collect())
    })
    .await
    .map_err(|e: tauri::Error| e.to_string())?
}

/// additive 客户端（OpenCode）：从 live 配置移除该供应商，但保留列表记录。
#[tauri::command]
pub async fn provider_remove_from_live(handle: tauri::AppHandle, id: String) -> Result<(), String> {
    let result = tauri::async_runtime::spawn_blocking(move || {
        let _guard = provider_write_guard();
        provider_service()
            .remove_from_live(&id, &engine(), &client_paths())
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?;
    if result.is_ok() {
        crate::refresh_tray_menu(&handle);
    }
    result
}

/// Hermes v12+ `providers:` 字典里的外部供应商（Web UI 托管，只读呈现）。
#[tauri::command]
pub async fn provider_hermes_external() -> Result<Vec<mcpane_adapter_hermes::DictProvider>, String>
{
    tauri::async_runtime::spawn_blocking(|| {
        provider_service()
            .hermes_external_providers(&client_paths())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// additive 客户端在 live 配置里已启用的供应商 id（前端据此显示「已启用」）。
#[tauri::command]
pub async fn provider_live_ids(app: String) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        provider_service()
            .live_provider_ids(parse_app(&app)?, &client_paths())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 设置页「本地环境」：各客户端的安装位置 + 版本 + 是否跑得起来。
/// 跑不起来时附带失败原因（起不来 / 超时 / 非零退出），界面据此给对症的下一步。
/// 会对每个 CLI 跑一次 `--version`（Node 启动，几百毫秒），只在用户打开设置页时调用。
#[tauri::command]
pub async fn client_env_status() -> Result<Vec<mcpane_provider::ClientEnv>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        // 与 detect_clients / provider_clients_installed 同口径。
        let dirs: std::collections::BTreeMap<ClientApp, String> = ClientApp::ALL
            .into_iter()
            .filter_map(|app| config_dir_for_app(app).map(|dir| (app, dir.display().to_string())))
            .collect();
        mcpane_provider::client_envs_with_dirs(&dirs)
    })
    .await
    .map_err(|e| e.to_string())
}

/// 各客户端配置目录的用户覆盖（app → 绝对路径；未覆盖的 app 不出现）。
pub type ConfigDirOverrides = std::collections::BTreeMap<String, String>;

#[tauri::command]
pub async fn provider_config_dirs_get() -> Result<ConfigDirOverrides, String> {
    tauri::async_runtime::spawn_blocking(|| {
        provider_service()
            .config_dir_overrides()
            .map(|dirs| {
                dirs.into_iter()
                    .map(|(app, dir)| (app.as_str().to_owned(), dir))
                    .collect()
            })
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 设置/清除某客户端的配置目录覆盖（空串 = 回到默认位置）。
#[tauri::command]
pub async fn provider_config_dir_set(app: String, dir: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = provider_write_guard();
        provider_service()
            .set_config_dir_override(parse_app(&app)?, &dir)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 确保某 app 的官方项存在（用户删掉后从"添加供应商"再加回来）。已存在则原样返回。
#[tauri::command]
pub async fn provider_official_ensure(
    handle: tauri::AppHandle,
    app: String,
) -> Result<ProviderProfile, String> {
    let result = tauri::async_runtime::spawn_blocking(move || {
        let _guard = provider_write_guard();
        provider_service()
            .ensure_official(parse_app(&app)?)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?;
    if result.is_ok() {
        crate::refresh_tray_menu(&handle);
    }
    result
}

#[tauri::command]
pub async fn provider_add(
    handle: tauri::AppHandle,
    profile: ProviderProfile,
) -> Result<ProviderProfile, String> {
    let result = tauri::async_runtime::spawn_blocking(move || {
        let _guard = provider_write_guard();
        provider_service()
            .add_relay(profile)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?;
    if result.is_ok() {
        crate::refresh_tray_menu(&handle);
    }
    result
}

#[tauri::command]
pub async fn provider_update(
    handle: tauri::AppHandle,
    profile: ProviderProfile,
) -> Result<(), String> {
    let result = tauri::async_runtime::spawn_blocking(move || {
        let _guard = provider_write_guard();
        provider_service()
            .update_relay(profile, &engine(), &client_paths())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?;
    if result.is_ok() {
        crate::refresh_tray_menu(&handle);
        // 编辑的供应商可能是 Desktop proxy 当前项或某 app 的路由候选，地址/Key/
        // 模型变更要立即反映到路由快照——失败要上抛：路由还攥着旧上游，
        // 客户端只会报「无效的本地网关 token」或连错上游，用户无从归因。
        crate::desktop_proxy::sync_from_store()
            .await
            .map_err(|e| format!("配置已保存，但本地路由未同步：{e}"))?;
    }
    result
}

#[tauri::command]
pub async fn provider_delete(handle: tauri::AppHandle, id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = provider_write_guard();
        let service = provider_service();
        // additive 客户端：删记录前先把它从 live 配置里摘掉，否则用户删了供应商，
        // OpenCode/OpenClaw 的配置里还留着一条连不上的孤儿条目。
        // 已经不在配置里（用户先点过「移除」）时 remove_from_live 是 no-op。
        let is_additive = service
            .list()
            .ok()
            .and_then(|profiles| profiles.into_iter().find(|p| p.id == id))
            .is_some_and(|p| p.app.is_additive());
        if is_additive && let Err(error) = service.remove_from_live(&id, &engine(), &client_paths())
        {
            // 摘不掉不该挡住删除（配置文件可能被外部改坏），但要留痕。
            eprintln!("[mcpane] 删除前从 live 配置移除失败（已忽略）: {error}");
        }
        service.delete(&id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    crate::refresh_tray_menu(&handle);
    // 删掉的可能是 Desktop 网关上游或路由的故障转移候选。
    crate::desktop_proxy::sync_from_store()
        .await
        .map_err(|e| format!("供应商已删除，但本地路由未同步：{e}"))?;
    Ok(())
}

/// 切换某个 app 的当前 Provider（官方订阅 ↔ 中转站）。
#[tauri::command]
pub async fn provider_switch(
    handle: tauri::AppHandle,
    app: String,
    id: String,
) -> Result<SwitchOutcome, String> {
    let result = tauri::async_runtime::spawn_blocking(move || switch_provider_sync(&app, &id))
        .await
        .map_err(|e| e.to_string())?;
    if result.is_ok() {
        crate::refresh_tray_menu(&handle);
    }
    result
}

/// 录入 API Key：明文只在此瞬时存在，落 Keychain 后即丢弃。
/// 改的是当前生效项的 Key 时立即重写 live（Key 轮换即时生效，不必再手动切换）。
#[tauri::command]
pub async fn provider_set_api_key(id: String, api_key: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = provider_write_guard();
        let svc = provider_service();
        svc.set_api_key(&id, &Secret::new(api_key))
            .map_err(|e| e.to_string())?;
        svc.reapply_current(&id, &engine(), &client_paths())
            .map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| e.to_string())??;
    // Key 轮换若命中当前 Desktop proxy 供应商，网关上游快照同步换 Key。
    crate::desktop_proxy::sync_from_store()
        .await
        .map_err(|e| format!("Key 已保存，但本地网关未同步：{e}"))?;
    Ok(())
}

/// 编辑官方订阅的可覆盖项（附加配置片段 / 通用配置开关）。
#[tauri::command]
pub async fn provider_official_update(
    handle: tauri::AppHandle,
    app: String,
    extra_settings: Option<serde_json::Value>,
    common_config_enabled: bool,
) -> Result<(), String> {
    let result = tauri::async_runtime::spawn_blocking(move || {
        let app = parse_app(&app)?;
        let _guard = provider_write_guard();
        provider_service()
            .update_official(
                app,
                extra_settings.unwrap_or(serde_json::Value::Null),
                common_config_enabled,
                &engine(),
                &client_paths(),
            )
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?;
    if result.is_ok() {
        crate::refresh_tray_menu(&handle);
    }
    result
}

// ---------- 通用配置片段（按 app 各一份） ----------

fn parse_app(app: &str) -> Result<ClientApp, String> {
    app.parse()
}

#[tauri::command]
pub async fn provider_common_snippet(app: String) -> Result<Option<serde_json::Value>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        provider_service()
            .common_snippet(parse_app(&app)?)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 保存/清空某 app 的全局通用配置片段（Claude=settings.json 子集，
/// Codex=config.toml 子树的 JSON 表达；跨供应商共享）。
#[tauri::command]
pub async fn provider_common_snippet_set(
    app: String,
    snippet: Option<serde_json::Value>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = provider_write_guard();
        provider_service()
            .set_common_snippet(parse_app(&app)?, snippet, &engine(), &client_paths())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 多端点测速（对齐 cc-switch test_api_endpoints）：并发测 HTTP 首响应延迟。
#[tauri::command]
pub async fn provider_test_endpoints(
    urls: Vec<String>,
) -> Result<Vec<mcpane_provider::EndpointLatency>, String> {
    Ok(mcpane_provider::test_endpoints(urls).await)
}

/// 拉取供应商可用模型列表。
/// 地址恒用表单传入的 base_url；Key 来源：新增时用表单输入的 api_key，
/// 编辑时 api_key 留空、改传 id 由后端从 Keychain 物化。
#[tauri::command(rename_all = "camelCase")]
pub async fn provider_fetch_models(
    base_url: String,
    api_key: Option<String>,
    id: Option<String>,
    models_url: Option<String>,
) -> Result<Vec<String>, String> {
    let key = match api_key.filter(|k| !k.trim().is_empty()) {
        Some(key) => key,
        None => {
            let id = id.ok_or_else(|| "请先填写 API Key".to_owned())?;
            // Keychain 是同步阻塞调用，别占 async 线程（本文件其余同类操作
            // 都走 spawn_blocking，这里此前是漏网之鱼）。
            tauri::async_runtime::spawn_blocking(move || {
                provider_service()
                    .reveal_key(&id)
                    .map(|key| key.expose().to_owned())
                    .map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| e.to_string())??
        }
    };
    mcpane_provider::fetch_models(&base_url, &key, models_url.as_deref()).await
}

// ---------- 统一供应商（Universal Provider） ----------

#[tauri::command]
pub async fn provider_unified_list() -> Result<Vec<UnifiedProvider>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        provider_service().list_unified().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 新建/更新统一供应商（保存即同步影子条目到启用的 app）。
#[tauri::command]
pub async fn provider_unified_upsert(
    handle: tauri::AppHandle,
    unified: UnifiedProvider,
) -> Result<UnifiedProvider, String> {
    let result = tauri::async_runtime::spawn_blocking(move || {
        let _guard = provider_write_guard();
        provider_service()
            .upsert_unified(unified, &engine(), &client_paths())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?;
    if result.is_ok() {
        crate::refresh_tray_menu(&handle);
    }
    result
}

#[tauri::command]
pub async fn provider_unified_delete(handle: tauri::AppHandle, id: String) -> Result<(), String> {
    let result = tauri::async_runtime::spawn_blocking(move || {
        let _guard = provider_write_guard();
        provider_service()
            .delete_unified(&id)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?;
    if result.is_ok() {
        crate::refresh_tray_menu(&handle);
    }
    result
}

/// 录入统一供应商 API Key（同时下发到各影子条目的 Secret 副本）。
#[tauri::command]
pub async fn provider_unified_set_api_key(id: String, api_key: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = provider_write_guard();
        provider_service()
            .set_unified_api_key(&id, &Secret::new(api_key), &engine(), &client_paths())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- 托盘支撑（TASK-602） ----------

/// 托盘菜单条目：(id, name, 是否当前生效)。
pub type TrayProviderEntry = (String, String, bool);

/// 托盘菜单用：某 app 是否开启了本地路由（读失败按未开启）。
pub fn tray_routing_enabled(app: ClientApp) -> bool {
    provider_service()
        .routing(app)
        .is_ok_and(|config| config.enabled)
}

/// 托盘菜单用：按 app 分组的 Provider 列表（同步，托盘构建期调用）。
///
/// 返回 map 而不是元组——新增客户端时托盘侧不用再改签名与解构，
/// 按 `ClientApp::ALL` 遍历即可（多客户端下元组很快就没法看了）。
pub fn tray_providers() -> std::collections::BTreeMap<ClientApp, Vec<TrayProviderEntry>> {
    let service = provider_service();
    let (profiles, current) = match (service.list(), service.current()) {
        (Ok(p), Ok(c)) => (p, c),
        _ => return std::collections::BTreeMap::new(),
    };
    let mut grouped: std::collections::BTreeMap<ClientApp, Vec<TrayProviderEntry>> = ClientApp::ALL
        .into_iter()
        .map(|app| (app, Vec::new()))
        .collect();
    for p in profiles {
        let is_current = current.get(&p.app) == Some(&p.id);
        grouped
            .entry(p.app)
            .or_default()
            .push((p.id, p.name, is_current));
    }
    grouped
}

/// 后台定时刷新托盘配额缓存：只刷新已缓存、且仍是所属 app 当前生效项的
/// Provider——不主动查从未查过的 Provider，避免后台任务第一次接触某个
/// 中转站凭据时意外弹出 Keychain 授权提示（那种首次接触应该只发生在用户
/// 自己点了"查询用量"之后）。官方订阅不在查询范围内。
pub(crate) async fn refresh_cached_provider_quotas(app: &tauri::AppHandle) {
    let current_ids: std::collections::HashSet<String> = tray_providers()
        .into_values()
        .flatten()
        .filter(|(_, _, is_current)| *is_current)
        .map(|(id, _, _)| id)
        .collect();
    let mut targets: Vec<String> = crate::quota_cache::cached_ids()
        .into_iter()
        .filter(|id| current_ids.contains(id))
        .collect();
    // 「额度优先」路由的候选（中转站）即使没查过也要有额度数据，排序才有依据。
    targets.extend(crate::desktop_proxy::quota_aware_candidate_ids());
    targets.sort();
    targets.dedup();
    if refresh_quota_targets(targets).await {
        crate::tray::refresh_tray_menu(app);
    }
}

/// 逐个查询并写入额度缓存；返回缓存是否有变化。
/// 瞬时网络失败（Err）保留上一份；确定性失败（如登录过期）要覆盖，免得托盘挂着旧数字。
async fn refresh_quota_targets(ids: Vec<String>) -> bool {
    let mut refreshed = false;
    for id in ids {
        if let Ok(quota) = provider_service().quota(&id).await {
            refreshed |= crate::quota_cache::record(id, quota);
        }
    }
    refreshed
}

/// 把一条路径搬到覆盖目录下（保留文件名）。
///
/// 各客户端的配置文件都住在**同一个目录**里（`~/.claude/`、`~/.codex/`、
/// `~/.gemini/` …），所以换目录只需把 file_name 接到新目录——adapter 的
/// join 逻辑一行都不用改。Claude Desktop 是唯一例外（四文件跨两目录），
/// 它在 `set_config_dir_override` 那层就被挡掉了。
fn relocate(path: std::path::PathBuf, dir: &std::path::Path) -> std::path::PathBuf {
    path.file_name().map(|name| dir.join(name)).unwrap_or(path)
}

/// 客户端配置路径（统一走 ClientAdapter discovery，遵循 `CODEX_HOME` 语义；
/// 用户设了配置目录覆盖时改指覆盖目录）。
pub(crate) fn client_paths() -> ClientPaths {
    let home = user_home();
    let overrides = provider_service()
        .config_dir_overrides()
        .unwrap_or_default();
    let with_override = |app: ClientApp, path: std::path::PathBuf| match overrides.get(&app) {
        Some(dir) => relocate(path, std::path::Path::new(dir)),
        None => path,
    };
    let claude = adapter_for(ClientApp::Claude).discover_user_paths(&home);
    let codex = adapter_for(ClientApp::Codex).discover_user_paths(&home);
    let gemini = adapter_for(ClientApp::Gemini).discover_user_paths(&home);
    let grok = adapter_for(ClientApp::Grok).discover_user_paths(&home);
    let opencode = adapter_for(ClientApp::OpenCode).discover_user_paths(&home);
    let openclaw = adapter_for(ClientApp::OpenClaw).discover_user_paths(&home);
    let hermes = adapter_for(ClientApp::Hermes).discover_user_paths(&home);
    let codex_auth = codex
        .auth_file
        .unwrap_or_else(|| codex.live_config.with_file_name("auth.json"));
    let gemini_env = gemini
        .env_file
        .unwrap_or_else(|| gemini.live_config.with_file_name(".env"));
    ClientPaths {
        claude_settings: with_override(ClientApp::Claude, claude.live_config),
        codex_config: with_override(ClientApp::Codex, codex.live_config),
        codex_auth: with_override(ClientApp::Codex, codex_auth),
        gemini_env: with_override(ClientApp::Gemini, gemini_env),
        gemini_settings: with_override(ClientApp::Gemini, gemini.live_config),
        grok_config: with_override(ClientApp::Grok, grok.live_config),
        opencode_config: with_override(ClientApp::OpenCode, opencode.live_config),
        openclaw_config: with_override(ClientApp::OpenClaw, openclaw.live_config),
        hermes_config: with_override(ClientApp::Hermes, hermes.live_config),
        // Desktop 不支持覆盖（四文件跨两目录）。
        claude_desktop: claude_desktop::discover_paths(&home),
    }
}

/// 同步切换（托盘与 provider_switch 命令共用；走安全引擎与 auth 备份/恢复）。
pub fn switch_provider_sync(app: &str, provider_id: &str) -> Result<SwitchOutcome, String> {
    let app = parse_app(app)?;
    let _guard = provider_write_guard();
    let mut outcome = provider_service()
        .switch(app, provider_id, &engine(), &client_paths())
        .map_err(|e| e.to_string())?;
    // Codex/Gemini 从环境变量读到的值优先级高于我们写的配置：切换会显示成功、
    // 实际却仍走旧值。只在真写了配置时查——no-op 切换没必要打扰用户。
    if outcome.written {
        match app {
            ClientApp::Codex => outcome
                .warnings
                .extend(crate::envsync::conflict::codex_credential_conflicts()),
            ClientApp::Gemini => outcome
                .warnings
                .extend(crate::envsync::conflict::gemini_credential_conflicts()),
            // Claude 不检测 env 冲突（settings.json env 覆盖 shell）；Desktop
            // 只读自身 profile，无 shell env 交互。
            ClientApp::Claude => {}
            // Grok / OpenCode 的 Key 直接写进各自的配置文件，不读 shell env。
            ClientApp::Grok | ClientApp::OpenCode | ClientApp::OpenClaw | ClientApp::Hermes => {}
            // Desktop 切换后同步本地网关：proxy 模式拉起并刷新上游快照，
            // 官方/direct 停机释放端口。失败进 warnings（切换本身已成功）。
            ClientApp::ClaudeDesktop => {
                if let Err(error) = crate::desktop_proxy::sync_from_store_blocking() {
                    outcome.warnings.push(format!(
                        "本地网关同步失败，Claude Desktop 可能暂时无法对话：{error}"
                    ));
                }
            }
        }
    }
    // 路由快照的主候选跟随当前项（不看 written：同一项重切也要刷新 Key/模型）。
    if app.supports_routing()
        && let Err(error) = crate::desktop_proxy::sync_from_store_blocking()
    {
        outcome
            .warnings
            .push(format!("本地路由同步失败，请求可能仍发往原供应商：{error}"));
    }
    // 接管体验（幂等，故不看 written；no-op 时是修复路径）：切中转抹平
    // 官方登录摩擦，切官方清理我们写的插件键。失败降级 warning 不阻断。
    if app == ClientApp::Claude {
        apply_claude_takeover(provider_id, &mut outcome.warnings);
    }
    Ok(outcome)
}

/// Claude 接管体验（best-effort）：
/// - 切中转：`~/.claude.json` 置 `hasCompletedOnboarding=true`（CLI 不再弹
///   首启引导）；`<claude_dir>/config.json` 置 `primaryApiKey="any"`（插件/
///   引导接受任意 Key，不再要求官方登录）。
/// - 切官方：仅清我们写的 `primaryApiKey="any"`；onboarding 标记是事实、保留。
///
/// 两个文件都不归引擎管（非受管配置、无凭据），增量读改写 + 原子落盘；
/// 解析失败绝不覆盖用户数据（cc-switch 对坏文件重置为 `{}`，刻意偏离）。
fn apply_claude_takeover(provider_id: &str, warnings: &mut Vec<String>) {
    use mcpane_adapter_claude::takeover;
    let home = user_home();
    let state_path = mcpane_adapter_claude::discover_config_paths(&home, None).user_config;
    let plugin_path = mcpane_adapter_claude::claude_config_dir(&home).join("config.json");
    let to_official = provider_id == mcpane_provider::OFFICIAL_CLAUDE_ID;

    type TakeoverEditFn = dyn Fn(Option<&str>) -> Result<Option<String>, String>;
    let mut apply = |path: &std::path::Path, label: &str, edit: &TakeoverEditFn| {
        let existing = match std::fs::read_to_string(path) {
            Ok(content) => Some(content),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                warnings.push(format!("{label} 读取失败，接管标记未写入：{error}"));
                return;
            }
        };
        match edit(existing.as_deref()) {
            Ok(Some(next)) => {
                if let Err(error) = write_small_json_atomically(path, &next) {
                    warnings.push(format!("{label} 写入失败，接管标记未生效：{error}"));
                }
            }
            Ok(None) => {}
            Err(reason) => {
                warnings.push(format!("{label} 内容异常（{reason}），已保持原样未改动"));
            }
        }
    };

    if to_official {
        apply(&plugin_path, "插件配置 config.json", &|c| {
            Ok(takeover::plugin_key_clear(c))
        });
    } else {
        apply(
            &state_path,
            "CLI 状态 .claude.json",
            &takeover::onboarding_upsert,
        );
        apply(
            &plugin_path,
            "插件配置 config.json",
            &takeover::plugin_key_upsert,
        );
    }
}

/// 小状态文件的原子写（tmp + rename；无凭据内容，不需要 0600/fsync 强度）。
fn write_small_json_atomically(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    mcpane_config::atomic_io::atomic_write(path, content, false, false)
}

/// 读取 app 的 live 配置（剔除 MCPane 受管键），作为编辑弹窗「配置片段」的
/// 展示基线（对齐 cc-switch 编辑器展示整份当前配置的体验；只读，不写）。
#[tauri::command]
pub async fn provider_live_settings(app: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let app = parse_app(&app)?;
        let (path, format) = live_settings_source(app);
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let Ok(doc) = mcpane_config::ConfigDocument::parse(&content, format) else {
            return Ok(serde_json::json!({}));
        };
        let mut value = doc.get(&[]).unwrap_or_else(|| serde_json::json!({}));
        if let Some(obj) = value.as_object_mut() {
            strip_managed_keys(app, obj);
        }
        Ok(value)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// `provider_live_settings` 的读取源：客户端配置文件路径 + 解析格式。
fn live_settings_source(app: ClientApp) -> (std::path::PathBuf, mcpane_config::ConfigFormat) {
    let paths = client_paths();
    match app {
        ClientApp::Claude => (paths.claude_settings, mcpane_config::ConfigFormat::Json),
        ClientApp::Codex => (paths.codex_config, mcpane_config::ConfigFormat::Toml),
        // Gemini 的片段目标是 settings.json（.env 受管键不经片段表达）。
        ClientApp::Gemini => (paths.gemini_settings, mcpane_config::ConfigFormat::Json),
        ClientApp::Grok => (paths.grok_config, mcpane_config::ConfigFormat::Toml),
        // 文件名是 .json，内容是 JSONC/JSON5：OpenCode 官方文档的示例本身
        // 就带 `//` 注释与尾逗号，走严格 Json 解析预览必炸，落到下面的
        // `Err` 兜底显示成空 `{}`，误导用户以为配置是空的。写入路径
        // （adapter.rs 的 OpenCodeAdapter::live_config）早就用 Json5，
        // 这里之前漏改，读写用了两种格式（P2，2026-07-28 IPC 契约审计发现）。
        ClientApp::OpenCode => (paths.opencode_config, mcpane_config::ConfigFormat::Json5),
        // 文件名是 .json，内容是 JSON5（无引号键/单引号/注释）。
        ClientApp::OpenClaw => (paths.openclaw_config, mcpane_config::ConfigFormat::Json5),
        ClientApp::Hermes => (paths.hermes_config, mcpane_config::ConfigFormat::Yaml),
        // Desktop 的 live 即我们的 gateway profile（含明文网关 key，受管键隐藏）。
        ClientApp::ClaudeDesktop => (
            paths.claude_desktop.profile,
            mcpane_config::ConfigFormat::Json,
        ),
    }
}

/// 剥离编辑器预览里不该展示的 MCPane 受管键，按客户端分派到各自的剥离规则。
fn strip_managed_keys(app: ClientApp, obj: &mut serde_json::Map<String, serde_json::Value>) {
    match app {
        ClientApp::Claude => strip_claude_managed(obj),
        ClientApp::Codex => strip_codex_managed(obj),
        ClientApp::OpenCode => strip_opencode_managed(obj),
        ClientApp::Hermes => strip_hermes_managed(obj),
        ClientApp::OpenClaw => strip_openclaw_managed(obj),
        ClientApp::Grok => strip_grok_managed(obj),
        ClientApp::Gemini => strip_gemini_managed(obj),
        ClientApp::ClaudeDesktop => strip_claude_desktop_managed(obj),
    }
}

fn strip_claude_managed(obj: &mut serde_json::Map<String, serde_json::Value>) {
    if let Some(env) = obj.get_mut("env").and_then(|v| v.as_object_mut()) {
        for key in mcpane_adapter_claude::PROVIDER_ENV_KEYS {
            env.remove(key);
        }
    }
}

/// 只隐藏 MCPane 受管键（切换中转站时写入、切走时清理），其余内容
/// 与 ~/.codex/config.toml 全文一致——编辑器必须让用户认出自己的配置文件
/// （对齐 cc-switch 的完整展示）。
fn strip_codex_managed(obj: &mut serde_json::Map<String, serde_json::Value>) {
    obj.remove("model_provider");
    obj.remove("disable_response_storage");
    obj.remove("model_context_window");
    obj.remove("model_auto_compact_token_limit");
    // model_providers 表里用户可能有自建条目，只摘 MCPane 的 slug。
    let slugs: Vec<String> = provider_service()
        .list()
        .map(|profiles| {
            profiles
                .into_iter()
                .filter(|p| p.app == ClientApp::Codex)
                .filter_map(|p| p.codex_slug)
                .collect()
        })
        .unwrap_or_default();
    if let Some(tables) = obj
        .get_mut("model_providers")
        .and_then(|v| v.as_object_mut())
    {
        for slug in slugs {
            tables.remove(&slug);
        }
        if tables.is_empty() {
            obj.remove("model_providers");
        }
    }
}

/// provider 表整块由启用/移除流程管；mcp、theme 等其他键全文保留。
fn strip_opencode_managed(obj: &mut serde_json::Map<String, serde_json::Value>) {
    obj.remove(mcpane_adapter_opencode::PROVIDER_TABLE_KEY);
}

/// 供应商数组整块由启用/移除流程管；agent、mcp_servers 等保留。
fn strip_hermes_managed(obj: &mut serde_json::Map<String, serde_json::Value>) {
    obj.remove(mcpane_adapter_hermes::CUSTOM_PROVIDERS_KEY);
}

/// 只摘 models.providers；models.mode 等全局取舍留给用户看。路径取 adapter
/// 的常量而不是写字面量——adapter 改了这里要跟着变。
fn strip_openclaw_managed(obj: &mut serde_json::Map<String, serde_json::Value>) {
    let [parent, leaf] = mcpane_adapter_openclaw::PROVIDERS_PATH;
    if let Some(models) = obj.get_mut(parent).and_then(|v| v.as_object_mut()) {
        models.remove(leaf);
    }
}

/// 只摘掉 MCPane 写的模型表（mcpane- 前缀）与被我们改写的默认指针；用户
/// 自建的模型表与其他键全文保留。键路径同样取 adapter 常量，别在这里另写
/// 一份字面量。
fn strip_grok_managed(obj: &mut serde_json::Map<String, serde_json::Value>) {
    let [models_key, default_key] = mcpane_adapter_grok::MODELS_DEFAULT_PATH;
    let ours_is_default = obj
        .get(models_key)
        .and_then(|m| m.get(default_key))
        .and_then(|v| v.as_str())
        .is_some_and(|slug| slug.starts_with(mcpane_adapter_grok::SLUG_PREFIX));
    if ours_is_default && let Some(models) = obj.get_mut(models_key).and_then(|v| v.as_object_mut())
    {
        models.remove(default_key);
        if models.is_empty() {
            obj.remove(models_key);
        }
    }
    if let Some(tables) = obj
        .get_mut(mcpane_adapter_grok::MODEL_TABLE_KEY)
        .and_then(|v| v.as_object_mut())
    {
        tables.retain(|slug, _| !slug.starts_with(mcpane_adapter_grok::SLUG_PREFIX));
        if tables.is_empty() {
            obj.remove(mcpane_adapter_grok::MODEL_TABLE_KEY);
        }
    }
}

/// 只隐藏 MCPane 受管的 auth 类型键，mcpServers 等全文保留。
fn strip_gemini_managed(obj: &mut serde_json::Map<String, serde_json::Value>) {
    if let Some(auth) = obj
        .get_mut("security")
        .and_then(|v| v.get_mut("auth"))
        .and_then(|v| v.as_object_mut())
    {
        auth.remove("selectedType");
    }
}

/// profile 全部网关键由 MCPane 生成（含明文 key），一律隐藏；用户手工加的
/// 其他键保留可见。
fn strip_claude_desktop_managed(obj: &mut serde_json::Map<String, serde_json::Value>) {
    for key in claude_desktop::PROFILE_MANAGED_KEYS {
        obj.remove(key);
    }
}

// ---------- Claude Desktop 状态与本地网关 ----------

/// Claude Desktop 状态面（对齐 cc-switch ClaudeDesktopStatus 字段语义；
/// expected/actual 都给前端，比较与提示文案由前端负责）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeDesktopStatusDto {
    pub supported: bool,
    pub configured: bool,
    pub applied_id: Option<String>,
    pub actual_base_url: Option<String>,
    /// profile 中存在非 claude-safe 模型名（新版 Desktop 会 fail-all 拒收）。
    pub stale_raw_models: bool,
    /// 当前生效供应商的模式（官方/未切换为 null）。
    pub mode: Option<DesktopMode>,
    pub expected_base_url: Option<String>,
    /// proxy 模式但路由表解析失败（Desktop 将拿不到可用模型）。
    pub missing_route_mappings: bool,
    pub gateway_token_configured: bool,
    pub proxy_running: bool,
    pub profile_path: String,
}

#[tauri::command]
pub async fn claude_desktop_status() -> Result<ClaudeDesktopStatusDto, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let svc = provider_service();
        let paths = client_paths();
        let live = claude_desktop::read_live_status(&paths.claude_desktop);
        let current = svc.current().map_err(|e| e.to_string())?;
        let profiles = svc.list().map_err(|e| e.to_string())?;
        let current_profile = current
            .get(&ClientApp::ClaudeDesktop)
            .and_then(|id| profiles.iter().find(|p| p.id == *id))
            .filter(|p| p.category == ProviderCategory::Relay);
        let gateway = svc.desktop_gateway().map_err(|e| e.to_string())?;
        let mode = current_profile.map(|p| p.desktop_mode);
        let expected_base_url = current_profile.and_then(|p| match p.desktop_mode {
            DesktopMode::Direct => p
                .base_url
                .as_deref()
                .map(|url| url.trim_end_matches('/').to_owned()),
            DesktopMode::Proxy => gateway
                .as_ref()
                .map(|gw| claude_desktop::proxy_gateway_base_url(gw.listen_port)),
        });
        let missing_route_mappings = current_profile.is_some_and(|p| {
            p.desktop_mode == DesktopMode::Proxy
                && claude_desktop::resolve_proxy_routes(&p.desktop_routes).is_err()
        });
        Ok(ClaudeDesktopStatusDto {
            supported: claude_desktop::is_supported_platform(),
            configured: live.configured,
            applied_id: live.applied_id,
            actual_base_url: live.actual_base_url,
            stale_raw_models: live.stale_raw_models,
            mode,
            expected_base_url,
            missing_route_mappings,
            gateway_token_configured: gateway.is_some(),
            proxy_running: crate::desktop_proxy::is_running(),
            profile_path: paths.claude_desktop.profile.display().to_string(),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 从 Claude Code 供应商批量导入兼容的 Claude Desktop 供应商
///（对齐 cc-switch import_claude_desktop_providers_from_claude）。返回导入条数。
#[tauri::command]
pub async fn claude_desktop_import_from_claude(handle: tauri::AppHandle) -> Result<usize, String> {
    let result = tauri::async_runtime::spawn_blocking(|| {
        let _guard = provider_write_guard();
        provider_service()
            .import_desktop_from_claude()
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?;
    if result.is_ok() {
        crate::refresh_tray_menu(&handle);
    }
    result
}

// ---------- 本地路由（ADR-022） ----------

/// 按路由开关重写当前项 live（启动重新接管用）。
pub(crate) fn reapply_routing(app: ClientApp) -> Result<(), String> {
    let _guard = provider_write_guard();
    provider_service()
        .reapply_routing(app, &engine(), &client_paths())
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// 未开路由却仍指向路由的 live（强杀残留）按直连重写（启动自愈用）。
pub(crate) fn heal_stale_takeover(app: ClientApp) -> Result<bool, String> {
    let _guard = provider_write_guard();
    provider_service()
        .heal_stale_takeover(app, &engine(), &client_paths())
        .map_err(|e| e.to_string())
}

/// 当前项按直连重写 live（退出前还原用）。
pub(crate) fn restore_direct(app: ClientApp) -> Result<(), String> {
    let _guard = provider_write_guard();
    restore_direct_unguarded(app)
}

/// 不取写锁的直连还原：只供路由起不来时的应急回退用——它会在已持锁的切换
/// 流程里被调用（锁不可重入），且重写内容完全由 store 决定、幂等。
pub(crate) fn restore_direct_unguarded(app: ClientApp) -> Result<(), String> {
    provider_service()
        .restore_direct(app, &engine(), &client_paths())
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn provider_routing_get(app: String) -> Result<RoutingConfig, String> {
    let app = parse_app(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        provider_service().routing(app).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 保存路由配置：先重写 live，再同步路由快照（开启时拉起路由服务）。
#[tauri::command]
pub async fn provider_routing_set(
    handle: tauri::AppHandle,
    app: String,
    config: RoutingConfig,
) -> Result<RoutingConfig, String> {
    let app = parse_app(&app)?;
    let (saved, previous) = tauri::async_runtime::spawn_blocking(move || {
        let _guard = provider_write_guard();
        let svc = provider_service();
        let previous = svc.routing(app).map_err(|e| e.to_string())?;
        let saved = svc
            .set_routing(app, config, &engine(), &client_paths())
            .map_err(|e| e.to_string())?;
        Ok::<_, String>((saved, previous))
    })
    .await
    .map_err(|e| e.to_string())??;
    crate::refresh_tray_menu(&handle);
    if let Err(error) = crate::desktop_proxy::sync_from_store().await {
        // 路由起不来：同步已把客户端回退直连，这里再把开关恢复原状，界面如实反映。
        let _ = tauri::async_runtime::spawn_blocking(move || {
            let _guard = provider_write_guard();
            provider_service().set_routing(app, previous, &engine(), &client_paths())
        })
        .await;
        let _ = crate::desktop_proxy::sync_from_store().await;
        return Err(format!("本地路由未能启动，已保持直连：{error}"));
    }
    crate::desktop_proxy::record_takeover(app, Ok(()));
    if saved.enabled && saved.strategy == mcpane_provider::RoutingStrategy::QuotaAware {
        // 刚切到额度优先：立刻拉一轮候选额度，不必等下一个 5 分钟周期。
        tauri::async_runtime::spawn(async {
            refresh_quota_targets(crate::desktop_proxy::quota_aware_candidate_ids()).await;
        });
    }
    Ok(saved)
}

#[tauri::command]
pub fn router_status() -> Vec<mcpane_desktop_proxy::AppRouteStatus> {
    crate::desktop_proxy::route_status()
}

/// 最近的路由请求（新 → 旧；`app` 为空取全部）。
#[tauri::command]
pub fn router_recent_requests(
    app: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<mcpane_desktop_proxy::RequestRecord>, String> {
    let app = app.as_deref().map(parse_app).transpose()?;
    Ok(crate::desktop_proxy::recent_requests(
        app,
        limit.unwrap_or(50).min(500),
    ))
}

/// 清空路由请求记录（`app` 为空清全部；各供应商累计用量保留）。
#[tauri::command]
pub fn router_clear_requests(app: Option<String>) -> Result<(), String> {
    let app = app.as_deref().map(parse_app).transpose()?;
    crate::desktop_proxy::clear_requests(app);
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
pub fn router_reset_breaker(app: String, provider_id: String) -> Result<(), String> {
    let app = parse_app(&app)?;
    crate::desktop_proxy::reset_breaker(app, &provider_id);
    Ok(())
}

/// 本地网关运行态 + 配置端口（设置面板与路由开关共用）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopProxyStatusDto {
    pub running: bool,
    pub port: Option<u16>,
    pub has_upstream: bool,
    /// store 中配置的监听端口（未生成网关时为默认值）。
    pub listen_port: u16,
    /// 最近一次同步失败原因（如端口被占用）；成功后为 null。
    pub last_error: Option<String>,
}

fn desktop_proxy_status_dto() -> Result<DesktopProxyStatusDto, String> {
    let status = crate::desktop_proxy::status();
    let listen_port = provider_service()
        .desktop_gateway()
        .map_err(|e| e.to_string())?
        .map(|gw| gw.listen_port)
        .unwrap_or(mcpane_provider::DESKTOP_PROXY_DEFAULT_PORT);
    Ok(DesktopProxyStatusDto {
        running: status.running,
        port: status.port,
        has_upstream: status.has_upstream,
        listen_port,
        last_error: crate::desktop_proxy::last_error(),
    })
}

#[tauri::command]
pub async fn desktop_proxy_status() -> Result<DesktopProxyStatusDto, String> {
    tauri::async_runtime::spawn_blocking(desktop_proxy_status_dto)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn desktop_proxy_start() -> Result<DesktopProxyStatusDto, String> {
    crate::desktop_proxy::start_manual().await?;
    tauri::async_runtime::spawn_blocking(desktop_proxy_status_dto)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn desktop_proxy_stop() -> Result<(), String> {
    crate::desktop_proxy::stop_manual().await
}

/// 修改网关监听端口；运行中禁改（对齐 cc-switch「先停止代理服务」约束）。
#[tauri::command(rename_all = "camelCase")]
pub async fn desktop_proxy_set_port(listen_port: u16) -> Result<(), String> {
    let previous = tauri::async_runtime::spawn_blocking(move || {
        let _guard = provider_write_guard();
        let svc = provider_service();
        let previous = svc
            .desktop_gateway()
            .map_err(|e| e.to_string())?
            .map(|gw| gw.listen_port);
        svc.set_desktop_proxy_port(listen_port)
            .map_err(|e| e.to_string())?;
        Ok::<_, String>(previous)
    })
    .await
    .map_err(|e| e.to_string())??;
    if !crate::desktop_proxy::is_running() {
        return Ok(());
    }
    // 运行中改端口：新端口起来后把所有指向路由的客户端配置改写到新地址；
    // 新端口绑不上则回滚端口，旧实例一直在服务。
    if let Err(error) = crate::desktop_proxy::sync_from_store().await {
        if let Some(previous) = previous {
            let _ = tauri::async_runtime::spawn_blocking(move || {
                let _guard = provider_write_guard();
                provider_service().set_desktop_proxy_port(previous)
            })
            .await;
            let _ = crate::desktop_proxy::sync_from_store().await;
        }
        return Err(format!(
            "端口 {listen_port} 无法使用，已保持原端口：{error}"
        ));
    }
    tauri::async_runtime::spawn_blocking(reapply_router_clients)
        .await
        .map_err(|e| e.to_string())?
}

/// 把所有指向本地路由的客户端配置按当前端口重写（接管中的 app + proxy 模式的 Desktop）。
fn reapply_router_clients() -> Result<(), String> {
    let _guard = provider_write_guard();
    let svc = provider_service();
    let (engine, paths) = (engine(), client_paths());
    for app in svc.routed_apps().map_err(|e| e.to_string())? {
        svc.reapply_routing(app, &engine, &paths)
            .map_err(|e| e.to_string())?;
        crate::desktop_proxy::record_takeover(app, Ok(()));
    }
    let desktop_current = svc
        .current()
        .map_err(|e| e.to_string())?
        .get(&ClientApp::ClaudeDesktop)
        .cloned();
    let is_proxy = |id: &str| {
        svc.list()
            .ok()
            .and_then(|list| list.into_iter().find(|p| p.id == id))
            .is_some_and(|p| p.desktop_mode == DesktopMode::Proxy)
    };
    if let Some(id) = desktop_current.filter(|id| is_proxy(id)) {
        svc.reapply_current(&id, &engine, &paths)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Provider 用量/余额查询（Coding Plan / 按余额计费中转站，按 base_url 自动识别；
/// 官方订阅一律拒绝，不发请求）。结果记入托盘缓存并刷新托盘菜单摘要。
#[tauri::command]
pub async fn provider_quota(
    app: tauri::AppHandle,
    id: String,
) -> Result<mcpane_provider::ProviderQuota, String> {
    let quota = provider_service()
        .quota(&id)
        .await
        .map_err(|e| e.to_string())?;
    if crate::quota_cache::record(id, quota.clone()) {
        crate::tray::refresh_tray_menu(&app);
    }
    Ok(quota)
}

#[tauri::command]
pub async fn provider_test_connectivity(id: String) -> Result<u16, String> {
    provider_service()
        .test_connectivity(&id)
        .await
        .map_err(|e| e.to_string())
}
