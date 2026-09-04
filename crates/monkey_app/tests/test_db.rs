use monkey_app::db::Store;
use tempfile::tempdir;

#[tokio::test]
async fn test_enqueue_new_event_inserts() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = Store::new(&db_path).unwrap();

    let is_new = store
        .enqueue("d1", "issues", "acme", "widget", 1, "{}")
        .await
        .unwrap();
    assert!(is_new);
}

#[tokio::test]
async fn test_enqueue_dedup_returns_false() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = Store::new(&db_path).unwrap();

    let is_new1 = store
        .enqueue("d1", "issues", "acme", "widget", 1, "{}")
        .await
        .unwrap();
    assert!(is_new1);

    let is_new2 = store
        .enqueue("d1", "issues", "acme", "widget", 1, "{}")
        .await
        .unwrap();
    assert!(!is_new2);

    let pending = store.pending_events(10).await.unwrap();
    assert_eq!(pending.len(), 1);
}

#[tokio::test]
async fn test_claim_and_finish_flow() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = Store::new(&db_path).unwrap();

    store
        .enqueue("d1", "issues", "acme", "widget", 1, "{}")
        .await
        .unwrap();

    assert!(store.claim("d1").await.unwrap());
    // second claim fails (already running)
    assert!(!store.claim("d1").await.unwrap());

    store.done("d1", Some("/data/sessions/x")).await.unwrap();
    let pending = store.pending_events(10).await.unwrap();
    assert_eq!(pending.len(), 0);
}

#[tokio::test]
async fn test_prior_session_dir_excludes_current_event() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = Store::new(&db_path).unwrap();

    // A completed prior run recorded its session dir.
    store
        .enqueue("seed", "issues", "acme", "widget", 1, "{}")
        .await
        .unwrap();
    store.claim("seed").await.unwrap();
    store
        .done("seed", Some("/sessions/acme__widget__1"))
        .await
        .unwrap();

    // The current pending follow-up event.
    store
        .enqueue("d1", "issue_comment", "acme", "widget", 1, "{}")
        .await
        .unwrap();

    // Looking up prior history for the current event finds the earlier run...
    let prior = store
        .prior_session_dir("acme", "widget", 1, "d1")
        .await
        .unwrap();
    assert_eq!(prior.as_deref(), Some("/sessions/acme__widget__1"));

    // ...and the current event itself never counts as prior history.
    let none = store
        .prior_session_dir("acme", "widget", 1, "seed")
        .await
        .unwrap();
    assert_eq!(none, None);
}

#[tokio::test]
async fn test_prior_session_dir_ignores_events_without_session() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = Store::new(&db_path).unwrap();

    // Skipped events complete without a session dir.
    store
        .enqueue("skip", "issues", "acme", "widget", 1, "{}")
        .await
        .unwrap();
    store.done("skip", None).await.unwrap();

    let prior = store
        .prior_session_dir("acme", "widget", 1, "other")
        .await
        .unwrap();
    assert_eq!(prior, None);
}

#[tokio::test]
async fn test_audit_tool_call_recorded() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = Store::new(&db_path).unwrap();

    store
        .audit_tool_call("acme", "widget", 1, "/issues/1/comment", "{}", "{}")
        .await
        .unwrap();

    store
        .with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT tool FROM tool_calls").unwrap();
            let mut rows = stmt.query([]).unwrap();
            let row = rows.next().unwrap().unwrap();
            let tool: String = row.get(0).unwrap();
            assert_eq!(tool, "/issues/1/comment");
        })
        .unwrap();
}

#[tokio::test]
async fn test_store_enables_wal_and_busy_timeout() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = Store::new(&db_path).unwrap();

    store
        .with_conn(|conn| {
            let mode: String = conn
                .query_row("PRAGMA journal_mode", [], |r| r.get(0))
                .unwrap();
            assert_eq!(mode.to_lowercase(), "wal");

            let busy: i64 = conn
                .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
                .unwrap();
            assert_eq!(busy, 5000);
        })
        .unwrap();
}

#[tokio::test]
async fn test_store_missing_parent_dir_is_created() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("nested/subdir/test.db");
    Store::new(&db_path).unwrap();

    assert!(db_path.exists());
}

#[tokio::test]
async fn test_autoclose_is_only_due_once() {
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().join("test.db")).unwrap();

    store
        .schedule_autoclose("acme", "widget", 7, "reporter", 1000.0)
        .await
        .unwrap();

    let due = store.due_autocloses(10).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].author_login, "reporter");
    assert_eq!(due[0].number, 7);
    assert_eq!(due[0].closed_at, None);

    store.complete_autoclose("acme", "widget", 7).await.unwrap();
    assert!(store.due_autocloses(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn test_autoclose_reschedule_reopens_the_window() {
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().join("test.db")).unwrap();

    store
        .schedule_autoclose("acme", "widget", 7, "reporter", 1000.0)
        .await
        .unwrap();
    store.complete_autoclose("acme", "widget", 7).await.unwrap();

    // A later answer on the same issue starts a fresh window instead of being
    // closed immediately because the previous one already ran.
    store
        .schedule_autoclose("acme", "widget", 7, "maintainer", 2000.0)
        .await
        .unwrap();

    let due = store.due_autocloses(10).await.unwrap();
    assert_eq!(due.len(), 1, "rescheduling must reopen the window");
    assert_eq!(due[0].author_login, "maintainer");
    assert_eq!(due[0].close_at, 2000.0);
}

#[tokio::test]
async fn test_due_autocloses_orders_by_close_at_and_respects_limit() {
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().join("test.db")).unwrap();

    for (number, close_at) in [(1, 300.0), (2, 100.0), (3, 200.0)] {
        store
            .schedule_autoclose("acme", "widget", number, "reporter", close_at)
            .await
            .unwrap();
    }

    let due = store.due_autocloses(2).await.unwrap();
    let numbers: Vec<i64> = due.iter().map(|entry| entry.number).collect();
    assert_eq!(numbers, vec![2, 3], "oldest window must be handled first");
}
