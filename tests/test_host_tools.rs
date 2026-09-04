use axum::Router;
use axum::extract::Json;
use axum::routing::post;
use monkey::db::Store;
use monkey::gh_writeback::RepoRef;
use monkey::host_tools::GHProxy;
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
    let proxy = GHProxy::new(&format!("http://{}", addr), "key", store, &repo_ref);

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
