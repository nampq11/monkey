use monkey::adapters::Outcome;
use monkey::db::Store;
use monkey::gh_writeback::{RepoRef, has_required_headers, open_pr_if_gated};
use monkey::host_tools::GHProxy;
use tempfile::tempdir;

const GOOD_BODY: &str = r#"## Repro
always

## Cause
root

## Fix
patch

## Verification
test passes

Fixes #123
"#;

#[test]
fn test_required_headers_pass_when_present() {
    assert!(has_required_headers(GOOD_BODY, 123));
}

#[test]
fn test_missing_section_fails() {
    let bad = GOOD_BODY.replace("## Fix", "Fix");
    assert!(!has_required_headers(&bad, 123));
}

#[test]
fn test_missing_reference_fails() {
    let bad = GOOD_BODY.replace("Fixes #123", "Addresses #123");
    assert!(!has_required_headers(&bad, 123));
}

#[test]
fn test_accepts_close_and_resolve_on_the_same_issue() {
    assert!(has_required_headers(
        &GOOD_BODY.replace("Fixes #123", "Closes #123"),
        123
    ));
    assert!(has_required_headers(
        &GOOD_BODY.replace("Fixes #123", "Resolves #123"),
        123
    ));
}

#[test]
fn test_rejects_reference_to_a_different_issue() {
    assert!(!has_required_headers(
        &GOOD_BODY.replace("Fixes #123", "Resolves #456"),
        123
    ));
}

#[tokio::test]
async fn test_open_pr_requires_a_real_branch() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = Store::new(&db_path).unwrap();
    let repo_ref = RepoRef {
        owner: "acme".into(),
        repo: "widget".into(),
        number: 123,
    };
    let proxy = GHProxy::new("http://127.0.0.1:9999", "key", store, &repo_ref);

    let outcome = Outcome {
        pr_body: GOOD_BODY.to_string(),
        summary: "Fix the bug".to_string(),
        branch: String::new(), // empty branch!
        ..Default::default()
    };

    let result = open_pr_if_gated(&proxy, &outcome, &repo_ref, std::path::Path::new("/wt"))
        .await
        .unwrap();

    assert_eq!(result["action"], "comment_fallback");
    assert_eq!(result["reason"], "missing_branch");
}

#[tokio::test]
async fn test_open_pr_pushes_branch_then_opens() {
    let pushed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let prs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

    let p_clone = pushed.clone();
    let pr_clone = prs.clone();
    let app = axum::Router::new()
        .route(
            "/git/push",
            axum::routing::post({
                let p = p_clone.clone();
                move |axum::Json(body): axum::Json<serde_json::Value>| {
                    let p = p.clone();
                    async move {
                        p.lock().unwrap().push(body);
                        axum::Json(serde_json::json!({ "ok": true }))
                    }
                }
            }),
        )
        .route(
            "/pulls/acme/widget",
            axum::routing::post({
                let pr = pr_clone.clone();
                move |axum::Json(body): axum::Json<serde_json::Value>| {
                    let pr = pr.clone();
                    async move {
                        pr.lock().unwrap().push(body);
                        axum::Json(serde_json::json!({ "number": 1 }))
                    }
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

    let branch = "farm/abc1234/widget";
    let outcome = Outcome {
        pr_body: GOOD_BODY.to_string(),
        summary: "Fix the bug".to_string(),
        branch: branch.to_string(),
        ..Default::default()
    };

    let result = open_pr_if_gated(&proxy, &outcome, &repo_ref, std::path::Path::new("/wt"))
        .await
        .unwrap();

    assert_eq!(result["action"], "open_pr");
    assert_eq!(pushed.lock().unwrap().len(), 1);
    assert_eq!(pushed.lock().unwrap()[0]["branch"], branch);
    assert_eq!(prs.lock().unwrap().len(), 1);
    assert_eq!(prs.lock().unwrap()[0]["head"], branch);
    assert_eq!(prs.lock().unwrap()[0]["title"], "Fix the bug");
}
