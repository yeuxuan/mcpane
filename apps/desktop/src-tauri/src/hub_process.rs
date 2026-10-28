//! Hub sidecar 托管：桌面应用接管 mcpane-hub 的完整生命周期。
//!
//! 状态机：Probing → Disowned（adopt 外部 hub）/ Starting → Managed
//!         → Backoff（退避重启）→ Failed（端口被占 / 连续崩溃）→ ShutDown。
//! - 探活以控制面 UDS status 为权威（能证明对端是 mcpane-hub）；
//!   端口有服务时进一步验明占用者身份（lsof + ps）：自家 mcpane-hub 的
//!   孤儿（PPID==1，上个 App 会话残留）或半僵尸（UDS 死 / TCP 活）一律
//!   SIGTERM 回收后重新拉起受管实例；真正的外部程序才报端口占用；
//! - 应用退出时经控制面 shutdown 优雅关停（3s 超时兜底强杀）；adopt 的外部 hub 不动；
//! - spawn 时设 `MCPANE_SUPERVISED=1`：hub 监听 stdin 管道 EOF 自杀，
//!   兜住 RunEvent::Exit 不触发的死法（dev Ctrl+C / 崩溃 / SIGKILL），无孤儿；
//! - sidecar stdout/stderr 落盘 `mcpane_home()/logs/mcpane-hub.log`。

use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tauri::{Emitter, Manager};
use tauri_plugin_shell::ShellExt;
use tauri_plugin_shell::process::{CommandChild, CommandEvent};

const HUB_TCP_ADDR: &str = mcpane_hub_protocol::DEFAULT_BIND_ADDR;
/// spawn 后等待就绪的上限；超时视为本次启动失败。
const READY_TIMEOUT: Duration = Duration::from_secs(10);
const PROBE_INTERVAL: Duration = Duration::from_millis(250);
/// Disowned 下复查外部 hub 的间隔（连续 2 次失败则接管）。
const ADOPT_RECHECK: Duration = Duration::from_secs(5);
/// 端口被外部程序占用时的复查间隔（占用者退出后自动恢复拉起）。
const PORT_RECHECK: Duration = Duration::from_secs(10);
/// 连续崩溃后等待用户重开偏好或定时重试的间隔。
const CRASH_RECOVERY_RECHECK: Duration = Duration::from_secs(60);
const MAX_CONSECUTIVE_FAILS: u32 = 5;
/// spawn_hub 里 enabled 竞态命中时的错误标记：supervise() 据此识别为
/// 主动关闭而非真实失败，不计入连败计数。
const HUB_DISABLED_RACE: &str = "hub 偏好在 spawn 竞态窗口内被关闭";
/// 稳定运行达到该时长后重置连败计数。
const STABLE_RESET: Duration = Duration::from_secs(30);
const LOG_ROTATE_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum HubProcessState {
    Probing,
    /// 外部 hub 在跑（开发者 cargo run 等），adopt 不托管。
    Disowned,
    Starting,
    Managed {
        pid: u32,
    },
    Backoff {
        attempt: u32,
    },
    Failed {
        reason: FailReason,
        /// 端口占用者身份（仅 PortOccupied，且仅 unix 查得到）。
        /// 单独放在 Failed 上而不是塞进 FailReason：后者是 unit variant，
        /// 序列化成 `"portOccupied"` 字符串，改成 struct variant 会换掉前端契约。
        #[serde(skip_serializing_if = "Option::is_none")]
        occupant: Option<OccupantInfo>,
    },
    /// 用户在设置里关闭了 Hub 偏好：不探活、不拉起，等待偏好重新打开。
    Disabled,
    ShutDown,
}

#[derive(Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FailReason {
    PortOccupied,
    RepeatedCrash,
}

/// 端口占用者的最小身份，够用户自己去结束它。
///
/// 探测早就查得到这些（`port_occupant` 用 lsof + ps，本来只用于判断占用者是不是
/// 自家的僵尸 hub），却从不外传——界面只能说一句「端口被其他程序占用」，
/// 用户接下来该找谁全靠自己猜。
///
/// 只带进程名与 pid：完整命令行可能含他人的 token 或路径，不进 UI 也不进日志。
#[derive(Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OccupantInfo {
    /// 可执行文件 basename，如 `node`。
    pub name: String,
    pub pid: u32,
}

pub struct HubSupervisor {
    state: Mutex<HubProcessState>,
    child: Mutex<Option<CommandChild>>,
    /// 子进程退出代数：每次 Terminated 递增，供 ready/退出等待订阅。
    exited: tokio::sync::watch::Sender<u64>,
    shutting_down: AtomicBool,
    /// Hub 偏好开关（设置页）；初值 true，supervise() 启动时从库里覆盖。
    enabled: AtomicBool,
    /// 偏好变化的唤醒信号：Disabled 分支靠它从长眠中醒来。
    toggle: tokio::sync::Notify,
}

impl HubSupervisor {
    fn new() -> Self {
        Self {
            state: Mutex::new(HubProcessState::Probing),
            child: Mutex::new(None),
            exited: tokio::sync::watch::Sender::new(0),
            shutting_down: AtomicBool::new(false),
            enabled: AtomicBool::new(true),
            toggle: tokio::sync::Notify::new(),
        }
    }

    /// 设置页调用：切换偏好，关闭时立即杀掉正在托管的子进程。
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
        if !enabled && let Some(child) = self.store_child(None) {
            let _ = child.kill();
        }
        self.toggle.notify_one();
    }

    pub fn snapshot(&self) -> HubProcessState {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    fn set_state(&self, app: &tauri::AppHandle, next: HubProcessState) {
        // 状态未变不重复 emit：端口占用等循环每 10s 复查一轮，
        // 重复事件会让前端反复 invalidate 查询。
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if *state == next {
                return;
            }
            *state = next.clone();
        }
        let _ = app.emit("hub-process-state", &next);
    }

    fn store_child(&self, child: Option<CommandChild>) -> Option<CommandChild> {
        std::mem::replace(
            &mut *self
                .child
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
            child,
        )
    }
}

/// setup 阶段调用：注册托管状态并启动 supervisor 任务。
pub fn spawn(app: &tauri::AppHandle) {
    let supervisor = std::sync::Arc::new(HubSupervisor::new());
    app.manage(supervisor.clone());
    let app = app.clone();
    tauri::async_runtime::spawn(supervise(app, supervisor));
}

/// 优雅关停托管的 hub（控制面 shutdown → 3s 超时强杀）；adopt 的外部 hub 不动。
/// Unix 在控制面不可达时退回 SIGTERM。正常退出路径由 ExitRequested 拦截后在
/// 异步任务里调用，不阻塞主线程事件循环。
/// 返回是否真的关停了受管子进程（无子进程可关时为 false）。
pub async fn shutdown(app: &tauri::AppHandle) -> bool {
    let supervisor = match app.try_state::<std::sync::Arc<HubSupervisor>>() {
        Some(state) => state.inner().clone(),
        None => return false,
    };
    supervisor.shutting_down.store(true, Ordering::SeqCst);
    let Some(child) = supervisor.store_child(None) else {
        return false;
    };

    let mut exited = supervisor.exited.subscribe();
    let asked = crate::commands::request_hub_shutdown().await;
    if asked {
        let graceful = tokio::time::timeout(Duration::from_secs(3), exited.changed())
            .await
            .is_ok();
        if graceful {
            return true;
        }
    }

    #[cfg(unix)]
    {
        // 旧版 Hub 没有 shutdown 命令，或控制面已死：退回 SIGTERM。
        if !asked {
            let pid = child.pid().to_string();
            let sent = std::process::Command::new("kill")
                .args(["-TERM", &pid])
                .status()
                .map(|status| status.success())
                .unwrap_or(false);
            if sent {
                let graceful = tokio::time::timeout(Duration::from_secs(3), exited.changed())
                    .await
                    .is_ok();
                if graceful {
                    return true;
                }
            }
        }
        let _ = child.kill();
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
    true
}

/// RunEvent::Exit 兜底（阻塞主线程）：正常退出已在 ExitRequested 异步清理，
/// 只有绕过该拦截的退出路径才会走到这里。
pub fn shutdown_blocking(app: &tauri::AppHandle) {
    let _ = tauri::async_runtime::block_on(shutdown(app));
}

/// updater 的 on_before_exit 钩子：安装器启动前同步关停 hub 并确认端口释放。
/// Windows 上 updater 是 ShellExecuteW 拉起安装器后 std::process::exit(0) 直退，
/// RunEvent::Exit 永不触发——不在这里关，hub 就会占着安装目录里的
/// mcpane-hub.exe，NSIS 写文件直接报错中断安装。
/// 必须换独立 OS 线程：本函数由 updater 在 tokio 工作线程上同步调用，
/// 就地 block_on 会命中「runtime 内嵌套 runtime」panic。
pub fn shutdown_before_update(app: &tauri::AppHandle) {
    let app = app.clone();
    let _ = std::thread::spawn(move || {
        tauri::async_runtime::block_on(async {
            // kill() 只发终止请求不等回收；以端口释放为进程确已退出的凭据。
            // 没有受管子进程时（hub 被 adopt / 已停）不空等，交由 NSIS 钩子兜底。
            if shutdown(&app).await {
                wait_port_free(Duration::from_secs(3)).await;
            }
        })
    })
    .join();
}

// ---------- 探活 ----------

enum Probe {
    /// 确认是 mcpane-hub 在服务（开发者前台运行或另一 App 实例托管）。
    Hub,
    /// 自家 mcpane-hub 的残留：孤儿（父进程已死）或半僵尸（控制面死、
    /// 数据面活，实锤见 2026-07-13 孤儿事故）。可安全回收后重新拉起。
    StaleHub {
        pid: u32,
    },
    /// 端口有服务但不是 hub（无法通过控制面验明身份）。
    ForeignPort,
    Free,
}

/// 数据面端口监听进程的身份（lsof + ps 查得，仅 unix）。
#[cfg(unix)]
struct PortOccupant {
    pid: u32,
    ppid: u32,
    /// 可执行文件 basename，如 `mcpane-hub`。
    name: String,
}

/// 解析 `lsof -Fp` 输出（`p<pid>` 行）。
#[cfg(unix)]
fn parse_lsof_pid(output: &str) -> Option<u32> {
    output
        .lines()
        .find_map(|line| line.strip_prefix('p')?.trim().parse().ok())
}

/// 解析 `ps -o ppid=,comm=` 输出：`"  704 /path/to/mcpane-hub"`。
/// comm 只按首个空白切一次，路径含空格（如 Application Support）不受影响。
#[cfg(unix)]
fn parse_ps_ppid_comm(output: &str) -> Option<(u32, String)> {
    let (ppid, comm) = output.trim().split_once(char::is_whitespace)?;
    let name = std::path::Path::new(comm.trim())
        .file_name()?
        .to_string_lossy()
        .into_owned();
    Some((ppid.parse().ok()?, name))
}

/// 查询谁在监听数据面端口。lsof/ps 均为系统自带，失败返回 None（按无法验明处理）。
#[cfg(unix)]
fn port_occupant() -> Option<PortOccupant> {
    let lsof = std::process::Command::new("lsof")
        .args([
            "-nP",
            &format!("-iTCP@{HUB_TCP_ADDR}"),
            "-sTCP:LISTEN",
            "-Fp",
        ])
        .output()
        .ok()?;
    let pid = parse_lsof_pid(&String::from_utf8_lossy(&lsof.stdout))?;
    let ps = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "ppid=,comm="])
        .output()
        .ok()?;
    let (ppid, name) = parse_ps_ppid_comm(&String::from_utf8_lossy(&ps.stdout))?;
    Some(PortOccupant { pid, ppid, name })
}

#[cfg(unix)]
fn send_signal(pid: u32, signal: &str) -> bool {
    std::process::Command::new("kill")
        .args([signal, &pid.to_string()])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// 回收残留的自家 hub：SIGTERM 走优雅退出（删控制面 socket、带走下游
/// 子进程），3s 未释放端口再 SIGKILL 兜底。返回端口是否已释放。
#[cfg(unix)]
async fn reclaim_stale_hub(pid: u32) -> bool {
    eprintln!("[hub-supervisor] 检测到残留 mcpane-hub (pid={pid})，回收后重新拉起");
    if send_signal(pid, "-TERM") && wait_port_free(Duration::from_secs(3)).await {
        return true;
    }
    let _ = send_signal(pid, "-KILL");
    wait_port_free(Duration::from_secs(2)).await
}

async fn wait_port_free(limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if !tcp_listening().await {
            return true;
        }
    }
    false
}

fn is_mcpane_hub_process(name: &str) -> bool {
    name.eq_ignore_ascii_case("mcpane-hub") || name.eq_ignore_ascii_case("mcpane-hub.exe")
}

/// `netstat -ano` 一行里找监听 `addr` 的 PID。中文 Windows 状态列是「侦听」。
/// 抽出来给单测喂样例，不依赖真机 netstat。
#[cfg(any(test, windows))]
fn parse_netstat_listener_pid(text: &str, addr: &str) -> Option<u32> {
    for line in text.lines() {
        let listening = line.contains("LISTENING") || line.contains("侦听");
        if !listening || !line.contains(addr) {
            continue;
        }
        return line.split_whitespace().last()?.parse().ok();
    }
    None
}

/// 本进程（桌面 App）的创建时间，编码成单个 u64（FILETIME 高低位拼接）。
/// 供 hub 的 `MCPANE_SUPERVISOR_PID` 兜底轮询做身份核对，见 [`spawn_hub`]。
#[cfg(windows)]
#[allow(unsafe_code)]
fn windows_own_start_filetime() -> Option<u64> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    unsafe {
        let handle = GetCurrentProcess();
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        if GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) == 0 {
            return None;
        }
        Some(((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64)
    }
}

#[cfg(not(windows))]
fn windows_own_start_filetime() -> Option<u64> {
    None
}

#[cfg(windows)]
struct PortOccupant {
    pid: u32,
    name: String,
}

#[cfg(windows)]
fn suppress_console_window(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0800_0000);
}

/// Windows：netstat + tasklist 查监听数据面端口的进程。
#[cfg(windows)]
fn port_occupant() -> Option<PortOccupant> {
    let mut cmd = std::process::Command::new("netstat");
    cmd.args(["-ano"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    suppress_console_window(&mut cmd);
    let output = cmd.output().ok()?;
    let text = mcpane_config::process_output::decode(&output.stdout);
    let pid = parse_netstat_listener_pid(&text, HUB_TCP_ADDR)?;
    let name = windows_process_name(pid)?;
    Some(PortOccupant { pid, name })
}

#[cfg(windows)]
fn windows_process_name(pid: u32) -> Option<String> {
    let mut cmd = std::process::Command::new("tasklist");
    cmd.args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    suppress_console_window(&mut cmd);
    let output = cmd.output().ok()?;
    let line = mcpane_config::process_output::decode(&output.stdout);
    let name = line.split(',').next()?.trim_matches('"');
    if name.is_empty() || name.eq_ignore_ascii_case("INFO:") {
        return None;
    }
    Some(name.to_owned())
}

/// Windows：taskkill /T /F 回收残留 hub。无窗口进程收不到 WM_CLOSE，不先试无 /F。
#[cfg(windows)]
async fn reclaim_stale_hub(pid: u32) -> bool {
    eprintln!("[hub-supervisor] 检测到残留 mcpane-hub (pid={pid})，回收后重新拉起");
    let mut kill = std::process::Command::new("taskkill");
    kill.args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    suppress_console_window(&mut kill);
    let _ = kill.status();
    wait_port_free(Duration::from_secs(2)).await
}

/// 控制面 UDS status 探活（1s 超时）：唯一能证明对端身份的手段。
#[cfg(unix)]
async fn uds_status_ok() -> bool {
    use tokio::io::AsyncWriteExt;
    let socket = mcpane_hub_protocol::control_socket_path();
    let line = serde_json::to_string(&mcpane_hub_protocol::ControlCommand::Status).ok();
    let request = async {
        let mut stream = tokio::net::UnixStream::connect(&socket).await.ok()?;
        stream.write_all((line? + "\n").as_bytes()).await.ok()?;
        // 必须与 control_call 同一套读法：探活若靠 read_to_end 判帧，会与 hub 侧
        // 的等待双向死等到超时，活着的 hub 被探成「已死」并触发回收重拉。
        crate::commands::read_control_response(stream, crate::commands::CONTROL_READ_TIMEOUT)
            .await
            .ok()
    };
    tokio::time::timeout(Duration::from_secs(1), request)
        .await
        .ok()
        .flatten()
        .is_some()
}

/// 控制面 Named Pipe status 探活：与 Unix 的 uds_status_ok 对称。
/// FILE_NOT_FOUND 重试只走 `open_control_pipe_for_probe`，UI 的 `open_control_pipe`
/// 不重试「没开」；这里也不能再套一层 1s 总超时把重试预算吃光。
#[cfg(windows)]
async fn pipe_status_ok() -> bool {
    use tokio::io::AsyncWriteExt;
    let Ok(mut stream) = crate::commands::open_control_pipe_for_probe().await else {
        return false;
    };
    let Some(line) = serde_json::to_string(&mcpane_hub_protocol::ControlCommand::Status).ok()
    else {
        return false;
    };
    let request = async {
        stream.write_all((line + "\n").as_bytes()).await.ok()?;
        crate::commands::read_control_response(stream, crate::commands::CONTROL_READ_TIMEOUT)
            .await
            .ok()
    };
    tokio::time::timeout(Duration::from_secs(1), request)
        .await
        .ok()
        .flatten()
        .is_some()
}

async fn tcp_listening() -> bool {
    tokio::time::timeout(
        Duration::from_millis(500),
        tokio::net::TcpStream::connect(HUB_TCP_ADDR),
    )
    .await
    .is_ok_and(|result| result.is_ok())
}

async fn probe() -> Probe {
    #[cfg(unix)]
    {
        let uds_ok = uds_status_ok().await;
        if !uds_ok && !tcp_listening().await {
            return Probe::Free;
        }
        // 端口有服务：验明占用者身份，决定 adopt / 回收 / 报占用。
        match port_occupant() {
            // 自家 hub 的孤儿（父进程已死，上个 App 会话的残留）即使控制面
            // 还活着也回收重启——App 重开即换受管新实例；半僵尸（UDS 死）
            // 无论孤儿与否都回收：若属于另一活着的 App 实例，对方会重拉健康
            // 实例，下一轮探活将 adopt 它，不会互相踩踏。
            Some(o) if is_mcpane_hub_process(&o.name) && (o.ppid == 1 || !uds_ok) => {
                Probe::StaleHub { pid: o.pid }
            }
            // 控制面可达且非孤儿：开发者前台运行（cargo run）等，adopt 不托管。
            _ if uds_ok => Probe::Hub,
            _ => Probe::ForeignPort,
        }
    }
    #[cfg(windows)]
    {
        let pipe_ok = pipe_status_ok().await;
        if !pipe_ok && !tcp_listening().await {
            return Probe::Free;
        }
        match port_occupant() {
            Some(o) if is_mcpane_hub_process(&o.name) && !pipe_ok => {
                // open 已对 FILE_NOT_FOUND 短重试；换实例空窗仍可能漏过。
                // 再探一次，两次都失败才回收，避免误杀刚 bind 完的活 Hub。
                tokio::time::sleep(Duration::from_millis(200)).await;
                if pipe_status_ok().await {
                    Probe::Hub
                } else {
                    Probe::StaleHub { pid: o.pid }
                }
            }
            _ if pipe_ok => Probe::Hub,
            _ if tcp_listening().await => Probe::ForeignPort,
            _ => Probe::Free,
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        if tcp_listening().await {
            Probe::Hub
        } else {
            Probe::Free
        }
    }
}

/// 就绪判定：unix / Windows 都走控制面（UDS 或 Named Pipe）。
async fn hub_alive() -> bool {
    #[cfg(unix)]
    {
        uds_status_ok().await
    }
    #[cfg(windows)]
    {
        pipe_status_ok().await
    }
    #[cfg(not(any(unix, windows)))]
    {
        tcp_listening().await
    }
}

// ---------- supervisor 主循环 ----------

/// 读取 Hub 偏好；数据库不可达（罕见，如迁移失败）时默认启用，不阻塞启动。
async fn load_hub_enabled_pref() -> bool {
    let Ok(pool) = crate::commands::db_pool().await else {
        return true;
    };
    mcpane_storage::SettingsRepo::new(pool)
        .get_hub_enabled()
        .await
        .unwrap_or(true)
}

async fn supervise(app: tauri::AppHandle, supervisor: std::sync::Arc<HubSupervisor>) {
    let mut attempt: u32 = 0;
    supervisor
        .enabled
        .store(load_hub_enabled_pref().await, Ordering::SeqCst);
    loop {
        if supervisor.shutting_down.load(Ordering::SeqCst) {
            supervisor.set_state(&app, HubProcessState::ShutDown);
            return;
        }
        if !supervisor.enabled.load(Ordering::SeqCst) {
            supervisor.set_state(&app, HubProcessState::Disabled);
            supervisor.toggle.notified().await;
            attempt = 0;
            continue;
        }
        supervisor.set_state(&app, HubProcessState::Probing);
        match probe().await {
            Probe::Hub => {
                supervisor.set_state(&app, HubProcessState::Disowned);
                attempt = 0;
                watch_external(&supervisor).await;
                continue;
            }
            Probe::StaleHub { pid } => {
                if reclaim_stale_hub(pid).await {
                    // 端口已释放：回到循环头重新探活 → Free → 拉起受管实例。
                    attempt = 0;
                    continue;
                }
                // 回收失败（权限异常/进程卡死）：按端口占用退避，复查时会再试。
                eprintln!("[hub-supervisor] 残留 mcpane-hub (pid={pid}) 回收失败，按端口占用退避");
                if port_occupied_hold(&app, &supervisor).await.is_break() {
                    return;
                }
                continue;
            }
            Probe::ForeignPort => {
                if port_occupied_hold(&app, &supervisor).await.is_break() {
                    return;
                }
                continue;
            }
            Probe::Free => {}
        }

        supervisor.set_state(&app, HubProcessState::Starting);
        let mut exited = supervisor.exited.subscribe();
        exited.mark_unchanged();
        match spawn_hub(&app, &supervisor) {
            Err(error) if error == HUB_DISABLED_RACE => {
                // 用户主动关闭：不计入失败次数，顶部循环下一轮转 Disabled。
                continue;
            }
            Err(error) => {
                eprintln!("[hub-supervisor] sidecar 启动失败: {error}");
                attempt += 1;
            }
            Ok(pid) => {
                if wait_ready(&mut exited).await {
                    supervisor.set_state(&app, HubProcessState::Managed { pid });
                    let run_start = Instant::now();
                    let _ = exited.changed().await;
                    supervisor.store_child(None);
                    if supervisor.shutting_down.load(Ordering::SeqCst) {
                        supervisor.set_state(&app, HubProcessState::ShutDown);
                        return;
                    }
                    if !supervisor.enabled.load(Ordering::SeqCst) {
                        // 用户主动关闭：不计入失败次数，顶部循环下一轮转 Disabled。
                        continue;
                    }
                    if run_start.elapsed() >= STABLE_RESET {
                        attempt = 0;
                    }
                    attempt += 1;
                } else {
                    // 就绪超时或启动即退：清掉本次子进程再退避。
                    if let Some(child) = supervisor.store_child(None) {
                        let _ = child.kill();
                    }
                    attempt += 1;
                }
            }
        }

        if attempt >= MAX_CONSECUTIVE_FAILS {
            supervisor.set_state(
                &app,
                HubProcessState::Failed {
                    reason: FailReason::RepeatedCrash,
                    occupant: None,
                },
            );
            if repeated_crash_hold(&app, &supervisor).await.is_break() {
                return;
            }
            attempt = 0;
            continue;
        }
        supervisor.set_state(&app, HubProcessState::Backoff { attempt });
        let backoff = Duration::from_millis(500u64.saturating_mul(1 << attempt.min(6)))
            .min(Duration::from_secs(30));
        // 退避期间没有子进程在跑，关闭偏好不涉及进程泄漏；但用 select 让 toggle
        // 提前唤醒，避免状态最长滞后 30s 才从 Backoff 转到 Disabled。
        tokio::select! {
            () = tokio::time::sleep(backoff) => {}
            () = supervisor.toggle.notified() => {}
        }
    }
}

/// Failed(PortOccupied) 驻留：占用未解除期间原地复查，不回到循环头翻动
/// Probing/Failed 状态（交替 emit 会让前端每 10s 白白 invalidate 两个查询）。
/// 返回 Break 表示应用正在关停（已置 ShutDown），调用方直接结束 supervise。
async fn port_occupied_hold(
    app: &tauri::AppHandle,
    supervisor: &HubSupervisor,
) -> std::ops::ControlFlow<()> {
    // 占用者身份此刻就查得到，报给用户省掉他自己 lsof 一趟。
    let occupant = {
        #[cfg(any(unix, windows))]
        {
            port_occupant().map(|found| OccupantInfo {
                name: found.name,
                pid: found.pid,
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            None
        }
    };
    supervisor.set_state(
        app,
        HubProcessState::Failed {
            reason: FailReason::PortOccupied,
            occupant,
        },
    );
    loop {
        tokio::time::sleep(PORT_RECHECK).await;
        if supervisor.shutting_down.load(Ordering::SeqCst) {
            supervisor.set_state(app, HubProcessState::ShutDown);
            return std::ops::ControlFlow::Break(());
        }
        if !supervisor.enabled.load(Ordering::SeqCst) {
            return std::ops::ControlFlow::Continue(());
        }
        // 占用者变化（消失 / 变成可回收的残留 hub）都交回主循环处理。
        if !matches!(probe().await, Probe::ForeignPort) {
            return std::ops::ControlFlow::Continue(());
        }
    }
}

/// Failed(RepeatedCrash) 驻留：等待用户重开 Hub 偏好或定时重试，不永久退出 supervise。
async fn repeated_crash_hold(
    app: &tauri::AppHandle,
    supervisor: &HubSupervisor,
) -> std::ops::ControlFlow<()> {
    // 单次等待后一律交回主循环重新决策，不在此自旋——主循环拿到 Continue 会
    // 重置 attempt 并重新判断偏好状态，等价于「等一轮再看」。
    tokio::select! {
        () = supervisor.toggle.notified() => {}
        () = tokio::time::sleep(CRASH_RECOVERY_RECHECK) => {}
    }
    if supervisor.shutting_down.load(Ordering::SeqCst) {
        supervisor.set_state(app, HubProcessState::ShutDown);
        return std::ops::ControlFlow::Break(());
    }
    // 偏好已关：交回主循环进入 Disabled；仍开着：定时/ toggle 后重试拉起。
    std::ops::ControlFlow::Continue(())
}

/// Disowned：周期复查外部 hub，连续 2 次不可达则返回（交回主循环接管）。
async fn watch_external(supervisor: &HubSupervisor) {
    let mut misses = 0u32;
    loop {
        tokio::time::sleep(ADOPT_RECHECK).await;
        if supervisor.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        if !supervisor.enabled.load(Ordering::SeqCst) {
            return;
        }
        if hub_alive().await {
            misses = 0;
        } else {
            misses += 1;
            if misses >= 2 {
                return;
            }
        }
    }
}

/// spawn 后轮询就绪；子进程提前退出或超时返回 false。
async fn wait_ready(exited: &mut tokio::sync::watch::Receiver<u64>) -> bool {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if hub_alive().await {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        // 睡一个探测间隔，期间子进程退出则立即失败。
        if tokio::time::timeout(PROBE_INTERVAL, exited.changed())
            .await
            .is_ok()
        {
            return false;
        }
    }
}

// ---------- sidecar spawn 与日志 ----------

fn spawn_hub(
    app: &tauri::AppHandle,
    supervisor: &std::sync::Arc<HubSupervisor>,
) -> Result<u32, String> {
    // 传文件名而非 externalBin 的相对路径：shell 插件按 exe 同目录 join 解析，
    // 而 tauri-build/bundler 都把 sidecar 平铺拷到 exe 旁（无 binaries/ 前缀）。
    // Dock/Finder 启动时 PATH 往往只有系统路径；sidecar 继承它会导致
    // Hub 起的 stdio 下游找不到 npx/uvx。补齐常见 bin 后再注入。
    let path = mcpane_hub_protocol::cli_path::augmented_path_from_env();
    let command = app
        .shell()
        .sidecar("mcpane-hub")
        .map_err(|error| error.to_string())?
        // 托管标记：hub 监听 stdin 管道 EOF，App 无论如何退出（含 dev 模式
        // Ctrl+C、崩溃）都会关闭管道，hub 随之自杀——不再产生孤儿进程。
        .env("MCPANE_SUPERVISED", "1")
        // Windows 兜底（真机复现 2026-08-12）：CreateProcess 的句柄继承在
        // 某些路径下会让管道语义失灵，stdin 永远读不到 EOF，App 被强杀后
        // hub 变孤儿。额外把自身 PID 传给 hub，供它按 MCPANE_SUPERVISOR_PID
        // 轮询父进程是否还活着（见 mcpane-hub/src/main.rs）。
        .env("MCPANE_SUPERVISOR_PID", std::process::id().to_string())
        // 光比 PID 不够：本机高频起停的开发工具几秒内就能把一个已死 PID
        // 复用给无关新进程，单凭 PID 存活会误判"父进程还在"从而永不清理
        // （真机复现：这台机器上确实踩中过）。把自身进程创建时间也带上，
        // hub 每次探活都拿 PID 现存进程的创建时间核对，对不上就当作
        // "原父进程已不在，只是 PID 被复用"。
        .env(
            "MCPANE_SUPERVISOR_START_FILETIME",
            windows_own_start_filetime()
                .map(|v| v.to_string())
                .unwrap_or_default(),
        )
        .env("PATH", path);
    let (mut events, child) = command.spawn().map_err(|error| error.to_string())?;
    let pid = child.pid();
    supervisor.store_child(Some(child));
    // 关竞态窗口：shutdown 可能在 spawn 与 store 之间取走了 None——
    // 存入后复查退出标志，命中则立刻收回强杀，避免应用退出后留孤儿 hub。
    if supervisor.shutting_down.load(Ordering::SeqCst) {
        if let Some(child) = supervisor.store_child(None) {
            let _ = child.kill();
        }
        return Err("应用正在退出，放弃本次拉起".into());
    }
    // 同一个窗口对 enabled 偏好同样成立：set_enabled(false) 若恰好落在
    // probe()→spawn() 之间，此时 child 还没 store，杀不到；这里存入后
    // 补一次复查，命中则立刻收回强杀，避免偏好已关闭但子进程仍在跑。
    if !supervisor.enabled.load(Ordering::SeqCst) {
        if let Some(child) = supervisor.store_child(None) {
            let _ = child.kill();
        }
        return Err(HUB_DISABLED_RACE.into());
    }

    let exited = supervisor.exited.clone();
    let log_path = prepare_log_file();
    tauri::async_runtime::spawn(async move {
        fn open_log(path: &std::path::Path) -> Option<std::fs::File> {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()
        }
        let mut log = log_path.as_deref().and_then(open_log);
        let mut log_bytes = log_path
            .as_deref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0);
        while let Some(event) = events.recv().await {
            match event {
                CommandEvent::Stdout(line) | CommandEvent::Stderr(line) => {
                    if let Some(file) = log.as_mut() {
                        let _ = file.write_all(&line);
                        let _ = file.write_all(b"\n");
                        log_bytes += line.len() as u64 + 1;
                    }
                    // 运行期轮转：hub 长跑（正常路径不重启）时日志不能只增不减。
                    if log_bytes > LOG_ROTATE_BYTES
                        && let Some(path) = log_path.as_deref()
                    {
                        let _ = std::fs::rename(path, path.with_extension("log.1"));
                        log = open_log(path);
                        log_bytes = 0;
                    }
                    #[cfg(debug_assertions)]
                    eprintln!("[hub] {}", String::from_utf8_lossy(&line));
                }
                CommandEvent::Terminated(payload) => {
                    if let Some(file) = log.as_mut() {
                        let _ = writeln!(file, "[supervisor] hub 退出 code={:?}", payload.code);
                    }
                    exited.send_modify(|generation| *generation += 1);
                }
                _ => {}
            }
        }
    });
    Ok(pid)
}

/// 日志路径 `mcpane_home()/logs/mcpane-hub.log`；超 5MB 轮转为 `.1`。
fn prepare_log_file() -> Option<std::path::PathBuf> {
    let dir = crate::commands::mcpane_home().join("logs");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("mcpane-hub.log");
    if let Ok(meta) = std::fs::metadata(&path)
        && meta.len() > LOG_ROTATE_BYTES
    {
        let _ = std::fs::rename(&path, dir.join("mcpane-hub.log.1"));
    }
    Some(path)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn lsof_pid_parses_field_output() {
        assert_eq!(parse_lsof_pid("p67259\n"), Some(67259));
        assert_eq!(parse_lsof_pid("f15\np80629\n"), Some(80629));
        assert_eq!(parse_lsof_pid(""), None);
        assert_eq!(parse_lsof_pid("pabc\n"), None);
    }

    #[test]
    fn ps_output_parses_ppid_and_basename() {
        assert_eq!(
            parse_ps_ppid_comm("    1 /Users/x/target/debug/mcpane-hub\n"),
            Some((1, "mcpane-hub".to_owned()))
        );
        // comm 是含空格的路径也只按首个空白切一次。
        assert_eq!(
            parse_ps_ppid_comm(" 704 /Applications/My App.app/Contents/MacOS/other\n"),
            Some((704, "other".to_owned()))
        );
        assert_eq!(parse_ps_ppid_comm(""), None);
        assert_eq!(parse_ps_ppid_comm("garbage"), None);
    }
}

#[cfg(test)]
mod netstat_parse_tests {
    use super::parse_netstat_listener_pid;

    #[test]
    fn english_listening_line_yields_pid() {
        let text = "  TCP    127.0.0.1:24860    0.0.0.0:0    LISTENING    4242\r\n";
        assert_eq!(
            parse_netstat_listener_pid(text, "127.0.0.1:24860"),
            Some(4242)
        );
    }

    #[test]
    fn chinese_listening_line_yields_pid() {
        let text = "  TCP    127.0.0.1:24860    0.0.0.0:0    侦听    4242\r\n";
        assert_eq!(
            parse_netstat_listener_pid(text, "127.0.0.1:24860"),
            Some(4242)
        );
    }

    #[test]
    fn established_connection_is_ignored() {
        let text = "  TCP    127.0.0.1:24860    127.0.0.1:9    ESTABLISHED    99\r\n";
        assert_eq!(parse_netstat_listener_pid(text, "127.0.0.1:24860"), None);
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    /// 真机复现（2026-08-12）：这条给 hub 的 PID 复用兜底轮询用，取不到就该
    /// 传 `None` 退化，而不是让 `spawn_hub` panic 或者传出垃圾值。
    #[test]
    fn own_start_filetime_is_available_and_stable() {
        let first = windows_own_start_filetime();
        assert!(first.is_some(), "本进程自身的创建时间必须能取到");
        let second = windows_own_start_filetime();
        assert_eq!(first, second, "同一进程连续两次查询必须得到同一个创建时间");
    }
}
