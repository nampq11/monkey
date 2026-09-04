use monkey_app::adapters::Outcome;
use monkey_app::db::Store;
use monkey_app::gh_writeback::{
    RepoRef, clean_pr_title, determine_pr_title, has_required_headers, open_pr_if_gated,
};
use monkey_app::host_tools::GHProxy;
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
    let app = axum::Router::new().route(
        "/issues/acme/widget/123/comment",
        axum::routing::post(|| async { axum::Json(serde_json::json!({"ok": true})) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
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
    let proxy = GHProxy::new(&format!("http://{}", address), "key", store, &repo_ref).unwrap();

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
    let proxy = GHProxy::new(&format!("http://{}", addr), "key", store, &repo_ref).unwrap();

    let branch = "farm/abc1234/widget";
    let summary = format!("{}é", "a".repeat(119));
    let outcome = Outcome {
        pr_body: GOOD_BODY.to_string(),
        summary,
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
    assert_eq!(
        prs.lock().unwrap()[0]["title"],
        format!("A{}", "a".repeat(118))
    );
}

#[test]
fn test_clean_pr_title_strips_conventional_prefix_and_punctuation() {
    assert_eq!(
        clean_pr_title("fix(sandbox): refresh existing git mirror before creating workspaces."),
        "Refresh existing git mirror before creating workspaces"
    );
    assert_eq!(
        clean_pr_title("feat!: support new mode!"),
        "Support new mode"
    );
    assert_eq!(clean_pr_title("## Repro"), "Repro");
    assert_eq!(
        clean_pr_title("Fix stale Git mirrors in sandbox workspace preparation"),
        "Fix stale Git mirrors in sandbox workspace preparation"
    );
}

#[tokio::test]
async fn test_determine_pr_title_prefers_git_commit_subject() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path();
    std::process::Command::new("git")
        .args(["init", "-b", "main", repo_path.to_str().unwrap()])
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args([
            "-C",
            repo_path.to_str().unwrap(),
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "fix(sandbox): refresh mirrors before branching.",
        ])
        .output()
        .unwrap();

    let repo_ref = RepoRef {
        owner: "acme".into(),
        repo: "widget".into(),
        number: 14,
    };
    let outcome = Outcome {
        summary: "## Repro\nStale mirror repro.\n## Fix\nFixed.".to_string(),
        ..Default::default()
    };
    let title = determine_pr_title(repo_path, &outcome, &repo_ref).await;
    assert_eq!(title, "Refresh mirrors before branching");
}

#[tokio::test]
async fn test_determine_pr_title_skips_markdown_headers_in_summary_fallback() {
    let repo_ref = RepoRef {
        owner: "acme".into(),
        repo: "widget".into(),
        number: 14,
    };
    let outcome = Outcome {
        summary: "## Repro\nRefresh mirrors before branching.\n## Fix\nFixed.".to_string(),
        ..Default::default()
    };

    let title =
        determine_pr_title(std::path::Path::new("/no-such-path"), &outcome, &repo_ref).await;
    assert_eq!(title, "Refresh mirrors before branching");
}

#[tokio::test]
async fn test_open_pr_updates_existing_when_already_exists() {
    let pushed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let updated_prs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

    let p_clone = pushed.clone();
    let u_clone = updated_prs.clone();

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
            axum::routing::post(|| async {
                (
                    axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                    axum::Json(serde_json::json!({
                        "error": true,
                        "status": 422,
                        "body": {
                            "message": "Validation Failed",
                            "errors": [{
                                "code": "custom",
                                "message": "A pull request already exists for acme:farm/abc1234/widget.",
                                "resource": "PullRequest"
                            }]
                        }
                    })),
                )
            })
            .get(|| async {
                axum::Json(serde_json::json!([
                    {
                        "number": 42,
                        "title": "Old title",
                        "state": "open"
                    }
                ]))
            }),
        )
        .route(
            "/pulls/acme/widget/42",
            axum::routing::patch({
                let u = u_clone.clone();
                move |axum::Json(body): axum::Json<serde_json::Value>| {
                    let u = u.clone();
                    async move {
                        u.lock().unwrap().push(body);
                        axum::Json(serde_json::json!({
                            "number": 42,
                            "title": "Updated title",
                            "state": "open"
                        }))
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
    let proxy = GHProxy::new(&format!("http://{}", addr), "key", store, &repo_ref).unwrap();

    let branch = "farm/abc1234/widget";
    let outcome = Outcome {
        pr_body: GOOD_BODY.to_string(),
        summary: "Fix stale mirrors in sandbox".to_string(),
        branch: branch.to_string(),
        ..Default::default()
    };

    let result = open_pr_if_gated(&proxy, &outcome, &repo_ref, std::path::Path::new("/wt"))
        .await
        .unwrap();

    assert_eq!(result["action"], "open_pr");
    assert_eq!(result["pr"]["number"], 42);
    assert_eq!(pushed.lock().unwrap().len(), 1);
    assert_eq!(updated_prs.lock().unwrap().len(), 1);
    assert_eq!(
        updated_prs.lock().unwrap()[0]["title"],
        "Fix stale mirrors in sandbox"
    );
}
