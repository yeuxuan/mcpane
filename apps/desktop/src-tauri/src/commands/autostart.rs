//! 开机自启（tauri-plugin-autostart）。
//!
//! 事实源是**操作系统本身**，不入库：macOS 看
//! `~/Library/LaunchAgents/MCPane.plist` 在不在，Windows 看
//! `HKCU\...\Run` 有没有键，Linux 看 `~/.config/autostart/*.desktop`。
//! 库里再存一份 bool 只会撒谎——用户在系统设置（macOS「登录项」/ Windows
//! 任务管理器「启动」）里关掉之后，MCPane 仍会显示"已开启"。
//!
//! 默认开启走一次性播种：库里只记「有没有推过」，推成功才落盘，失败下次
//! 启动再试。用户关掉之后不会被复活。
//!
//! macOS 登录项要显示应用图标，必须在 LaunchAgent plist 里写
//! `AssociatedBundleIdentifiers`（系统按 bundle id 去找 MCPane.app）。
//! 插件不会写这个键；再把 `tauri dev` 的裸二进制注册进去，系统设置就只
//! 看得到一个叫 `mcpane` 的通用 exec 图标。所以未打包的构建既不播种、
//! 也不接受手动开启，并撤回已经指向本进程的登录项。
//!
//! 注册进去的命令行带 [`AUTOSTART_FLAG`]，供 `main.rs` 判断这次是不是开机
//! 拉起的：是就静默驻留托盘，不弹窗、不占 Dock（见 `stay_in_tray_on_launch`）。

#[cfg(any(test, target_os = "macos"))]
use crate::plist_xml::xml_escape;

/// 自启动项注册的命令行参数。
///
/// macOS 必须用 LaunchAgent 而非 AppleScript 登录项：后者只认 `--hidden`
/// / `--minimized` 两个参数（见 auto-launch 的 macos 实现），自定义 flag
/// 传不进去，"开机拉起"这一事实在进程里就无从判断了。
pub const AUTOSTART_FLAG: &str = "--autostart";

/// 开发构建拒绝写入登录项。前端按此码映射 i18n，不要改字符串。
pub const UNPACKAGED_BUILD_ERROR: &str = "unpackaged-build";

/// 本次进程是否由开机自启拉起。
pub fn launched_by_autostart() -> bool {
    argv_has_autostart_flag(std::env::args())
}

/// argv 判定本体（`launched_by_autostart` 读的是进程全局状态，测不了）。
fn argv_has_autostart_flag<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter().any(|arg| arg.as_ref() == AUTOSTART_FLAG)
}

#[tauri::command]
pub fn autostart_get(app: tauri::AppHandle) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;

    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

/// 切换开机自启，返回**写完之后实测**的状态。
///
/// 不回显入参：写系统自启动项可能被安全策略/权限挡下却不报错（企业管控的
/// Windows 注册表、只读的 LaunchAgents 目录），回显入参会让界面显示成功、
/// 下次开机却没起来。回读一次，让开关如实反映系统。
#[tauri::command]
pub fn autostart_set(app: tauri::AppHandle, enabled: bool) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;

    if enabled && should_skip_registration() {
        return Err(UNPACKAGED_BUILD_ERROR.into());
    }

    let manager = app.autolaunch();
    if enabled {
        manager.enable().map_err(|e| e.to_string())?;
        #[cfg(target_os = "macos")]
        if let Err(error) = annotate_launch_agent(&app) {
            eprintln!("[mcpane] 登录项图标标注失败（自启已写入）: {error}");
        }
    } else {
        manager.disable().map_err(|e| e.to_string())?;
    }
    manager.is_enabled().map_err(|e| e.to_string())
}

/// 装完 / 升级后首次启动：把开机自启的默认「开」推给操作系统一次。
///
/// 未打包的构建不播种：macOS 看是不是 `.app` 包，其余平台 debug 一律跳过。
/// macOS 上若登录项已经指向本进程（`tauri dev` 裸二进制），撤回以免系统
/// 设置显示无图标的可执行文件。
pub(crate) async fn ensure_default_autostart(app: tauri::AppHandle) {
    if let Err(error) = seed_autostart_default(app).await {
        eprintln!("[mcpane] 开机自启默认开启未生效，下次启动再试: {error}");
    }
}

async fn seed_autostart_default(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;

    if should_skip_registration() {
        #[cfg(target_os = "macos")]
        retract_login_item_if_points_at_current_exe(&app);
        return Ok(());
    }

    let repo = super::settings_repo().await?;
    let seeded = repo
        .get_autostart_seeded()
        .await
        .map_err(|e| e.to_string())?;
    if should_seed_autostart_default(seeded) {
        let manager = app.autolaunch();
        manager.enable().map_err(|e| e.to_string())?;
        let actually_enabled = manager.is_enabled().map_err(|e| e.to_string())?;
        if !autostart_seed_succeeded(actually_enabled) {
            return Err("系统未接受开机自启写入".into());
        }
        repo.set_autostart_seeded(true)
            .await
            .map_err(|e| e.to_string())?;
        eprintln!("[mcpane] 已默认开启开机自启");
    }

    #[cfg(target_os = "macos")]
    {
        // 就地改 Program 路径，不走 enable()：enable 会整份重写 plist，把系统
        // 设置关掉时留下的 Disabled 键抹掉，等于把用户关掉的登录项又打开。
        if let Err(error) = repair_unpackaged_program_path(&app) {
            eprintln!("[mcpane] 登录项路径修复失败: {error}");
        }
        if app.autolaunch().is_enabled().ok() == Some(true)
            && let Err(error) = annotate_launch_agent(&app)
        {
            eprintln!("[mcpane] 登录项图标标注失败: {error}");
        }
    }
    Ok(())
}

/// 尚未向系统推过默认开启时才 enable；推过了不再碰，避免用户关掉后又被打开。
fn should_seed_autostart_default(seeded: bool) -> bool {
    !seeded
}

/// 只有系统实测已开启才把播种标记落盘；被策略挡下则下次启动再试。
fn autostart_seed_succeeded(actually_enabled: bool) -> bool {
    actually_enabled
}

/// `tauri dev` / `cargo run` 不得写登录项：裸二进制在系统设置里没有应用图标。
/// macOS 看是不是 `.app` 包；其余平台 debug 构建一律跳过。
fn should_skip_registration() -> bool {
    #[cfg(target_os = "macos")]
    {
        path_looks_unpackaged(&current_exe_display())
    }
    #[cfg(not(target_os = "macos"))]
    {
        cfg!(debug_assertions)
    }
}

/// 不在 `.app` 包里的二进制（`tauri dev`、`cargo run`）注册进登录项后，
/// 系统设置只能显示通用 exec 图标。
#[cfg(any(test, target_os = "macos"))]
fn path_looks_unpackaged(path: &str) -> bool {
    !path.contains(".app/Contents/MacOS/")
}

#[cfg(target_os = "macos")]
fn current_exe_display() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok())
        .map(|path| path.display().to_string())
        .unwrap_or_default()
}

/// 把 bundle id 写进插件刚生成的 plist，系统设置才会显示 MCPane 图标和名字。
#[cfg(any(test, target_os = "macos"))]
fn inject_associated_bundle_id(plist: &str, bundle_id: &str) -> Result<String, String> {
    if plist.contains("<key>AssociatedBundleIdentifiers</key>") {
        return Ok(plist.to_owned());
    }
    let needle = "<key>Label</key>";
    let at = plist
        .find(needle)
        .ok_or_else(|| "LaunchAgent plist 缺少 Label".to_owned())?;
    let insert = format!(
        "<key>AssociatedBundleIdentifiers</key>\n  <array>\n    <string>{}</string>\n  </array>\n  ",
        xml_escape(bundle_id)
    );
    let mut out = String::with_capacity(plist.len() + insert.len());
    out.push_str(&plist[..at]);
    out.push_str(&insert);
    out.push_str(&plist[at..]);
    Ok(out)
}

/// LaunchAgent 的 Program 路径（ProgramArguments 第一项）。
#[cfg(any(test, target_os = "macos"))]
fn launch_agent_program(plist: &str) -> Option<&str> {
    let rest = plist.split_once("<key>ProgramArguments</key>")?.1;
    let rest = rest.split_once("<string>")?.1;
    let (program, _) = rest.split_once("</string>")?;
    Some(program)
}

/// 只替换 ProgramArguments 第一项，其它键（含 `Disabled`）原样保留。
#[cfg(any(test, target_os = "macos"))]
fn rewrite_launch_agent_program(plist: &str, new_program: &str) -> Option<String> {
    let args_at = plist.find("<key>ProgramArguments</key>")?;
    let open = "<string>";
    let close = "</string>";
    let open_at = args_at + plist[args_at..].find(open)?;
    let inner_at = open_at + open.len();
    let close_at = inner_at + plist[inner_at..].find(close)?;
    let mut out = String::with_capacity(plist.len() + new_program.len());
    out.push_str(&plist[..inner_at]);
    out.push_str(&xml_escape(new_program));
    out.push_str(&plist[close_at..]);
    Some(out)
}

#[cfg(target_os = "macos")]
fn launch_agent_plist_path(app_name: &str) -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        std::path::PathBuf::from(home)
            .join("Library/LaunchAgents")
            .join(format!("{app_name}.plist")),
    )
}

#[cfg(target_os = "macos")]
fn annotate_launch_agent(app: &tauri::AppHandle) -> Result<(), String> {
    debug_assert_eq!(
        app.config().identifier.as_str(),
        crate::plist_xml::BUNDLE_IDENTIFIER,
        "tauri.conf.json identifier 与 plist_xml::BUNDLE_IDENTIFIER 不一致"
    );
    let Some(path) = launch_agent_plist_path(&app.package_info().name) else {
        return Err("找不到用户主目录".into());
    };
    let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let next = inject_associated_bundle_id(&raw, &app.config().identifier)?;
    if next != raw {
        std::fs::write(&path, next).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 登录项还指向开发二进制时，只改 Program 路径，不 enable。
#[cfg(target_os = "macos")]
fn repair_unpackaged_program_path(app: &tauri::AppHandle) -> Result<(), String> {
    let exe = current_exe_display();
    if exe.is_empty() || path_looks_unpackaged(&exe) {
        return Ok(());
    }
    let Some(path) = launch_agent_plist_path(&app.package_info().name) else {
        return Ok(());
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let Some(program) = launch_agent_program(&raw) else {
        return Ok(());
    };
    if !path_looks_unpackaged(program) {
        return Ok(());
    }
    let Some(next) = rewrite_launch_agent_program(&raw, &exe) else {
        return Err("LaunchAgent plist 缺少 ProgramArguments".into());
    };
    if next != raw {
        std::fs::write(&path, next).map_err(|e| e.to_string())?;
        eprintln!("[mcpane] 已把登录项从开发二进制改到安装包");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn retract_login_item_if_points_at_current_exe(app: &tauri::AppHandle) {
    use tauri_plugin_autostart::ManagerExt;

    let exe = current_exe_display();
    if exe.is_empty() {
        return;
    }
    let Some(path) = launch_agent_plist_path(&app.package_info().name) else {
        return;
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return;
    };
    if launch_agent_program(&raw) != Some(exe.as_str()) {
        return;
    }
    if let Err(error) = app.autolaunch().disable() {
        eprintln!("[mcpane] 撤回开发二进制登录项失败: {error}");
    } else {
        eprintln!("[mcpane] 已撤回指向开发二进制的登录项，避免系统设置显示无图标的可执行文件");
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::{
        AUTOSTART_FLAG, UNPACKAGED_BUILD_ERROR, argv_has_autostart_flag, autostart_seed_succeeded,
        inject_associated_bundle_id, launch_agent_program, path_looks_unpackaged,
        rewrite_launch_agent_program, should_seed_autostart_default,
    };
    use crate::plist_xml::BUNDLE_IDENTIFIER;

    const SAMPLE_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
  <dict>
  <key>Label</key>
  <string>MCPane</string>
  <key>ProgramArguments</key>
  <array><string>/Applications/MCPane.app/Contents/MacOS/mcpane</string><string>--autostart</string></array>
  <key>RunAtLoad</key>
  <true/>
  </dict>
</plist>"#;

    const DISABLED_UNPACKAGED_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
  <dict>
  <key>Disabled</key>
  <true/>
  <key>Label</key>
  <string>MCPane</string>
  <key>ProgramArguments</key>
  <array><string>/tmp/debug/mcpane</string><string>--autostart</string></array>
  <key>RunAtLoad</key>
  <true/>
  </dict>
</plist>"#;

    #[test]
    fn detects_the_flag_anywhere_in_argv() {
        // argv[0] 是程序自身路径，flag 永远在其后。
        assert!(argv_has_autostart_flag([
            "/Applications/MCPane.app/Contents/MacOS/MCPane",
            AUTOSTART_FLAG,
        ]));
    }

    #[test]
    fn plain_launch_is_not_autostart() {
        assert!(!argv_has_autostart_flag([
            "/Applications/MCPane.app/Contents/MacOS/MCPane"
        ]));
    }

    /// 前缀匹配会把 `--autostart-delay` 之类的未来参数误判成开机拉起，
    /// 于是普通启动也不弹窗——必须整词相等。
    #[test]
    fn requires_exact_match_not_prefix() {
        assert!(!argv_has_autostart_flag(["mcpane", "--autostart-delay=30"]));
    }

    #[test]
    fn seeds_only_when_not_yet_applied() {
        assert!(should_seed_autostart_default(false));
        assert!(!should_seed_autostart_default(true));
    }

    /// 系统拒绝写入时不能把 seeded 写成 true，否则永远不会再试，开关会一直关着。
    #[test]
    fn does_not_record_seed_when_os_still_disabled() {
        assert!(!autostart_seed_succeeded(false));
        assert!(autostart_seed_succeeded(true));
    }

    #[test]
    fn unpackaged_dev_binary_is_rejected() {
        assert!(path_looks_unpackaged(
            "/Users/me/MCPanePro/apps/desktop/src-tauri/target/debug/mcpane"
        ));
        assert!(!path_looks_unpackaged(
            "/Applications/MCPane.app/Contents/MacOS/mcpane"
        ));
    }

    #[test]
    fn unpackaged_error_code_is_stable_for_i18n() {
        assert_eq!(UNPACKAGED_BUILD_ERROR, "unpackaged-build");
    }

    #[test]
    fn reads_launch_agent_program_from_plist() {
        assert_eq!(
            launch_agent_program(SAMPLE_PLIST),
            Some("/Applications/MCPane.app/Contents/MacOS/mcpane")
        );
        assert!(path_looks_unpackaged(
            launch_agent_program(
                r#"<key>ProgramArguments</key><array><string>/tmp/debug/mcpane</string></array>"#
            )
            .unwrap()
        ));
    }

    /// 系统设置关掉登录项时常留下 Disabled=true 而不删文件。就地改路径必须
    /// 保留这个键，否则等于把用户关掉的项又打开。
    #[test]
    fn rewrite_keeps_disabled_and_only_replaces_program() {
        let out = rewrite_launch_agent_program(
            DISABLED_UNPACKAGED_PLIST,
            "/Applications/MCPane.app/Contents/MacOS/mcpane",
        )
        .expect("sample has ProgramArguments");
        assert!(out.contains("<key>Disabled</key>"));
        assert!(out.contains("<true/>"));
        assert!(out.contains("/Applications/MCPane.app/Contents/MacOS/mcpane"));
        assert!(!out.contains("/tmp/debug/mcpane"));
        assert!(out.contains("--autostart"));
        assert_eq!(
            launch_agent_program(&out),
            Some("/Applications/MCPane.app/Contents/MacOS/mcpane")
        );
    }

    #[test]
    fn rewrite_escapes_xml_in_the_new_program_path() {
        let out = rewrite_launch_agent_program(SAMPLE_PLIST, "/tmp/a&b<c>")
            .expect("sample has ProgramArguments");
        assert!(out.contains("<string>/tmp/a&amp;b&lt;c&gt;</string>"));
        assert!(!out.contains("<string>/tmp/a&b<c></string>"));
    }

    #[test]
    fn injects_bundle_id_so_login_items_can_show_the_app_icon() {
        let out = inject_associated_bundle_id(SAMPLE_PLIST, BUNDLE_IDENTIFIER)
            .expect("sample plist has Label");
        assert!(out.contains("<key>AssociatedBundleIdentifiers</key>"));
        assert!(out.contains("<string>io.mcpane.desktop</string>"));
        assert!(out.contains("/Applications/MCPane.app/Contents/MacOS/mcpane"));
        assert!(out.contains("--autostart"));
    }

    #[test]
    fn inject_is_idempotent() {
        let once = inject_associated_bundle_id(SAMPLE_PLIST, BUNDLE_IDENTIFIER)
            .expect("sample plist has Label");
        let twice = inject_associated_bundle_id(&once, BUNDLE_IDENTIFIER)
            .expect("injected plist still has Label");
        assert_eq!(once, twice);
    }
}
