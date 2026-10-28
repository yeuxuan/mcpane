//! 设置与应用信息（TASK-506 数据层）。

use super::mcpane_home;

// ---------- 设置（TASK-506 数据层） ----------

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub data_dir: String,
    pub db_path: String,
    pub hub_endpoint: String,
    pub app_version: String,
}

#[tauri::command]
pub fn app_info() -> AppInfo {
    let home = mcpane_home();
    AppInfo {
        data_dir: home.display().to_string(),
        db_path: home.join("mcpane.db").display().to_string(),
        hub_endpoint: mcpane_hub_protocol::HUB_MCP_URL.into(),
        app_version: env!("CARGO_PKG_VERSION").into(),
    }
}

// ---------- Hub 偏好开关 ----------

pub(crate) async fn settings_repo() -> Result<mcpane_storage::SettingsRepo, String> {
    Ok(mcpane_storage::SettingsRepo::new(super::db_pool().await?))
}

#[tauri::command]
pub async fn hub_enabled_get() -> Result<bool, String> {
    settings_repo()
        .await?
        .get_hub_enabled()
        .await
        .map_err(|e| e.to_string())
}

/// 切换 Hub 偏好：写库后立即驱动 supervisor（关闭时杀掉正在托管的子进程）。
#[tauri::command]
pub async fn hub_set_enabled(
    enabled: bool,
    supervisor: tauri::State<'_, std::sync::Arc<crate::hub_process::HubSupervisor>>,
) -> Result<(), String> {
    settings_repo()
        .await?
        .set_hub_enabled(enabled)
        .await
        .map_err(|e| e.to_string())?;
    supervisor.set_enabled(enabled);
    Ok(())
}

/// 启动静默检查更新的开关。
#[tauri::command]
pub async fn auto_update_check_get() -> Result<bool, String> {
    settings_repo()
        .await?
        .get_auto_update_check()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn auto_update_check_set(enabled: bool) -> Result<(), String> {
    settings_repo()
        .await?
        .set_auto_update_check(enabled)
        .await
        .map_err(|e| e.to_string())
}

// ---------- 全局出网代理（ADR-017） ----------

#[tauri::command]
pub async fn proxy_get() -> Result<mcpane_net::ProxyConfig, String> {
    settings_repo()
        .await?
        .get_proxy()
        .await
        .map_err(|e| e.to_string())
}

/// 保存全局出网代理配置。
///
/// 顺序是「先试构建 → 再写库 → 再驱动运行时」：
/// - 试构建走的是与真实构建**同一条路**（`check` 内部就是 build），因此
///   「校验过了、驱动却失败」的窗口不存在——不然会落得库里存新值、运行态跑
///   旧值、界面显示第三种；
/// - 写库先于驱动，DB 写失败时运行态不会偷跑到与持久化不一致的状态。
///
/// 桌面端与 Hub 是两个进程、各有一套 reqwest 客户端，故两边都要驱动：
/// 本进程原地换客户端，Hub 走控制面重连下游。
#[tauri::command]
pub async fn proxy_set(config: mcpane_net::ProxyConfig) -> Result<(), String> {
    config.check().map_err(|e| e.to_string())?;

    settings_repo()
        .await?
        .set_proxy(&config)
        .await
        .map_err(|e| e.to_string())?;

    mcpane_net::set_proxy(&config).map_err(|e| e.to_string())?;

    // Hub 是独立进程，自有一套客户端。ReloadServers 会让它重读配置并按新代理
    // 重连全部下游。
    //
    // 失败不阻断保存：Hub 没在跑时连不上是常态，它下次启动会自己从库里读
    // （main.rs 启动即 load_downstreams）。但「Hub 在跑却没收到」是异常——
    // 它会带着旧代理一直跑到下次 reload，不记一笔的话没人知道。
    if let Err(error) =
        super::control_call(mcpane_hub_protocol::ControlCommand::ReloadServers).await
    {
        eprintln!("[mcpane] 通知 Hub 重载代理失败（Hub 未运行则属正常）: {error}");
    }

    Ok(())
}

/// 扫描本机可用代理（实测通过的才返回，见 mcpane-net::scan）。
#[tauri::command]
pub async fn proxy_scan() -> Vec<mcpane_net::scan::DetectedProxy> {
    mcpane_net::scan::scan_local_proxies().await
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyTestResult {
    pub ok: bool,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
}

/// 实测某个代理地址是否可用。不落库，纯探测。
#[tauri::command]
pub async fn proxy_test(url: String) -> ProxyTestResult {
    match mcpane_net::scan::measure_through_proxy(&url).await {
        Ok(latency_ms) => ProxyTestResult {
            ok: true,
            latency_ms: Some(latency_ms),
            error: None,
        },
        Err(error) => ProxyTestResult {
            ok: false,
            latency_ms: None,
            error: Some(error),
        },
    }
}

/// 用系统默认浏览器打开外部链接（供应商官网 / 获取 API Key 页）。
/// 只放行 http(s)，防止任意 scheme（file:/自定义协议）被前端注入执行。
#[tauri::command]
pub fn open_external(handle: tauri::AppHandle, url: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;

    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("仅支持打开 http(s) 链接".into());
    }
    handle
        .opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}
