//! WebDAV 传输集成测试：本地 axum mock 服务器上验证
//! PROPFIND 探测 / 乐观 MKCOL / PUT / GET / 401 / etag 条件写语义与端到端同步。

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri, header};
use axum::response::Response;
use mcpane_sync::{PutCondition, RemoteStore, SyncEngine, SyncError, WebDavConfig, WebDavStore};

/// path → (内容, 版本号)；etag = "v{版本号}"。
type Objects = HashMap<String, (Vec<u8>, u64)>;

#[derive(Clone, Default)]
struct DavState {
    objects: Arc<Mutex<Objects>>,
    /// 模拟不支持条件写的服务器：带 If-* 头的 PUT 一律 501。
    reject_conditionals: Arc<std::sync::atomic::AtomicBool>,
}

async fn dav_handler(
    State(state): State<DavState>,
    method: Method,
    uri: Uri,
    headers: header::HeaderMap,
    body: Bytes,
) -> Response {
    // Basic dXNlcjpwYXNz = user:pass
    let authorized = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == "Basic dXNlcjpwYXNz");
    if !authorized {
        return Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(axum::body::Body::empty())
            .unwrap();
    }
    let path = uri.path().to_owned();
    let status = match method.as_str() {
        "PROPFIND" => StatusCode::MULTI_STATUS,
        "MKCOL" => {
            // 第二次 MKCOL 同目录 → 405（已存在），走乐观分支。
            let mut objects = state.objects.lock().unwrap();
            match objects.entry(format!("dir:{path}")) {
                std::collections::hash_map::Entry::Occupied(_) => StatusCode::METHOD_NOT_ALLOWED,
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert((Vec::new(), 0));
                    StatusCode::CREATED
                }
            }
        }
        "PUT" => {
            let if_match = headers.get(header::IF_MATCH).and_then(|v| v.to_str().ok());
            let if_none_match = headers
                .get(header::IF_NONE_MATCH)
                .and_then(|v| v.to_str().ok());
            if state
                .reject_conditionals
                .load(std::sync::atomic::Ordering::Relaxed)
                && (if_match.is_some() || if_none_match.is_some())
            {
                return Response::builder()
                    .status(StatusCode::NOT_IMPLEMENTED)
                    .body(axum::body::Body::empty())
                    .unwrap();
            }
            let mut objects = state.objects.lock().unwrap();
            let current = objects.get(&path).map(|(_, v)| *v);
            // 条件写语义（RFC 7232）：If-Match 强比较；If-None-Match: * 仅当不存在。
            let precondition_ok = match (if_match, if_none_match) {
                (Some(expected), _) => current.is_some_and(|v| expected == format!("\"v{v}\"")),
                (None, Some("*")) => current.is_none(),
                _ => true,
            };
            if !precondition_ok {
                StatusCode::PRECONDITION_FAILED
            } else {
                let next = current.unwrap_or(0) + 1;
                objects.insert(path, (body.to_vec(), next));
                StatusCode::CREATED
            }
        }
        "DELETE" => {
            if state.objects.lock().unwrap().remove(&path).is_some() {
                StatusCode::NO_CONTENT
            } else {
                StatusCode::NOT_FOUND
            }
        }
        "GET" => {
            let objects = state.objects.lock().unwrap();
            return match objects.get(&path) {
                Some((data, version)) => Response::builder()
                    .status(StatusCode::OK)
                    .header(header::ETAG, format!("\"v{version}\""))
                    .body(axum::body::Body::from(data.clone()))
                    .unwrap(),
                None => Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            };
        }
        _ => StatusCode::METHOD_NOT_ALLOWED,
    };
    Response::builder()
        .status(status)
        .body(axum::body::Body::empty())
        .unwrap()
}

async fn spawn_dav() -> (String, DavState) {
    let state = DavState::default();
    let app = axum::Router::new()
        .fallback(dav_handler)
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/dav/box"), state)
}

fn store_for(base_url: &str, password: &str) -> WebDavStore {
    WebDavStore::new(WebDavConfig {
        base_url: base_url.to_owned(),
        username: "user".into(),
        password: password.into(),
        remote_root: "mcpane-sync".into(),
    })
    .unwrap()
}

#[tokio::test]
async fn webdav_store_full_object_lifecycle() {
    let (base_url, _state) = spawn_dav().await;
    let store = store_for(&base_url, "pass");

    store.test().await.unwrap();
    // 两次 ensure（第二次命中 405 乐观分支）。
    store.ensure_dirs().await.unwrap();
    store.ensure_dirs().await.unwrap();

    assert!(store.get("manifest.json", 1024).await.unwrap().is_none());
    store
        .put("manifest.json", b"{\"a\":1}".to_vec(), PutCondition::None)
        .await
        .unwrap();
    let obj = store.get("manifest.json", 1024).await.unwrap().unwrap();
    assert_eq!(obj.data, b"{\"a\":1}");
    assert_eq!(obj.etag.as_deref(), Some("\"v1\""));
    // 大小上限：超限即错。
    assert!(store.get("manifest.json", 3).await.is_err());
}

#[tokio::test]
async fn webdav_conditional_put_semantics() {
    let (base_url, _state) = spawn_dav().await;
    let store = store_for(&base_url, "pass");
    store.ensure_dirs().await.unwrap();

    // IfAbsent 首写成功；二次 IfAbsent → 412 → Conflict。
    store
        .put("manifest.json", b"v1".to_vec(), PutCondition::IfAbsent)
        .await
        .unwrap();
    let err = store
        .put("manifest.json", b"x".to_vec(), PutCondition::IfAbsent)
        .await
        .unwrap_err();
    assert!(matches!(err, SyncError::Conflict(_)), "{err}");

    // 旧 etag IfMatch → Conflict；现行 etag → 放行。
    let etag = store
        .get("manifest.json", 1024)
        .await
        .unwrap()
        .unwrap()
        .etag
        .unwrap();
    let err = store
        .put(
            "manifest.json",
            b"x".to_vec(),
            PutCondition::IfMatch("\"v999\"".into()),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, SyncError::Conflict(_)), "{err}");
    store
        .put("manifest.json", b"v2".to_vec(), PutCondition::IfMatch(etag))
        .await
        .unwrap();
}

#[tokio::test]
async fn webdav_conditional_put_degrades_on_501() {
    let (base_url, state) = spawn_dav().await;
    let store = store_for(&base_url, "pass");
    store.ensure_dirs().await.unwrap();
    state
        .reject_conditionals
        .store(true, std::sync::atomic::Ordering::Relaxed);

    // 服务器不支持条件写：IfAbsent / IfMatch 均降级为无条件写成功。
    store
        .put("manifest.json", b"v1".to_vec(), PutCondition::IfAbsent)
        .await
        .unwrap();
    store
        .put(
            "manifest.json",
            b"v2".to_vec(),
            PutCondition::IfMatch("\"whatever\"".into()),
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .get("manifest.json", 1024)
            .await
            .unwrap()
            .unwrap()
            .data,
        b"v2",
        "降级重试必须携带原 body"
    );
}

#[tokio::test]
async fn webdav_delete_is_idempotent() {
    let (base_url, _state) = spawn_dav().await;
    let store = store_for(&base_url, "pass");
    store.ensure_dirs().await.unwrap();
    store
        .put("mcpane-abc.db", b"x".to_vec(), PutCondition::None)
        .await
        .unwrap();
    store.delete("mcpane-abc.db").await.unwrap();
    assert!(store.get("mcpane-abc.db", 1024).await.unwrap().is_none());
    // 再删不存在的对象也成功（幂等）。
    store.delete("mcpane-abc.db").await.unwrap();
}

#[tokio::test]
async fn webdav_wrong_password_is_auth_error() {
    let (base_url, _state) = spawn_dav().await;
    let store = store_for(&base_url, "wrong");
    let err = store.test().await.unwrap_err();
    assert!(err.to_string().contains("认证失败"), "{err}");
}

#[tokio::test]
async fn end_to_end_sync_over_webdav() {
    let (base_url, _state) = spawn_dav().await;
    let store = store_for(&base_url, "pass");
    let dir = tempfile::tempdir().unwrap();

    // 设备 A 上传。
    let home_a = dir.path().join("home-a");
    std::fs::create_dir_all(&home_a).unwrap();
    std::fs::write(home_a.join("providers.json"), br#"{"schemaVersion":2}"#).unwrap();
    let pool_a = mcpane_storage::open_database(&dir.path().join("a.db"))
        .await
        .unwrap();
    let up = SyncEngine::new(&pool_a, &home_a)
        .upload(&store, None, false)
        .await
        .unwrap();

    // 设备 B 未下载直接上传 → 冲突（基线不匹配，经真实 HTTP 链路）。
    let home_b = dir.path().join("home-b");
    std::fs::create_dir_all(&home_b).unwrap();
    let pool_b = mcpane_storage::open_database(&dir.path().join("b.db"))
        .await
        .unwrap();
    let engine_b = SyncEngine::new(&pool_b, &home_b);
    let err = engine_b.upload(&store, None, false).await.unwrap_err();
    assert!(matches!(err, SyncError::Conflict(_)), "{err}");

    // 设备 B 下载。
    let info = engine_b.remote_info(&store).await.unwrap();
    assert!(info.exists && info.incompatible.is_none());
    let down = engine_b.download(&store).await.unwrap();
    assert_eq!(down.snapshot_id, up.snapshot_id);
    assert_eq!(
        std::fs::read_to_string(home_b.join("providers.json")).unwrap(),
        r#"{"schemaVersion":2}"#
    );
}
