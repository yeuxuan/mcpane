//! 主窗口生命周期：托盘隐藏/唤起、轻量模式下的 WebView 延迟销毁与重建、
//! macOS Dock 图标可见性。从 `main.rs` 拆出——应用装配（Builder/setup/run
//! 事件分发）与这里的具体实现是两层不同的关注点。

/// 隐藏后延迟销毁 WebView 的等待时长：误关后快速重开仍是秒开（快路径），
/// 久驻后台才真正拆掉 WebView 释放内存（约 250MB，见轻量模式计划）。
const DESTROY_DELAY: std::time::Duration = std::time::Duration::from_secs(60);

/// 退托盘代数：每次 hide_to_tray 递增、show_main_window 也递增，
/// 延迟销毁任务据此识别自己是否已过期（期间窗口被重开过则不得销毁）。
static HIDE_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 首次显示的门控（见 `on_app_ready` / `window_ready`）。
static APP_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static PAGE_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static FIRST_SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 前端首帧迟迟不来时的兜底：宁可闪一下也不能让用户以为没启动。
const FIRST_SHOW_FALLBACK: std::time::Duration = std::time::Duration::from_millis(1500);

/// `RunEvent::Ready`：AppKit 已 finishLaunching，从此刻起才能 show()。
///
/// 但不在这里直接显示——WebView 通常还没画出第一帧，先出窗口就是一块
/// 白/灰底再跳成主题色的闪屏（Windows 上原生 setTheme 还是异步的，更明显）。
/// 改为等前端挂载完成后经 `window_ready` 命令来揭幕；两边谁后到谁揭幕，
/// 再加一个超时兜底。开机自启拉起的那次本就不出窗口，直接跳过。
pub(crate) fn on_app_ready(app: &tauri::AppHandle) {
    use std::sync::atomic::Ordering;

    if crate::commands::launched_by_autostart() {
        return;
    }
    APP_READY.store(true, Ordering::SeqCst);
    if PAGE_READY.load(Ordering::SeqCst) {
        reveal_on_launch(app);
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_SHOW_FALLBACK).await;
        reveal_on_launch(&app);
    });
}

/// 前端首帧已提交：App 根组件挂载后调用。Ready 还没到就只记账，交给 Ready 揭幕。
pub(crate) fn on_page_ready(app: &tauri::AppHandle) {
    use std::sync::atomic::Ordering;

    PAGE_READY.store(true, Ordering::SeqCst);
    if APP_READY.load(Ordering::SeqCst) {
        reveal_on_launch(app);
    }
}

/// 启动揭幕只做一次；之后的显示（托盘唤起/二次启动/Reopen）走 `show_main_window`。
fn reveal_on_launch(app: &tauri::AppHandle) {
    use std::sync::atomic::Ordering;

    if FIRST_SHOWN.swap(true, Ordering::SeqCst) {
        return;
    }
    // 调用方可能在 IPC 线程或 tokio 定时器线程；AppKit 的 show/makeKeyAndOrderFront
    // 只认主线程，派回去再做。
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || show_main_window(&handle));
}

/// 退到托盘：隐藏主窗口，Windows 同时藏任务栏图标，macOS 藏 Dock 图标；
/// 60s 后窗口仍处于隐藏态则销毁 WebView 释放内存（唤起时重建）。
pub(crate) fn hide_to_tray(app: &tauri::AppHandle) {
    use tauri::Manager;
    use tauri_plugin_window_state::AppHandleExt;

    if let Some(window) = app.get_webview_window("main") {
        // 销毁前落盘几何，重建时由 window-state 插件恢复；
        // 必须在 hide 之前保存，避免个别平台隐藏后读不到位置。
        let _ = app.save_window_state(crate::WINDOW_STATE_FLAGS);
        let _ = window.hide();
        #[cfg(target_os = "windows")]
        {
            let _ = window.set_skip_taskbar(true);
        }
    }
    schedule_webview_destroy(app);
    #[cfg(target_os = "macos")]
    set_dock_visible(app, false);
}

/// 开机自启拉起：窗口从一开始就没显示过，直接进托盘常驻。
///
/// 与 [`hide_to_tray`] 的差别只在**不落盘窗口几何**：此刻的窗口从未上过屏，
/// 各平台报出的位置未必是用户上次摆的那个，存回去会把它挪走——用户开机后
/// 第一次打开面板，窗口就跑到了别处。
pub(crate) fn stay_in_tray_on_launch(app: &tauri::AppHandle) {
    #[cfg(target_os = "windows")]
    {
        use tauri::Manager;
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.set_skip_taskbar(true);
        }
    }
    // 开机自启尤其该走轻量模式：多数人开机后半天不会点开面板，
    // 白扛 250MB 的 WebView 没有道理。
    schedule_webview_destroy(app);
    #[cfg(target_os = "macos")]
    set_dock_visible(app, false);
}

/// 安排 [`DESTROY_DELAY`] 后销毁 WebView：期间窗口被唤起过则本次作废。
fn schedule_webview_destroy(app: &tauri::AppHandle) {
    use std::sync::atomic::Ordering;

    use tauri::Manager;

    let generation = HIDE_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(DESTROY_DELAY).await;
        // 代数变了 = 等待期间窗口被重开过（或又关了一次，交给新任务）。
        if HIDE_GENERATION.load(Ordering::SeqCst) != generation {
            return;
        }
        if let Some(window) = app.get_webview_window("main")
            && !window.is_visible().unwrap_or(true)
        {
            // destroy 触发 ExitRequested(code=None)，由既有拦截分支防退出。
            let _ = window.destroy();
        }
    });
}

/// 从托盘/Dock 恢复主窗口：还原任务栏/Dock 可见性后取消最小化、显示并聚焦；
/// 窗口已被轻量模式销毁时先按配置重建。
pub(crate) fn show_main_window(app: &tauri::AppHandle) {
    use std::sync::atomic::Ordering;

    use tauri::Manager;

    #[cfg(target_os = "macos")]
    set_dock_visible(app, true);
    // 使等待中的延迟销毁任务过期：窗口正在被唤起。
    HIDE_GENERATION.fetch_add(1, Ordering::SeqCst);

    let window = app
        .get_webview_window("main")
        .or_else(|| rebuild_main_window(app));
    if let Some(window) = window {
        #[cfg(target_os = "windows")]
        {
            let _ = window.set_skip_taskbar(false);
        }
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// 轻量态唤起：按 tauri.conf.json 的 main 窗口配置重建 WebView。
/// dev/prod URL 由 from_config 内部处理，几何由 window-state 插件在创建时恢复。
fn rebuild_main_window(app: &tauri::AppHandle) -> Option<tauri::WebviewWindow> {
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == "main")?;
    match tauri::WebviewWindowBuilder::from_config(app, config) {
        Ok(builder) => match builder.build() {
            Ok(window) => Some(window),
            Err(error) => {
                eprintln!("[mcpane] 重建主窗口失败: {error}");
                None
            }
        },
        Err(error) => {
            eprintln!("[mcpane] 加载主窗口配置失败: {error}");
            None
        }
    }
}

/// macOS：退到托盘时同时藏 Dock 图标（Accessory 策略），恢复窗口时切回 Regular。
///
/// 只能走 setActivationPolicy 这一条路，不可叠加 set_dock_visibility：
/// 后者底层是遗留 Carbon API TransformProcessType，从 UIElement 转回前台时
/// Dock 会把进程当新前台进程重新注册、读不到 bundle 信息，图标显示为
/// 通用可执行文件（终端样式）；且与 setActivationPolicy 混用时序互相打架。
#[cfg(target_os = "macos")]
fn set_dock_visible(app: &tauri::AppHandle, visible: bool) {
    use tauri::ActivationPolicy;

    let policy = if visible {
        ActivationPolicy::Regular
    } else {
        ActivationPolicy::Accessory
    };
    if let Err(error) = app.set_activation_policy(policy) {
        eprintln!("[mcpane] 设置激活策略失败: {error}");
    }
    #[cfg(debug_assertions)]
    if visible {
        reapply_dock_icon(app);
    }
}

/// dev 裸二进制没有 .app bundle，Dock 图标全靠 Tauri 启动时（Ready 事件）
/// setApplicationIconImage 注入；Accessory→Regular 切回时 Dock 依 LaunchServices
/// 信息重建 tile，读不到 bundle 图标就显示通用 exec（终端样）图标，运行时注入的
/// 图标不会被沿用，必须每次恢复后重设。bundle 正式版图标由 Info.plist 提供，
/// Dock 重建即正确，无需此步，故仅编译进 debug 构建。
///
/// unsafe 豁免：objc2-app-kit 将 setApplicationIconImage 标为 unsafe，无 safe
/// 封装可用；与 tauri 2.11.5 dev 图标注入（src/app.rs Ready 分支）同款调用。
#[cfg(all(target_os = "macos", debug_assertions))]
#[allow(unsafe_code)]
fn reapply_dock_icon(app: &tauri::AppHandle) {
    // run_on_main_thread 在主线程同步执行、跨线程排队执行，两种情况下都排在
    // 前面的 set_activation_policy 之后，保证 Dock tile 先重建、图标后写入。
    let _ = app.run_on_main_thread(|| {
        use objc2::{AllocAnyThread, MainThreadMarker};
        use objc2_app_kit::{NSApplication, NSImage};
        use objc2_foundation::NSData;

        static ICON: &[u8] = include_bytes!("../icons/icon.png");
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let data = NSData::with_bytes(ICON);
        let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
            return;
        };
        unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
    });
}
