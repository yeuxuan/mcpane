//! 自动更新（TASK-703 子项）：检查/下载安装/重启，全部走 Rust 侧命令，
//! 前端只 invoke——不引入 @tauri-apps/plugin-updater，也无需 capabilities 权限。
//!
//! 代理：updater 插件自带独立 reqwest，不经 mcpane-net 的出网客户端；
//! Manual 模式下把用户配的代理显式注入 builder（System 模式插件自会读
//! 环境变量，Direct 模式对公网端点即默认直连），否则改了代理的用户会看到
//! 「哪儿都能上就是检查不了更新」。

use serde::Serialize;
use tauri_plugin_updater::UpdaterExt;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub available: bool,
    pub current_version: String,
    pub version: Option<String>,
    pub notes: Option<String>,
}

/// 构造 updater（挂代理 + 安装前关停 hub 后 build）。
async fn build_updater(app: &tauri::AppHandle) -> Result<tauri_plugin_updater::Updater, String> {
    let mut builder = app.updater_builder();
    if let Ok(mcpane_net::ProxyConfig::Manual { url }) = super::proxy_get().await
        && let Ok(url) = url.parse()
    {
        builder = builder.proxy(url);
    }
    // Windows：插件拉起 NSIS 安装器后 std::process::exit(0) 直退，RunEvent::Exit
    // 不触发；hub 若还活着就锁死安装目录里的 mcpane-hub.exe，安装器写文件报错。
    // on_before_exit 恰好在 ShellExecuteW 之前同步执行，是唯一的可靠时机。
    builder = builder.on_before_exit({
        let app = app.clone();
        move || crate::hub_process::shutdown_before_update(&app)
    });
    builder.build().map_err(|e| e.to_string())
}

/// 检查是否有新版本（不下载）。
pub async fn check(app: &tauri::AppHandle) -> Result<UpdateStatus, String> {
    let updater = build_updater(app).await?;
    let current_version = env!("CARGO_PKG_VERSION").to_owned();
    match updater.check().await.map_err(|e| e.to_string())? {
        Some(update) => Ok(UpdateStatus {
            available: true,
            current_version,
            version: Some(update.version.clone()),
            notes: update.body.clone(),
        }),
        None => Ok(UpdateStatus {
            available: false,
            current_version,
            version: None,
            notes: None,
        }),
    }
}

#[tauri::command]
pub async fn update_check(app: tauri::AppHandle) -> Result<UpdateStatus, String> {
    check(&app).await
}

/// 下载并安装（重复调用幂等——没更新就报错）。
/// macOS/Linux：就地替换后返回，由用户点「重启」生效；
/// Windows：插件在此拉起 NSIS 安装器并 exit(0)，本调用不会返回。
#[tauri::command]
pub async fn update_install(app: tauri::AppHandle) -> Result<(), String> {
    let updater = build_updater(&app).await?;
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "当前已是最新版本".to_owned())?;
    update
        .download_and_install(|_received, _total| {}, || {})
        .await
        .map_err(|e| e.to_string())
}

/// 重启应用以应用更新（sidecar Hub 由 RunEvent::Exit 优雅关停，重启后 adopt/重拉）。
/// Windows 走不到这里——那条路径的关停在 on_before_exit 里做。
#[tauri::command]
pub async fn update_restart(app: tauri::AppHandle) {
    app.restart();
}
