//! S3 传输集成测试：本地 axum mock（path-style）验证 GET/PUT/HEAD/DELETE、
//! etag 提取与条件写语义。SigV4 签名正确性由真机端点验收（错误签名会被
//! 服务端 403 拒绝），mock 只校验 Authorization 头存在。

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri, header};
use axum::response::Response;
use mcpane_sync::{PutCondition, RemoteStore, S3Config, S3Store, SyncError};

type Objects = HashMap<String, (Vec<u8>, u64)>;

#[derive(Clone, Default)]
struct S3State {
    objects: Arc<Mutex<Objects>>,
}

async fn s3_handler(
    State(state): State<S3State>,
    method: Method,
    uri: Uri,
    headers: header::HeaderMap,
    body: Bytes,
) -> Response {
    // SigV4 请求必须带 Authorization（内容不在 mock 校验范围）。
    if headers.get(header::AUTHORIZATION).is_none() {
        return Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(axum::body::Body::empty())
            .unwrap();
    }
    let path = uri.path().to_owned();
    let empty = Response::builder();
    match method {
        Method::HEAD | Method::GET => {
            let objects = state.objects.lock().unwrap();
            match objects.get(&path) {
                Some((data, version)) => {
                    let payload = if method == Method::GET {
                        axum::body::Body::from(data.clone())
                    } else {
                        axum::body::Body::empty()
                    };
                    empty
                        .status(StatusCode::OK)
                        .header(header::ETAG, format!("\"v{version}\""))
                        .body(payload)
                        .unwrap()
                }
                None => empty
                    .status(StatusCode::NOT_FOUND)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            }
        }
        Method::PUT => {
            let mut objects = state.objects.lock().unwrap();
            let current = objects.get(&path).map(|(_, v)| *v);
            let if_match = headers.get(header::IF_MATCH).and_then(|v| v.to_str().ok());
            let if_none_match = headers
                .get(header::IF_NONE_MATCH)
                .and_then(|v| v.to_str().ok());
            let precondition_ok = match (if_match, if_none_match) {
                (Some(expected), _) => current.is_some_and(|v| expected == format!("\"v{v}\"")),
                (None, Some("*")) => current.is_none(),
                _ => true,
            };
            let status = if precondition_ok {
                let next = current.unwrap_or(0) + 1;
                objects.insert(path, (body.to_vec(), next));
                StatusCode::OK
            } else {
                StatusCode::PRECONDITION_FAILED
            };
            empty
                .status(status)
                .body(axum::body::Body::empty())
                .unwrap()
        }
        Method::DELETE => {
            let status = if state.objects.lock().unwrap().remove(&path).is_some() {
                StatusCode::NO_CONTENT
            } else {
                StatusCode::NOT_FOUND
            };
            empty
                .status(status)
                .body(axum::body::Body::empty())
                .unwrap()
        }
        _ => empty
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .body(axum::body::Body::empty())
            .unwrap(),
    }
}

async fn spawn_s3() -> (String, S3State) {
    let state = S3State::default();
    let app = axum::Router::new()
        .fallback(s3_handler)
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), state)
}

fn store_for(endpoint: &str) -> S3Store {
    S3Store::new(S3Config {
        region: "us-east-1".into(),
        bucket: "bucket".into(),
        access_key_id: "ak".into(),
        secret_access_key: "sk".into(),
        endpoint: endpoint.to_owned(),
        remote_root: "mcpane-sync".into(),
    })
    .unwrap()
}

#[tokio::test]
async fn s3_store_full_object_lifecycle() {
    let (endpoint, _state) = spawn_s3().await;
    let store = store_for(&endpoint);

    // HEAD 404 也算连通（test 命令语义）。
    store.test().await.unwrap();
    store.ensure_dirs().await.unwrap();

    assert!(store.get("manifest.json", 1024).await.unwrap().is_none());
    store
        .put("manifest.json", b"{\"a\":1}".to_vec(), PutCondition::None)
        .await
        .unwrap();
    let obj = store.get("manifest.json", 1024).await.unwrap().unwrap();
    assert_eq!(obj.data, b"{\"a\":1}");
    assert_eq!(obj.etag.as_deref(), Some("\"v1\""));
    assert!(store.get("manifest.json", 3).await.is_err(), "大小上限");

    store.delete("manifest.json").await.unwrap();
    assert!(store.get("manifest.json", 1024).await.unwrap().is_none());
    store.delete("manifest.json").await.unwrap(); // 幂等
}

#[tokio::test]
async fn s3_conditional_put_semantics() {
    let (endpoint, _state) = spawn_s3().await;
    let store = store_for(&endpoint);

    store
        .put("manifest.json", b"v1".to_vec(), PutCondition::IfAbsent)
        .await
        .unwrap();
    let err = store
        .put("manifest.json", b"x".to_vec(), PutCondition::IfAbsent)
        .await
        .unwrap_err();
    assert!(matches!(err, SyncError::Conflict(_)), "{err}");

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
    assert_eq!(
        store
            .get("manifest.json", 1024)
            .await
            .unwrap()
            .unwrap()
            .data,
        b"v2"
    );
}
