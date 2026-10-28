// Windows release 下隐藏控制台窗口。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod auto_sync;
mod commands;
mod config_watch;
mod desktop_proxy;
mod envsync;
mod hub_process;
mod mcp_clients;
mod quota_cache;
// 三个引用点（envsync::session、commands::autostart 的 macOS 分支与两处测试）
// 都是这个门控，mod 不跟上就会在非 macOS 的 bin 构建里变成 dead_code。
#[cfg(any(test, target_os = "macos"))]
mod plist_xml;
mod tray;
mod window;

// `crate::refresh_tray_menu(...)` 调用点分散在 commands/provider.rs、
// commands/sync.rs 等多处 Provider mutation 路径上，保留这条 crate 根路径，
// 免得把「托盘拆模块」的改动波及到一堆无关文件。
pub(crate) use tray::refresh_tray_menu;

/// window-state 插件只管位置/尺寸/最大化，不含 VISIBLE——轻量模式在窗口
/// 隐藏时落盘，带上可见性会让重建出的窗口直接不可见。
pub(crate) const WINDOW_STATE_FLAGS: tauri_plugin_window_state::StateFlags =
    tauri_plugin_window_state::StateFlags::POSITION
        .union(tauri_plugin_window_state::StateFlags::SIZE)
        .union(tauri_plugin_window_state::StateFlags::MAXIMIZED);

/// 接收 `mcpane://` 深链：运行中由回调送达，冷启动带着的链接用 `get_current` 补取。
/// 草稿暂存在后端，前端挂载后自取，所以冷启动时事件发早了也不会丢。
fn install_deep_link(app: &tauri::App) {
    use tauri_plugin_deep_link::DeepLinkExt;
    let deep_link = app.deep_link();
    // Linux AppImage 与 Windows 未安装的开发构建没有安装器替我们登记 scheme。
    #[cfg(any(windows, target_os = "linux"))]
    if (cfg!(target_os = "linux") || cfg!(debug_assertions))
        && let Err(error) = deep_link.register_all()
    {
        eprintln!("[mcpane] 注册 mcpane:// 失败: {error}");
    }
    let handle = app.handle().clone();
    deep_link.on_open_url(move |event| {
        commands::handle_import_urls(&handle, event.urls().iter().map(ToString::to_string));
    });
    if let Ok(Some(urls)) = deep_link.get_current() {
        commands::handle_import_urls(app.handle(), urls.iter().map(ToString::to_string));
    }
}

fn main() {
    // 先于一切：此后任何线程 panic 都会在 crash.log 留下现场。
    // 版本必须由此处传入——在共享 crate 内取会拿到它自己的版本号。
    mcpane_hub_protocol::panic_hook::install("desktop", env!("CARGO_PKG_VERSION"));
    tauri::Builder::default()
        // 必须最先注册：第二个实例要在任何插件初始化之前就被挡回。
        //
        // 挡的不是 Hub 端口——hub_process 的探活会 adopt 已在跑的实例，不会抢占。
        // 挡的是 providers.json 的 load-modify-save：PROVIDER_MUTEX 只是进程内锁，
        // 第二个进程绕过它并发切换会后写覆盖前写，最坏丢掉 codexAuthBackup 引用
        // （用户的 Codex 官方登录态）。envsync 的 ENVSYNC_LOCK 同样按"App 单实例"
        // 立论，此前该前提并无任何东西保证。
        //
        // 二次启动的语义 = 唤起已有窗口，等价于点托盘"打开 MCPane"；
        // show_main_window 已处理轻量模式下 WebView 被销毁需重建的情况。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            window::show_main_window(app);
        }))
        // 自定义 scheme（mcpane://）：macOS 由系统事件送达，Windows/Linux 由二次启动的
        // argv 经上面的单实例插件（deep-link feature）转发进同一个回调。
        .plugin(tauri_plugin_deep_link::init())
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(WINDOW_STATE_FLAGS)
                .build(),
        )
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        // 开机自启：注册进系统的命令行带 --autostart，setup 据此静默驻留托盘。
        // macOS 必须走 LaunchAgent——AppleScript 登录项传不了自定义参数，
        // 开机拉起就会跟手动打开一样弹出主窗口（详见 commands/autostart.rs）。
        .plugin({
            let builder = tauri_plugin_autostart::Builder::new().arg(commands::AUTOSTART_FLAG);
            #[cfg(target_os = "macos")]
            let builder =
                builder.macos_launcher(tauri_plugin_autostart::MacosLauncher::LaunchAgent);
            builder.build()
        })
        // 点关闭 = 退到托盘：hub 是常驻代理，窗口只是控制面板。
        // 真正退出走托盘"退出"（app.exit(0)）。
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                use tauri::Manager;
                api.prevent_close();
                window::hide_to_tray(window.app_handle());
            }
        })
        .setup(|app| {
            // 主窗口在 tauri.conf.json 里是 visible:false：开机自启的正确形态
            // 是"悄悄驻留托盘"，弹出窗口即失败，而 setup 里补 hide() 会先闪一帧。
            // 普通启动的那次 show 放在 RunEvent::Ready（见 run 的分支）——setup
            // 早于 NSApplication finishLaunching，此处 show() 会被 AppKit 丢掉。
            //
            // 藏 Dock 则要趁早：等到 Ready 再切 Accessory，图标已经蹦出来了。
            if commands::launched_by_autostart() {
                window::stay_in_tray_on_launch(app.handle());
            }
            // 开机自启默认开启：与后面的同步收编并行。未打包的构建会跳过
            //（macOS 看是不是 .app；其余平台 debug 一律跳过）；macOS 还会撤回
            // 已经指向本进程的登录项（系统设置否则只显示无图标的 exec）。
            // 失败不阻断启动，下次再试。
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    commands::ensure_default_autostart(handle).await;
                });
            }
            install_deep_link(app);
            // 首启存量收编：早于托盘构建与任何 IPC，把既有 live 配置收编为可管理
            // Provider 并置 current，令托盘与前端首帧的"当前生效项"如实反映 live。
            commands::import_live_configs_on_startup();
            // 收编之后再播种官方项：只给装了的客户端建，且每个 app 只播一次
            //（删掉的官方项不会复活）。顺序不能颠倒，见函数文档。
            commands::seed_official_providers_on_startup();
            // Desktop 本地网关：当前生效项为 proxy 模式时自拉起（重启 MCPane 后
            // Desktop 才能继续对话）；否则确保停机。启动路径没有 UI 可承接，
            // 失败记日志（用户操作触发的同步路径都已上抛）。
            // 开启了本地路由的 app：上次退出已还原直连，这里重新接管。
            desktop_proxy::reapply_routing_on_startup();
            if let Err(error) = desktop_proxy::sync_from_store_blocking() {
                eprintln!("[mcpane] 本地路由同步失败: {error}");
            }
            tray::setup_tray(app.handle())?;
            hub_process::spawn(app.handle());
            // 客户端配置被外部改动时提醒：既有的 hash 冲突检测要等到下次写入才
            // 报错，这里把发现提前到改动发生的那一刻。auth.json 不走引擎、无
            // history 可比对，故不监听（详见 config_watch 模块文档）。
            {
                let paths = commands::client_paths();
                config_watch::install(app.handle(), &[paths.claude_settings, paths.codex_config]);
            }
            // 把库里的代理配置加载进本进程的出网客户端（ADR-017）。不做这一步，
            // 用户配的代理只在「配完那一次」生效，重启即回到跟随系统。
            tauri::async_runtime::spawn(async {
                match commands::proxy_get().await {
                    Ok(config) => match mcpane_net::set_proxy(&config) {
                        // 「为什么连不上」是这个功能最高频的问题，回答它的第一步
                        // 永远是「当时在走哪条通道」。describe 已隐去认证信息。
                        Ok(()) => println!("[mcpane] 出网代理: {}", mcpane_net::describe(&config)),
                        // 配置坏了不该让应用起不来：保持 reqwest 默认（跟随系统）继续跑。
                        Err(error) => {
                            eprintln!("[mcpane] 应用代理配置失败，暂按跟随系统出网: {error}");
                        }
                    },
                    Err(error) => eprintln!("[mcpane] 读取代理配置失败: {error}"),
                }
            });
            // 启动静默检查更新（用户可在设置里关）。延迟几秒避开启动高峰；
            // 结果经事件通知前端展示，检查失败静默——离线启动不该弹错。
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    if !commands::auto_update_check_get().await.unwrap_or(true) {
                        return;
                    }
                    if let Ok(status) = commands::updater::check(&handle).await
                        && status.available
                    {
                        use tauri::Emitter;
                        let _ = handle.emit("mcpane://update-available", &status);
                    }
                });
            }
            // 启动时按保留期清理两张时间表——prune 命令只有手动入口，不在启动
            // 清一次的话它们会无限增长。天数取 commands 里的常量：设置页显示的
            // 保留窗口读的是同一个值，两处各写一个数就会承诺与行为对不上。
            tauri::async_runtime::spawn(async {
                if let Err(error) = commands::logs_prune(commands::LOG_RETAIN_DAYS).await {
                    eprintln!("[mcpane] 启动清理请求日志失败: {error}");
                }
                if let Err(error) = commands::usage_prune(commands::USAGE_RETAIN_DAYS).await {
                    eprintln!("[mcpane] 启动清理用量事件失败: {error}");
                }
            });
            // macOS：把 env.sh 里的受管变量重放进 GUI 会话（兜底 LaunchAgent
            // 被删/禁用的情况，见 ADR-016）；其余平台 no-op。
            // 自动云同步 worker（enabled+autoSync 双开关在执行时判定，常驻无害）。
            auto_sync::spawn_worker();
            // Provider 配额托盘缓存：每 5 分钟只刷新已缓存且仍当前生效的项
            //（见 commands::refresh_cached_provider_quotas 文档）。
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(5 * 60)).await;
                        commands::refresh_cached_provider_quotas(&handle).await;
                    }
                });
            }
            // models.dev 定价同步（默认启用，24h 间隔，失败只记状态不打扰）。
            commands::spawn_startup_sync();
            // 定时本地备份：启动 60s 后首查、每 6h 复查；最新备份超 24h 即补一份，
            // 保留最近 10 份。失败只打日志——备份是兜底能力，不打扰用户。
            tauri::async_runtime::spawn(async {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                loop {
                    let home = mcpane_hub_protocol::mcpane_home();
                    if mcpane_storage::backup_due(&home, 24) {
                        match commands::db_pool().await {
                            Ok(pool) => {
                                if let Err(error) = mcpane_storage::backup_now(&pool, &home).await {
                                    eprintln!("[mcpane] 定时备份失败: {error}");
                                }
                                let _ = mcpane_storage::prune_backups(&home, 10);
                            }
                            Err(error) => eprintln!("[mcpane] 定时备份取库失败: {error}"),
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
                }
            });
            tauri::async_runtime::spawn_blocking(|| {
                for warning in envsync::resync_session() {
                    eprintln!("[mcpane] envsync 启动重放: {warning}");
                }
            });
            // dev 模式 Ctrl+C（SIGINT）/ 系统 SIGTERM 不会触发 RunEvent::Exit，
            // 显式转成 app.exit(0) 走统一的优雅关停（回收托管的 hub）。
            #[cfg(unix)]
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    use tokio::signal::unix::{SignalKind, signal};
                    let (Ok(mut interrupt), Ok(mut terminate)) = (
                        signal(SignalKind::interrupt()),
                        signal(SignalKind::terminate()),
                    ) else {
                        return;
                    };
                    tokio::select! {
                        _ = interrupt.recv() => {}
                        _ = terminate.recv() => {}
                    }
                    handle.exit(0);
                });
            }
            // Windows 没有 SIGTERM 等价物，但 `tauri dev` 挂着控制台时 Ctrl+C
            // 一样不会走 RunEvent::Exit；`ctrl_c()` 是 tokio 里少数天然跨平台的
            // 信号 API，同一套代码在 Windows 控制台上也能收到中断事件。
            #[cfg(windows)]
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    if tokio::signal::ctrl_c().await.is_ok() {
                        handle.exit(0);
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::detect_clients,
            commands::hub_status,
            commands::hub_diagnostics,
            commands::connection_create,
            commands::connection_rotate_sync,
            commands::connection_set_enabled,
            commands::connection_delete,
            commands::connection_set_tool_exposure,
            commands::providers_list,
            commands::provider_current,
            commands::provider_add,
            commands::provider_update,
            commands::provider_delete,
            commands::provider_switch,
            commands::provider_set_api_key,
            commands::provider_test_connectivity,
            commands::provider_official_update,
            commands::provider_official_ensure,
            commands::provider_clients_installed,
            commands::provider_remove_from_live,
            commands::provider_live_ids,
            commands::client_env_status,
            commands::provider_config_dirs_get,
            commands::provider_config_dir_set,
            commands::provider_common_snippet,
            commands::provider_common_snippet_set,
            commands::provider_test_endpoints,
            commands::provider_fetch_models,
            commands::provider_quota,
            commands::provider_live_settings,
            commands::claude_desktop_status,
            commands::claude_desktop_import_from_claude,
            commands::desktop_proxy_status,
            commands::desktop_proxy_start,
            commands::desktop_proxy_stop,
            commands::desktop_proxy_set_port,
            commands::provider_routing_get,
            commands::provider_routing_set,
            commands::router_status,
            commands::router_reset_breaker,
            commands::router_recent_requests,
            commands::router_clear_requests,
            commands::provider_unified_list,
            commands::provider_unified_upsert,
            commands::provider_unified_delete,
            commands::provider_unified_set_api_key,
            commands::claude_connect_preview,
            commands::claude_connect_apply,
            commands::claude_disconnect,
            commands::codex_connect_preview,
            commands::codex_connect_apply,
            commands::codex_disconnect,
            commands::mcp_clients_status,
            commands::custom_mcp_clients_list,
            commands::custom_mcp_client_upsert,
            commands::custom_mcp_client_remove,
            commands::mcp_client_connect_preview,
            commands::mcp_client_connect_apply,
            commands::mcp_client_disconnect,
            commands::client_config_snippet,
            commands::logs_recent,
            commands::logs_get,
            commands::servers_list,
            commands::server_upsert,
            commands::server_test,
            commands::server_set_enabled,
            commands::server_delete,
            commands::import_from_clients,
            commands::ccswitch_detect,
            commands::ccswitch_import,
            commands::server_discover,
            commands::server_oauth_login,
            commands::server_oauth_cancel,
            commands::server_oauth_logout,
            commands::import_link_peek,
            commands::import_link_confirm,
            commands::import_link_discard,
            commands::servers_import_json,
            commands::servers_import_preview,
            commands::servers_export,
            commands::servers_export_save,
            commands::catalog_list,
            commands::workspace_tool_toggle,
            commands::workspace_tool_overrides,
            commands::workspace_item_toggle,
            commands::workspace_item_overrides,
            commands::tool_annotations_set,
            commands::workspace_tool_discovery,
            commands::workspace_tool_discovery_set,
            commands::workspace_usage_guidance,
            commands::workspace_usage_guidance_set,
            commands::guidance_targets,
            commands::guidance_deliver,
            commands::guidance_revoke,
            commands::guidance_preview,
            commands::workspaces_list,
            commands::workspace_create,
            commands::workspace_server_set,
            commands::workspace_servers,
            commands::connection_set_workspace,
            commands::connection_denied_servers,
            commands::connection_server_access_set,
            commands::config_history_list,
            commands::config_external_changes,
            commands::config_rollback,
            commands::app_info,
            commands::data_overview,
            commands::open_external,
            commands::logs_prune,
            commands::logs_stats,
            commands::hub_process_state,
            commands::hub_enabled_get,
            commands::hub_set_enabled,
            commands::update_check,
            commands::update_install,
            commands::update_restart,
            commands::auto_update_check_get,
            commands::auto_update_check_set,
            commands::autostart_get,
            commands::autostart_set,
            commands::proxy_get,
            commands::proxy_set,
            commands::proxy_scan,
            commands::proxy_test,
            commands::backup_create,
            commands::backup_list,
            commands::backup_restore,
            commands::backup_delete,
            commands::sync_settings_get,
            commands::sync_settings_set,
            commands::sync_test,
            commands::sync_remote_info,
            commands::sync_upload,
            commands::sync_download,
            commands::provider_hermes_external,
            commands::init_status,
            commands::window_ready,
            commands::init_replace_db_from_backup,
            commands::log_frontend_error,
            commands::pricing_sync_status,
            commands::pricing_sync_set_enabled,
            commands::pricing_sync_run,
            commands::usage_sync,
            commands::usage_summary,
            commands::usage_by_model,
            commands::usage_price_info,
            commands::usage_price_override_set,
            commands::usage_price_override_clear,
            commands::usage_by_project,
            commands::usage_by_provider,
            commands::usage_trend,
            commands::usage_prune,
        ])
        .build(tauri::generate_context!())
        .unwrap_or_else(|error| panic!("MCPane desktop 启动失败: {error}"))
        .run(|app, event| match event {
            // 主窗口配置为 visible:false，普通启动在这里放行显示（真正 show 在
            // 前端首帧就绪后，见 window::on_app_ready）。
            //
            // 时机不能再往前挪到 setup：那时 NSApplication 还没 finishLaunching，
            // show() 下的 makeKeyAndOrderFront 会被 AppKit 丢弃，进程跑起来了却
            // 一个窗口也没有（实测 System Events 报窗口数 0）。
            //
            // 开机自启拉起的那次不显示——它要的就是没有窗口，setup 里已把
            // Dock 图标一并藏掉。
            tauri::RunEvent::Ready => window::on_app_ready(app),
            // code=None 的退出请求（macOS Cmd+Q / Quit AppleEvent，或隐藏窗口的
            // WebView 被系统回收后"无存活窗口"）一律拦下并退到托盘，保持后台驻留；
            // 用户主动退出（托盘"退出"/ SIGINT 转发）走 app.exit(0)，code=Some，不受影响。
            tauri::RunEvent::ExitRequested {
                code: None, api, ..
            } => {
                api.prevent_exit();
                window::hide_to_tray(app);
            }
            // 用户主动退出（托盘"退出"/ SIGINT 转发 → app.exit）：主线程同步等
            // hub 关停会卡死事件循环（托盘点"退出"出现 beachball 转圈），改为拦下
            // 本次退出，先藏窗口/托盘给出"已退出"反馈，异步清理完直接结束进程。
            tauri::RunEvent::ExitRequested {
                code: Some(code),
                api,
                ..
            } => {
                use tauri_plugin_window_state::AppHandleExt;

                api.prevent_exit();
                let _ = app.save_window_state(WINDOW_STATE_FLAGS);
                window::hide_to_tray(app);
                app.remove_tray_by_id("mcpane-tray");
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ =
                        tauri::async_runtime::spawn_blocking(desktop_proxy::restore_direct_on_exit)
                            .await;
                    hub_process::shutdown(&app).await;
                    // 绕过事件循环直接退出，避免再次触发 ExitRequested。
                    std::process::exit(code);
                });
            }
            // 兜底：绕过上面拦截的退出路径仍要关停托管的 hub。
            tauri::RunEvent::Exit => {
                desktop_proxy::restore_direct_on_exit();
                hub_process::shutdown_blocking(app);
            }
            // macOS：Dock 图标被点击重新激活时恢复主窗口（Dock 隐藏失败时的兜底）。
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => window::show_main_window(app),
            _ => {}
        });
}
