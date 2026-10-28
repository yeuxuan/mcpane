//! 下游远程 MCP 的 OAuth 登录（ADR-024）。
//!
//! 分工：桌面端负责**浏览器与本机回环回调**，Hub 负责协议与令牌（发现、注册、PKCE、换令牌、刷新、
//! 落盘）。令牌从不经过桌面：这里只拿到授权链接，再把浏览器带回来的 `code` / `state` 原样交给 Hub。
//!
//! 一次登录：绑 `127.0.0.1` 随机端口 → `oauth-begin`（Hub 返回授权链接）→ 打开系统浏览器 →
//! 等回调（最长 5 分钟，可取消）→ `oauth-complete`（Hub 校验 `state`、换令牌、重连）。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use mcpane_hub_protocol::ControlCommand;
use tauri_plugin_opener::OpenerExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::hub::{control_call, require_ok};

/// 用户在浏览器里完成授权的最长等待。
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// 单个回调连接读请求头的上限（防慢速连接占着监听）。
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_REQUEST_BYTES: usize = 8 * 1024;

/// 进行中的登录（server_id → 等待回调任务的中止句柄），用来取消与顶替。
static IN_FLIGHT: Mutex<Option<HashMap<String, tokio::task::AbortHandle>>> = Mutex::new(None);

fn in_flight<R>(f: impl FnOnce(&mut HashMap<String, tokio::task::AbortHandle>) -> R) -> R {
    let mut guard = IN_FLIGHT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(guard.get_or_insert_with(HashMap::new))
}

/// 浏览器带回来的授权结果。
#[derive(Debug, PartialEq, Eq)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    /// 授权服务器报的错（用户点了拒绝等）。
    error: Option<String>,
}

/// 从原始 HTTP 请求头里取出 `/callback` 的查询参数；不是回调（favicon、探测）返回 `None`。
fn parse_callback(head: &str) -> Option<Callback> {
    let mut parts = head.lines().next()?.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    let url = url::Url::parse(&format!("http://127.0.0.1{target}")).ok()?;
    if url.path() != "/callback" {
        return None;
    }
    let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
    let callback = Callback {
        code: query.get("code").cloned(),
        state: query.get("state").cloned(),
        error: query.get("error").cloned(),
    };
    // 既没有 code 也没有 error 的 /callback 不是授权结果（手敲的、扫描器的）。
    (callback.code.is_some() || callback.error.is_some()).then_some(callback)
}

async fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let page = format!(
        "<!doctype html><meta charset=utf-8><title>MCPane</title>\
         <body style=\"font:16px system-ui;margin:3rem\">{body}</body>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\n\
         connection: close\r\ncache-control: no-store\r\n\r\n{page}",
        page.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// 等第一份有效的授权回调。其他请求（图标、探测）回 404 后继续等。
async fn wait_for_callback(listener: TcpListener) -> Result<Callback, String> {
    loop {
        let (mut stream, _) = listener.accept().await.map_err(|e| e.to_string())?;
        let mut head = Vec::new();
        let mut buffer = [0u8; 1024];
        let read = tokio::time::timeout(READ_TIMEOUT, async {
            while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < MAX_REQUEST_BYTES {
                match stream.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => head.extend_from_slice(&buffer[..n]),
                }
            }
        })
        .await;
        if read.is_err() {
            continue;
        }
        match parse_callback(&String::from_utf8_lossy(&head)) {
            Some(callback) => {
                let body = if callback.error.is_some() {
                    "授权没有完成，可以关闭此页面回到 MCPane。"
                } else {
                    "授权完成，可以关闭此页面回到 MCPane。"
                };
                respond(&mut stream, "200 OK", body).await;
                return Ok(callback);
            }
            None => respond(&mut stream, "404 Not Found", "未找到").await,
        }
    }
}

/// 登录一个远程 MCP 服务器：打开浏览器授权，成功后 Hub 已重连并拿到令牌。
#[tauri::command]
pub async fn server_oauth_login(
    handle: tauri::AppHandle,
    id: String,
) -> Result<serde_json::Value, String> {
    // 同一服务器只留一次进行中的登录：再点一次等于重来。
    cancel_in_flight(&id);
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| format!("无法监听本机回调端口：{e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let begun = require_ok(
        control_call(ControlCommand::OAuthBegin {
            server_id: id.clone(),
            redirect_uri,
        })
        .await?,
        "无法发起登录",
    )?;
    let authorization_url = begun["authorizationUrl"]
        .as_str()
        .ok_or("Hub 没有返回授权链接")?
        .to_owned();

    let waiting = tokio::spawn(wait_for_callback(listener));
    in_flight(|map| map.insert(id.clone(), waiting.abort_handle()));
    if let Err(error) = handle.opener().open_url(authorization_url, None::<&str>) {
        cancel_in_flight(&id);
        abandon(&id).await;
        return Err(format!("无法打开系统浏览器：{error}"));
    }

    let outcome = tokio::time::timeout(LOGIN_TIMEOUT, waiting).await;
    in_flight(|map| map.remove(&id));
    let callback = match outcome {
        Ok(Ok(Ok(callback))) => callback,
        Ok(Ok(Err(error))) => {
            abandon(&id).await;
            return Err(error);
        }
        // 任务被中止：用户取消，或被新一次登录顶替。
        Ok(Err(_)) => {
            abandon(&id).await;
            return Err("登录已取消".into());
        }
        Err(_) => {
            abandon(&id).await;
            return Err("登录超时，请重新点击登录".into());
        }
    };
    if let Some(error) = callback.error {
        abandon(&id).await;
        return Err(format!("授权没有完成：{error}"));
    }
    let (Some(code), Some(state)) = (callback.code, callback.state) else {
        abandon(&id).await;
        return Err("授权回调缺少 code 或 state".into());
    };
    require_ok(
        control_call(ControlCommand::OAuthComplete {
            server_id: id,
            code,
            state,
        })
        .await?,
        "登录失败",
    )
}

/// 取消进行中的登录（等待回调的任务被中止，登录命令随即返回「已取消」）。
#[tauri::command]
pub fn server_oauth_cancel(id: String) {
    cancel_in_flight(&id);
}

/// 退出登录：Hub 删令牌，服务器回到「需要登录」。
#[tauri::command]
pub async fn server_oauth_logout(id: String) -> Result<(), String> {
    require_ok(
        control_call(ControlCommand::OAuthLogout { server_id: id }).await?,
        "退出登录失败",
    )
    .map(|_| ())
}

fn cancel_in_flight(id: &str) {
    if let Some(handle) = in_flight(|map| map.remove(id)) {
        handle.abort();
    }
}

/// 告诉 Hub 这次登录作废（它在内存里暂存了授权状态）；失败无妨，Hub 侧 10 分钟自会过期。
async fn abandon(id: &str) {
    let _ = control_call(ControlCommand::OAuthCancel {
        server_id: id.to_owned(),
    })
    .await;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn run<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn callback_is_parsed_with_percent_decoding() {
        let callback = parse_callback(
            "GET /callback?code=ab%2Fc&state=xyz HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        )
        .unwrap();
        assert_eq!(callback.code.as_deref(), Some("ab/c"));
        assert_eq!(callback.state.as_deref(), Some("xyz"));
        assert_eq!(callback.error, None);
    }

    #[test]
    fn an_authorization_error_is_a_callback_too() {
        let callback =
            parse_callback("GET /callback?error=access_denied&state=xyz HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(callback.error.as_deref(), Some("access_denied"));
        assert_eq!(callback.code, None);
    }

    #[test]
    fn anything_else_is_not_a_callback() {
        for head in [
            "GET /favicon.ico HTTP/1.1\r\n\r\n",
            "GET /callback HTTP/1.1\r\n\r\n",
            "GET /callback?foo=bar HTTP/1.1\r\n\r\n",
            "POST /callback?code=a&state=b HTTP/1.1\r\n\r\n",
            "GET /other?code=a&state=b HTTP/1.1\r\n\r\n",
            "",
            "garbage",
        ] {
            assert_eq!(parse_callback(head), None, "{head:?}");
        }
    }

    /// 真实回环连接：先来一个探测请求（404，继续等），再来真正的回调。
    #[test]
    fn listener_skips_noise_and_returns_the_real_callback() {
        run(async {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let waiting = tokio::spawn(wait_for_callback(listener));
            let get = |path: &'static str| async move {
                let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
                stream
                    .write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
                    .await
                    .unwrap();
                let mut reply = String::new();
                stream.read_to_string(&mut reply).await.unwrap();
                reply
            };
            assert!(get("/favicon.ico").await.starts_with("HTTP/1.1 404"));
            let reply = get("/callback?code=the-code&state=the-state").await;
            assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
            assert!(reply.contains("授权完成"));
            let callback = waiting.await.unwrap().unwrap();
            assert_eq!(callback.code.as_deref(), Some("the-code"));
            assert_eq!(callback.state.as_deref(), Some("the-state"));
        });
    }

    #[test]
    fn cancelling_aborts_the_wait() {
        run(async {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let waiting = tokio::spawn(wait_for_callback(listener));
            in_flight(|map| map.insert("s-cancel".into(), waiting.abort_handle()));
            cancel_in_flight("s-cancel");
            assert!(waiting.await.unwrap_err().is_cancelled());
            assert!(in_flight(|map| !map.contains_key("s-cancel")));
        });
    }
}
