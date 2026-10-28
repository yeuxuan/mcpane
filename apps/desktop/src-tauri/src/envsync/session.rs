//! 会话级/系统级副作用的抽象与平台薄壳（本文件不做单测——每个实现
//! ≤ 数行系统调用，编排与降级逻辑在 mod.rs 用记录型 stub 覆盖）。
//!
//! - macOS：`launchctl setenv/unsetenv`（对之后启动的 GUI 进程生效）+
//!   `~/Library/LaunchAgents/com.mcpane.env.plist`（RunAtLoad 重放，保证
//!   重启后 Dock 启动的客户端仍能读到变量；当前会话已被 setenv 覆盖，
//!   plist 只写文件不主动 bootstrap）。
//! - Linux：无会话级注入（environment.d 派生视图在 store 层）。
//! - Windows：HKCU\Environment 直写 + WM_SETTINGCHANGE 广播（不用 setx：
//!   弹窗闪烁、无删除能力、1024 截断）。

use std::path::Path;

#[cfg(any(target_os = "macos", test))]
use crate::plist_xml::{BUNDLE_IDENTIFIER, xml_escape};

pub(super) enum AgentState {
    /// 下面两个只有 macOS 构造：Windows 走注册表、Linux 无会话级注入，
    /// 两者的 `install_agent` 恒返回 `Unchanged`。
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Installed,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Updated,
    Unchanged,
}

pub(super) trait SessionSync {
    fn set_var(&self, var: &str, value: &str) -> Result<(), String>;
    fn unset_var(&self, var: &str) -> Result<(), String>;
    fn install_agent(&self, env_file: &Path, agent_path: &Path) -> Result<AgentState, String>;
    fn remove_agent(&self, agent_path: &Path) -> Result<(), String>;
}

/// LaunchAgent plist（纯字符串渲染，跨平台可测）：登录时读 env.sh，
/// 把全部受管变量重放进 GUI 会话。路径在 sh 双引号内，需转义。
///
/// `AssociatedBundleIdentifiers` 让系统设置按 MCPane.app 显示图标，
/// 而不是 Program 的 `/bin/sh` 通用图标。
#[cfg(any(target_os = "macos", test))]
pub(super) fn render_agent_plist(env_file: &Path) -> String {
    let sh_quoted = format!(
        "\"{}\"",
        env_file
            .display()
            .to_string()
            .replace('\\', r"\\")
            .replace('"', "\\\"")
            .replace('$', "\\$")
            .replace('`', "\\`")
    );
    let script = format!(
        "set -a; . {sh_quoted} 2>/dev/null || exit 0; \
         env | while IFS='=' read -r k v; do case \"$k\" in MCPANE_*) launchctl setenv \"$k\" \"$v\";; esac; done"
    );
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>AssociatedBundleIdentifiers</key>
	<array>
		<string>{}</string>
	</array>
	<key>Label</key>
	<string>com.mcpane.env</string>
	<key>ProgramArguments</key>
	<array>
		<string>/bin/sh</string>
		<string>-c</string>
		<string>{}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
</dict>
</plist>
"#,
        BUNDLE_IDENTIFIER,
        xml_escape(&script)
    )
}

#[cfg(target_os = "macos")]
pub(super) use macos::PlatformSession;

#[cfg(target_os = "macos")]
mod macos {
    use super::{AgentState, SessionSync, render_agent_plist};
    use std::path::Path;

    pub(in super::super) struct PlatformSession;

    fn launchctl(args: &[&str]) -> Result<(), String> {
        let output = std::process::Command::new("/bin/launchctl")
            .args(args)
            .output()
            .map_err(|e| format!("launchctl 启动失败：{e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "launchctl {} 失败：{}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }

    /// 以 HOME 属主 uid 定位 GUI 域（与 hub-protocol 的回退目录同一技巧，免 libc）。
    fn gui_domain() -> Option<String> {
        use std::os::unix::fs::MetadataExt;
        let home = std::env::var("HOME").ok()?;
        let uid = std::fs::metadata(home).ok()?.uid();
        Some(format!("gui/{uid}/com.mcpane.env"))
    }

    impl SessionSync for PlatformSession {
        fn set_var(&self, var: &str, value: &str) -> Result<(), String> {
            launchctl(&["setenv", var, value])
        }

        fn unset_var(&self, var: &str) -> Result<(), String> {
            launchctl(&["unsetenv", var])
        }

        fn install_agent(&self, env_file: &Path, agent_path: &Path) -> Result<AgentState, String> {
            let desired = render_agent_plist(env_file);
            let existing = std::fs::read_to_string(agent_path).ok();
            if existing.as_deref() == Some(desired.as_str()) {
                return Ok(AgentState::Unchanged);
            }
            let state = if existing.is_some() {
                AgentState::Updated
            } else {
                AgentState::Installed
            };
            if let Some(dir) = agent_path.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("创建 {} 失败：{e}", dir.display()))?;
            }
            std::fs::write(agent_path, desired)
                .map_err(|e| format!("写入 {} 失败：{e}", agent_path.display()))?;
            // 不主动 bootstrap：当前会话已由 setenv 覆盖，登录时 launchd 自动加载。
            Ok(state)
        }

        fn remove_agent(&self, agent_path: &Path) -> Result<(), String> {
            if let Some(domain) = gui_domain() {
                // 尽力卸载已登记的 job；未加载/不存在都不算错。
                let _ = launchctl(&["bootout", &domain]);
            }
            match std::fs::remove_file(agent_path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("删除 {} 失败：{e}", agent_path.display())),
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub(super) use linux::PlatformSession;

#[cfg(target_os = "linux")]
mod linux {
    use super::{AgentState, SessionSync};
    use std::path::Path;

    /// Linux 无会话级注入通道（GUI 覆盖靠 store 层的 environment.d）。
    pub(in super::super) struct PlatformSession;

    impl SessionSync for PlatformSession {
        fn set_var(&self, _var: &str, _value: &str) -> Result<(), String> {
            Ok(())
        }
        fn unset_var(&self, _var: &str) -> Result<(), String> {
            Ok(())
        }
        fn install_agent(
            &self,
            _env_file: &Path,
            _agent_path: &Path,
        ) -> Result<AgentState, String> {
            Ok(AgentState::Unchanged)
        }
        fn remove_agent(&self, _agent_path: &Path) -> Result<(), String> {
            Ok(())
        }
    }
}

#[cfg(windows)]
pub(super) use windows_impl::PlatformSession;

#[cfg(windows)]
mod windows_impl {
    // 豁免理由：WM_SETTINGCHANGE 广播必须走 Win32 SendMessageTimeoutW，
    // unsafe 面收敛在本模块（仅此一处），失败降级为警告不影响持久化本体。
    #![allow(unsafe_code)]

    use super::{AgentState, SessionSync};
    use std::path::Path;

    pub(in super::super) struct PlatformSession;

    fn hkcu_env() -> Result<winreg::RegKey, String> {
        use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
        winreg::RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags("Environment", KEY_SET_VALUE)
            .map_err(|e| format!("打开 HKCU\\Environment 失败：{e}"))
    }

    /// 通知已运行进程用户环境块已变（Explorer 刷新后新启动的 GUI 继承新值）。
    fn broadcast_change() -> Result<(), String> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
        };
        let param: Vec<u16> = "Environment\0".encode_utf16().collect();
        let sent = unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                param.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                5000,
                std::ptr::null_mut(),
            )
        };
        if sent == 0 {
            Err("WM_SETTINGCHANGE 广播超时（注销重登后生效）".to_owned())
        } else {
            Ok(())
        }
    }

    impl SessionSync for PlatformSession {
        fn set_var(&self, var: &str, value: &str) -> Result<(), String> {
            hkcu_env()?
                .set_value(var, &value)
                .map_err(|e| format!("写入 HKCU\\Environment\\{var} 失败：{e}"))?;
            // 注册表是存储本体；广播失败仅影响已运行进程的感知，不算失败。
            let _ = broadcast_change();
            Ok(())
        }

        fn unset_var(&self, var: &str) -> Result<(), String> {
            let key = hkcu_env()?;
            match key.delete_value(var) {
                Ok(()) => {
                    let _ = broadcast_change();
                    Ok(())
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("删除 HKCU\\Environment\\{var} 失败：{e}")),
            }
        }

        fn install_agent(
            &self,
            _env_file: &Path,
            _agent_path: &Path,
        ) -> Result<AgentState, String> {
            Ok(AgentState::Unchanged)
        }
        fn remove_agent(&self, _agent_path: &Path) -> Result<(), String> {
            Ok(())
        }
    }
}
