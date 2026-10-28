//! Hub 控制面（ADR-007）：类型化命令直达 hub。
//! Unix 走 UDS（共享 socket 路径），Windows 走 Named Pipe（共享管道名）。

use mcpane_hub_protocol::ControlCommand;

// ---------- Hub 控制面（UDS） ----------

/// 单次控制面往返的读超时（快命令：状态/开关/写库类，hub 侧毫秒级完成）。
#[cfg(any(unix, windows))]
pub(crate) const CONTROL_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// 慢命令的读超时。ReloadServers 会串行重连全部下游（每个连接超时 10s）、
/// TestServer 单次连接就允许 15s——统一用 5s 会在 hub 正常干活时提前放弃，
/// 把「重载其实成功了」「连接失败的真实错因」都报成超时假失败。
#[cfg(any(unix, windows))]
const CONTROL_SLOW_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

/// 按命令选读超时：会触发下游连接的命令走宽限，其余保持短超时快速失败。
#[cfg(any(unix, windows))]
fn response_timeout(command: &ControlCommand) -> std::time::Duration {
    match command {
        // OAuth 的发现 / 注册 / 换令牌是真实的网络往返，complete 与 logout 还会重连下游。
        ControlCommand::ReloadServers
        | ControlCommand::TestServer { .. }
        | ControlCommand::OAuthBegin { .. }
        | ControlCommand::OAuthComplete { .. }
        | ControlCommand::OAuthLogout { .. } => CONTROL_SLOW_READ_TIMEOUT,
        _ => CONTROL_READ_TIMEOUT,
    }
}

/// 读回一行 JSON 响应。
///
/// 按行读而非 `read_to_end`：Windows 上 hub 写完要等客户端读走才释放管道
/// （见 hub 侧 `wait_for_peer_close`），两端都靠「对端关闭」判帧会双向死等。
/// 流按值传入，返回即 drop，正好给出服务端等的关闭信号。旧版 hub（响应不带
/// 换行）也兼容：`read_line` 在 EOF 时返回已读内容。
#[cfg(any(unix, windows))]
pub(crate) async fn read_control_response<S>(
    stream: S,
    timeout: std::time::Duration,
) -> Result<serde_json::Value, String>
where
    S: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;
    let mut reader = tokio::io::BufReader::new(stream);
    let mut line = String::new();
    tokio::time::timeout(timeout, reader.read_line(&mut line))
        .await
        .map_err(|_| "控制面响应超时".to_owned())?
        .map_err(|e| e.to_string())?;
    // 空响应 = 对端没写就断了（hub 崩溃/被杀），报「解析失败」会把方向指错。
    if line.trim().is_empty() {
        return Err("控制面无响应：Hub 可能已退出，请重试或重启 Hub".to_owned());
    }
    serde_json::from_str(line.trim()).map_err(|e| format!("控制面响应解析失败：{e}"))
}

/// 向 Hub 控制面 socket 发送一条命令并取回响应。
#[cfg(unix)]
pub(crate) async fn control_call(command: ControlCommand) -> Result<serde_json::Value, String> {
    use tokio::io::AsyncWriteExt;
    let timeout = response_timeout(&command);
    let socket = mcpane_hub_protocol::control_socket_path();
    let mut stream = tokio::net::UnixStream::connect(&socket)
        .await
        .map_err(|e| format!("Hub 未运行或控制面不可达：{e}"))?;
    let line = serde_json::to_string(&command).map_err(|e| e.to_string())? + "\n";
    stream
        .write_all(line.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    read_control_response(stream, timeout).await
}

/// Named Pipe：全忙是 `ERROR_PIPE_BUSY` (231)，换实例空窗是
/// `ERROR_FILE_NOT_FOUND` (2)。BUSY 对 UI 与探活都该短重试；FILE_NOT_FOUND
/// 只在探活里重试——UI 调用把「Hub 没开」当成瞬时错误会白白多等 1 秒。
///
/// 抽成与 OS 无关的纯函数，好让 ubuntu CI 用 `from_raw_os_error` 罩住这条分派。
#[cfg(any(test, windows))]
fn is_pipe_busy(err: &std::io::Error) -> bool {
    matches!(err.raw_os_error(), Some(231))
}

#[cfg(any(test, windows))]
fn is_pipe_absent(err: &std::io::Error) -> bool {
    matches!(err.raw_os_error(), Some(2))
}

/// 打开控制面 Named Pipe。UI 命令路径：只重试 BUSY，Hub 没开立刻失败。
#[cfg(windows)]
pub(crate) async fn open_control_pipe()
-> Result<tokio::net::windows::named_pipe::NamedPipeClient, String> {
    open_control_pipe_inner(false).await
}

/// 探活路径：换实例的毫秒空窗会报 FILE_NOT_FOUND，短重试以免误杀活 Hub。
#[cfg(windows)]
pub(crate) async fn open_control_pipe_for_probe()
-> Result<tokio::net::windows::named_pipe::NamedPipeClient, String> {
    open_control_pipe_inner(true).await
}

#[cfg(windows)]
async fn open_control_pipe_inner(
    retry_absent: bool,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient, String> {
    use tokio::net::windows::named_pipe::ClientOptions;
    let pipe_name = mcpane_hub_protocol::control_pipe_name();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        match ClientOptions::new().open(&pipe_name) {
            Ok(client) => return Ok(client),
            Err(e)
                if (is_pipe_busy(&e) || (retry_absent && is_pipe_absent(&e)))
                    && std::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            Err(e) => return Err(format!("Hub 未运行或控制面不可达：{e}")),
        }
    }
}

/// Windows：经 Named Pipe 客户端发送同一条命令并取回响应（与 UDS 端语义一致）。
#[cfg(windows)]
pub(crate) async fn control_call(command: ControlCommand) -> Result<serde_json::Value, String> {
    use tokio::io::AsyncWriteExt;

    let timeout = response_timeout(&command);
    let mut stream = open_control_pipe().await?;
    let line = serde_json::to_string(&command).map_err(|e| e.to_string())? + "\n";
    stream
        .write_all(line.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    read_control_response(stream, timeout).await
}

#[cfg(not(any(unix, windows)))]
pub(crate) async fn control_call(_command: ControlCommand) -> Result<serde_json::Value, String> {
    Err("当前平台未实现 Hub 控制面".into())
}

/// 请求 Hub 优雅退出。写出 `shutdown` 即视为已送达——Hub 可能在回包前开始退出。
#[cfg(any(unix, windows))]
pub(crate) async fn request_hub_shutdown() -> bool {
    #[cfg(unix)]
    let stream = {
        let socket = mcpane_hub_protocol::control_socket_path();
        tokio::net::UnixStream::connect(&socket).await.ok()
    };
    #[cfg(windows)]
    let stream = open_control_pipe().await.ok();
    let Some(stream) = stream else {
        return false;
    };
    write_shutdown_and_drain(stream).await
}

#[cfg(any(unix, windows))]
async fn write_shutdown_and_drain<S>(mut stream: S) -> bool
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    let Ok(line) = serde_json::to_string(&ControlCommand::Shutdown) else {
        return false;
    };
    if stream.write_all((line + "\n").as_bytes()).await.is_err() {
        return false;
    }
    let _ = read_control_response(stream, std::time::Duration::from_secs(2)).await;
    true
}

#[cfg(not(any(unix, windows)))]
pub(crate) async fn request_hub_shutdown() -> bool {
    false
}

/// 控制面把业务错误装在 `ok:false` 响应体里；这里统一转成 Err，让前端的
/// onError/乐观回滚真正触发——否则「hub 答复了一个失败」在前端看来是成功。
///
/// Status / Diagnostics / ListCatalog 成功时直接返回 payload（无 `ok` 字段）；
/// 只有显式 `{ ok: false }` 或 `{ ok: true }` 才按信封语义处理。
pub(crate) fn require_ok(
    response: serde_json::Value,
    fallback: &str,
) -> Result<serde_json::Value, String> {
    match response.get("ok") {
        None => Ok(response),
        Some(v) if *v == serde_json::json!(true) => Ok(response),
        Some(_) => Err(response["error"].as_str().unwrap_or(fallback).to_owned()),
    }
}

/// 写库成功后通知 Hub 重载下游，失败视 Hub 状态决定是否上抛。
///
/// Hub 在跑时**没有**任何定时收敛机制（健康监控只重连、不重读 DB），通知
/// 失败意味着 Hub 会一直带着旧配置跑到下一次成功 reload——静默吞掉会让
/// UI 呈现「开关翻转 + 转圈结束」的假成功。但「Hub 没在跑」是另一回事：
/// 下次启动 `load_downstreams` 本来就从 DB 收敛，此时报「未生效」是假警报
/// （启动/退避窗口里用户改配置完全正常）。故按 supervisor 状态分流：
/// - Managed/Disowned（有 hub 在跑却重载失败）与 Failed（不会自行收敛）→ 上抛；
/// - Disabled/Probing/Starting/Backoff/ShutDown → 静默，留启动收敛。
pub(crate) async fn notify_hub_reload(app: &tauri::AppHandle) -> Result<(), String> {
    use crate::hub_process::{HubProcessState, HubSupervisor};
    use tauri::Manager;
    let error = match control_call(ControlCommand::ReloadServers).await {
        Ok(response) if response["ok"] == serde_json::json!(true) => return Ok(()),
        Ok(response) => response["error"]
            .as_str()
            .unwrap_or("Hub 拒绝重载")
            .to_owned(),
        Err(error) => error,
    };
    let state = app.state::<std::sync::Arc<HubSupervisor>>().snapshot();
    let must_surface = matches!(
        state,
        HubProcessState::Managed { .. }
            | HubProcessState::Disowned
            | HubProcessState::Failed { .. }
    );
    if must_surface {
        Err(format!("配置已保存，但 Hub 未生效：{error}"))
    } else {
        eprintln!("[mcpane] Hub 未运行，配置已入库，将在 Hub 下次启动时生效");
        Ok(())
    }
}

#[tauri::command]
pub async fn hub_status() -> Result<serde_json::Value, String> {
    require_ok(
        control_call(ControlCommand::Status).await?,
        "Hub 状态查询失败",
    )
}

/// Hub 托管进程状态（supervisor 状态机快照，见 hub_process.rs）。
#[tauri::command]
pub async fn hub_process_state(
    supervisor: tauri::State<'_, std::sync::Arc<crate::hub_process::HubSupervisor>>,
) -> Result<crate::hub_process::HubProcessState, String> {
    Ok(supervisor.snapshot())
}

#[tauri::command]
pub async fn hub_diagnostics() -> Result<serde_json::Value, String> {
    require_ok(
        control_call(ControlCommand::Diagnostics).await?,
        "Hub 诊断查询失败",
    )
}

#[tauri::command]
pub async fn connection_create(
    name: String,
    client_type: String,
) -> Result<serde_json::Value, String> {
    let result = control_call(ControlCommand::CreateConnection { name, client_type }).await;
    crate::auto_sync::notify_changed();
    require_ok(result?, "创建连接失败")
}

#[tauri::command]
pub async fn connection_delete(client_connection_id: String) -> Result<serde_json::Value, String> {
    let result = control_call(ControlCommand::DeleteConnection {
        client_connection_id,
    })
    .await;
    crate::auto_sync::notify_changed();
    require_ok(result?, "删除连接失败")
}

#[tauri::command]
pub async fn connection_set_enabled(
    client_connection_id: String,
    enabled: bool,
) -> Result<serde_json::Value, String> {
    let command = if enabled {
        ControlCommand::EnableToken {
            client_connection_id,
        }
    } else {
        ControlCommand::RevokeToken {
            client_connection_id,
        }
    };
    let result = control_call(command).await;
    crate::auto_sync::notify_changed();
    require_ok(result?, "Token 状态更新失败")
}

/// 工具暴露模式：flat（平铺）/ gateway（网关）/ auto（按客户端自适应，ADR-020）。
#[tauri::command]
pub async fn connection_set_tool_exposure(
    client_connection_id: String,
    mode: String,
) -> Result<(), String> {
    let response = control_call(ControlCommand::SetConnectionToolExposure {
        client_connection_id,
        mode,
    })
    .await?;
    crate::auto_sync::notify_changed();
    require_ok(response, "暴露模式更新失败").map(|_| ())
}

/// 访问控制：某连接被拒绝访问的 Server ID 列表（直读 SQLite，不依赖 hub 运行）。
#[tauri::command]
pub async fn connection_denied_servers(
    client_connection_id: String,
) -> Result<Vec<String>, String> {
    let pool = super::db_pool().await?;
    mcpane_storage::ConnectionRepo::new(pool)
        .denied_servers(&client_connection_id)
        .await
        .map_err(|e| e.to_string())
}

/// 访问控制：开关某连接对某 Server 的访问权（走控制面，hub 内存即时生效）。
/// 控制面把业务错误装在 `ok:false` 响应体里，这里转成 Err 让前端乐观更新能回滚。
#[tauri::command]
pub async fn connection_server_access_set(
    client_connection_id: String,
    server_id: String,
    allowed: bool,
) -> Result<(), String> {
    let response = control_call(ControlCommand::SetConnectionServerAccess {
        client_connection_id,
        server_id,
        allowed,
    })
    .await?;
    crate::auto_sync::notify_changed();
    require_ok(response, "访问控制更新失败").map(|_| ())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::require_ok;

    #[test]
    fn require_ok_passes_direct_payload_without_ok_field() {
        let status = serde_json::json!({ "name": "mcpane-hub", "uptimeSecs": 1 });
        let out = require_ok(status.clone(), "fail").unwrap();
        assert_eq!(out, status);
    }

    #[test]
    fn require_ok_rejects_explicit_false_envelope() {
        let err = require_ok(
            serde_json::json!({ "ok": false, "error": "Workspace 无 Catalog" }),
            "fallback",
        )
        .unwrap_err();
        assert_eq!(err, "Workspace 无 Catalog");
    }

    #[test]
    fn require_ok_accepts_true_envelope() {
        let out = require_ok(serde_json::json!({ "ok": true, "token": "x" }), "fail").unwrap();
        assert_eq!(out["token"], "x");
    }

    #[test]
    fn pipe_ui_retries_busy_but_not_absent() {
        use super::{is_pipe_absent, is_pipe_busy};
        assert!(is_pipe_busy(&std::io::Error::from_raw_os_error(231)));
        assert!(!is_pipe_busy(&std::io::Error::from_raw_os_error(2)));
        assert!(is_pipe_absent(&std::io::Error::from_raw_os_error(2)));
        assert!(!is_pipe_absent(&std::io::Error::from_raw_os_error(231)));
        assert!(!is_pipe_busy(&std::io::Error::from_raw_os_error(5)));
        assert!(!is_pipe_absent(&std::io::Error::from_raw_os_error(5)));
        assert!(!is_pipe_busy(&std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "nope"
        )));
    }
}
