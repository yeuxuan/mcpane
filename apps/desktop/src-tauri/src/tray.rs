//! 系统托盘（TASK-602）：打开主窗口 / Provider 快捷切换 / 本地路由开关 / 退出。从 `main.rs`
//! 拆出，理由同 `window` 模块。

use crate::window;

/// 构建并注册托盘图标与菜单。
pub(crate) fn setup_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::tray::TrayIconBuilder;

    let menu = build_tray_menu(app)?;
    TrayIconBuilder::with_id("mcpane-tray")
        .icon(
            app.default_window_icon()
                .cloned()
                .ok_or(tauri::Error::WindowNotFound)?,
        )
        .menu(&menu)
        // 左键行为按平台惯例分岔：Windows 上左键是主操作（微软 UX 指南明写
        // "仅当其他选择都不适用时才用右键菜单响应左键"），macOS 菜单栏图标
        // 则历来是左键即弹菜单。
        .show_menu_on_left_click(cfg!(target_os = "macos"))
        .on_tray_icon_event(|_tray, _event| {
            #[cfg(target_os = "windows")]
            {
                use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = _event
                {
                    window::show_main_window(_tray.app_handle());
                }
            }
        })
        .on_menu_event(|app, event| {
            let id = event.id().as_ref().to_owned();
            match id.as_str() {
                "open" => window::show_main_window(app),
                "refresh" => {
                    if let Ok(menu) = build_tray_menu(app)
                        && let Some(tray) = app.tray_by_id("mcpane-tray")
                    {
                        let _ = tray.set_menu(Some(menu));
                    }
                }
                "quit" => app.exit(0),
                other => {
                    // 菜单项 id 格式：provider:<app>:<provider_id> / routing:<app>
                    if let Some(client_app) = other.strip_prefix("routing:") {
                        toggle_routing_from_tray(app.clone(), client_app.to_owned());
                    } else if let Some(rest) = other.strip_prefix("provider:")
                        && let Some((client_app, provider_id)) = rest.split_once(':')
                    {
                        let client_app = client_app.to_owned();
                        let provider_id = provider_id.to_owned();
                        let handle = app.clone();
                        tauri::async_runtime::spawn_blocking(move || {
                            switch_provider_from_tray(&handle, &client_app, &provider_id);
                        });
                    }
                }
            }
        })
        .build(app)?;
    Ok(())
}

/// 构建托盘菜单：按 `ClientApp::ALL` 为每个客户端建一个供应商切换子菜单，
/// 当前项打勾（additive 客户端上那是"最近一次启用的"，见 ADR-018）。
fn build_tray_menu(app: &tauri::AppHandle) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder};

    let open = MenuItemBuilder::with_id("open", "打开 MCPane").build(app)?;
    let refresh = MenuItemBuilder::with_id("refresh", "刷新菜单").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "退出").build(app)?;

    // 按 ClientApp::ALL 的顺序建子菜单：新增客户端只改 display_name 表，
    // 托盘这边不用动（顺序也与供应商页页签一致）。
    let grouped = crate::commands::tray_providers();
    let mut menu = MenuBuilder::new(app).item(&open);
    let submenus = mcpane_provider::ClientApp::ALL
        .into_iter()
        .map(|client| {
            let entries = grouped.get(&client).cloned().unwrap_or_default();
            let title = submenu_title(client, &entries);
            build_provider_submenu(app, &title, client, &entries)
        })
        .collect::<tauri::Result<Vec<_>>>()?;
    for submenu in &submenus {
        menu = menu.item(submenu);
    }
    menu.item(&refresh).separator().item(&quit).build()
}

/// 子菜单标题：显示名，当前 Provider 有缓存的成功配额时追加紧凑摘要
/// （如 "Kimi · 5h 45% · 周 12%"）。没有缓存/查询失败时不追加，不做占位。
fn submenu_title(
    app: mcpane_provider::ClientApp,
    entries: &[crate::commands::TrayProviderEntry],
) -> String {
    let display = mcpane_provider::adapter_for(app).display_name();
    let current_id = entries
        .iter()
        .find(|(_, _, is_current)| *is_current)
        .map(|(id, _, _)| id.as_str());
    let Some(quota) = current_id.and_then(crate::quota_cache::get) else {
        return display.to_owned();
    };
    match quota_summary(&quota) {
        Some(summary) => format!("{display} · {summary}"),
        None => display.to_owned(),
    }
}

/// 配额摘要：tiers（窗口式）与 balances（余额式）二选一，取决于查询到的是哪种。
fn quota_summary(quota: &mcpane_provider::ProviderQuota) -> Option<String> {
    if !quota.tiers.is_empty() {
        let parts: Vec<String> = quota
            .tiers
            .iter()
            .map(|tier| {
                let label = match tier.window {
                    mcpane_provider::QuotaWindow::FiveHour => "5h",
                    mcpane_provider::QuotaWindow::Weekly => "周",
                    mcpane_provider::QuotaWindow::Monthly => "月",
                };
                format!("{label} {:.0}%", tier.utilization)
            })
            .collect();
        return Some(parts.join(" · "));
    }
    if !quota.balances.is_empty() {
        let parts: Vec<String> = quota.balances.iter().map(format_balance).collect();
        return Some(parts.join(" · "));
    }
    None
}

/// 余额展示：CNY/USD 用符号省地方，其它币种原样带单位。
fn format_balance(balance: &mcpane_provider::Balance) -> String {
    match balance.currency.as_str() {
        "CNY" => format!("¥{:.2}", balance.amount),
        "USD" => format!("${:.2}", balance.amount),
        other => format!("{:.2} {other}", balance.amount),
    }
}

fn build_provider_submenu(
    app: &tauri::AppHandle,
    title: &str,
    client: mcpane_provider::ClientApp,
    providers: &[crate::commands::TrayProviderEntry],
) -> tauri::Result<tauri::menu::Submenu<tauri::Wry>> {
    use tauri::menu::{CheckMenuItemBuilder, SubmenuBuilder};

    let client_app = client.as_str();
    let mut submenu = SubmenuBuilder::new(app, title);
    for (id, name, is_current) in providers {
        let item = CheckMenuItemBuilder::with_id(format!("provider:{client_app}:{id}"), name)
            .checked(*is_current)
            .build(app)?;
        submenu = submenu.item(&item);
    }
    if client.supports_routing() {
        let enabled = crate::commands::tray_routing_enabled(client);
        let item = CheckMenuItemBuilder::with_id(format!("routing:{client_app}"), "本地路由")
            .checked(enabled)
            .build(app)?;
        submenu = submenu.separator().item(&item);
    }
    submenu.build()
}

/// 托盘开关本地路由：走与界面相同的命令（起不来时回退直连并恢复开关），失败经横幅提示。
fn toggle_routing_from_tray(app: tauri::AppHandle, client_app: String) {
    use tauri::Emitter;
    tauri::async_runtime::spawn(async move {
        let result = async {
            let mut config = crate::commands::provider_routing_get(client_app.clone()).await?;
            config.enabled = !config.enabled;
            crate::commands::provider_routing_set(app.clone(), client_app, config).await
        }
        .await;
        if let Err(error) = result {
            eprintln!("[mcpane] 托盘切换本地路由失败: {error}");
            let _ = app.emit(
                "tray-action-failed",
                serde_json::json!({ "message": format!("本地路由切换失败：{error}") }),
            );
        }
        // 失败时勾选态已被菜单自身翻转，重建以如实反映存储状态。
        refresh_tray_menu(&app);
    });
}

/// 重建托盘菜单（Provider 列表/勾选态变化后调用）。
/// 菜单操作在 macOS 必须走主线程，任意线程调用此函数都安全。
pub(crate) fn refresh_tray_menu(app: &tauri::AppHandle) {
    // Provider 系 mutation 全部经此刷新托盘，顺路作为自动同步的变更信号收敛点
    //（sync_download 期间有抑制计数，不会反向触发上传）。
    crate::auto_sync::notify_changed();
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Ok(menu) = build_tray_menu(&handle)
            && let Some(tray) = handle.tray_by_id("mcpane-tray")
        {
            let _ = tray.set_menu(Some(menu));
        }
    });
}

/// 托盘切换 Provider：成功后重建菜单同步勾选状态。
///
/// 托盘自身没有任何反馈通道（失败时用户只能靠「勾没变过来」察觉），故失败
/// 与警告都 emit 给窗口横幅展示；窗口关着时 emit 无接收方，eprintln 兜底。
fn switch_provider_from_tray(app: &tauri::AppHandle, client_app: &str, provider_id: &str) {
    use tauri::Emitter;
    let notify = |message: String| {
        eprintln!("[mcpane] {message}");
        let _ = app.emit(
            "tray-action-failed",
            serde_json::json!({ "message": message }),
        );
    };
    match crate::commands::switch_provider_sync(client_app, provider_id) {
        Ok(outcome) => {
            eprintln!("[mcpane] 托盘切换 Provider 成功: {client_app}:{provider_id}");
            for warning in &outcome.warnings {
                notify(format!("切换提示: {warning}"));
            }
            refresh_tray_menu(app);
        }
        Err(error) => notify(format!("托盘切换 Provider 失败: {error}")),
    }
}
