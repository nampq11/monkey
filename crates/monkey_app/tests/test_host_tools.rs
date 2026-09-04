use axum::Router;
use axum::extract::Json;
use axum::routing::{get, patch, post};
use monkey_app::db::Store;
use monkey_app::gh_writeback::RepoRef;
use monkey_app::host_tools::GHProxy;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

#[tokio::test]
async fn test_push_builds_the_right_request() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let cap_clone = captured.clone();

    let app = Router::new().route(
        "/git/push",
        post(move |Json(body): Json<Value>| {
            let cap = cap_clone.clone();
            async move {
                cap.lock().unwrap().push(body);
                Json(json!({ "ok": true }))
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = Store::new(&db_path).unwrap();

    let repo_ref = RepoRef {
        owner: "acme".into(),
        repo: "widget".into(),
        number: 123,
    };
    let proxy = GHProxy::new(&format!("http://{}", addr), "key", store, &repo_ref).unwrap();

    let result = proxy
        .push(std::path::Path::new("/data/wt"), "farm/abc1234/widget")
        .await
        .unwrap();
    assert_eq!(result["ok"], true);

    let calls = captured.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["worktree"], "/data/wt");
    assert_eq!(calls[0]["branch"], "farm/abc1234/widget");
    assert_eq!(calls[0]["repo"], "acme/widget");
}

#[tokio::test]
async fn test_list_pull_requests_builds_the_right_request() {
    let query_captured = Arc::new(Mutex::new(None));
    let query_clone = query_captured.clone();

    let app = Router::new().route(
        "/pulls/acme/widget",
        get(move |uri: axum::http::Uri| {
            let qc = query_clone.clone();
            async move {
                *qc.lock().unwrap() = uri.query().map(|q| q.to_string());
                Json(json!([{"number": 42}]))
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = Store::new(&db_path).unwrap();

    let repo_ref = RepoRef {
        owner: "acme".into(),
        repo: "widget".into(),
        number: 123,
    };
    let proxy = GHProxy::new(&format!("http://{}", addr), "key", store, &repo_ref).unwrap();

    let result = proxy
        .list_pull_requests(Some("acme:farm/123/branch"), Some("open"))
        .await
        .unwrap();
    assert_eq!(result[0]["number"], 42);
    assert_eq!(
        query_captured.lock().unwrap().as_deref(),
        Some("head=acme:farm/123/branch&state=open")
    );
}

#[tokio::test]
async fn test_update_pull_request_builds_the_right_request() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let cap_clone = captured.clone();

    let app = Router::new().route(
        "/pulls/acme/widget/42",
        patch(move |Json(body): Json<Value>| {
            let cap = cap_clone.clone();
            async move {
                cap.lock().unwrap().push(body);
                Json(json!({"number": 42, "title": "New Title"}))
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = Store::new(&db_path).unwrap();

    let repo_ref = RepoRef {
        owner: "acme".into(),
        repo: "widget".into(),
        number: 123,
    };
    let proxy = GHProxy::new(&format!("http://{}", addr), "key", store, &repo_ref).unwrap();

    let result = proxy
        .update_pull_request(42, json!({"title": "New Title"}))
        .await
        .unwrap();
    assert_eq!(result["number"], 42);
    assert_eq!(captured.lock().unwrap()[0]["title"], "New Title");
}

#[tokio::test]
async fn test_call_handles_empty_response() {
    let app = Router::new().route(
        "/issues/acme/widget/123/labels",
        post(|| async { axum::response::IntoResponse::into_response("") }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = Store::new(&db_path).unwrap();

    let repo_ref = RepoRef {
        owner: "acme".into(),
        repo: "widget".into(),
        number: 123,
    };
    let proxy = GHProxy::new(&format!("http://{}", addr), "key", store, &repo_ref).unwrap();

    let result = proxy.add_labels(&["bug".to_string()]).await.unwrap();
    assert_eq!(result["ok"], true);
}
