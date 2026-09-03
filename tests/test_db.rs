use monkey::db::Store;
use tempfile::tempdir;

#[test]
fn test_enqueue_new_event_inserts() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = Store::new(&db_path).unwrap();

    let is_new = store
        .enqueue("d1", "issues", "acme", "widget", 1, "{}")
        .unwrap();
    assert!(is_new);
}

#[test]
fn test_enqueue_dedup_returns_false() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = Store::new(&db_path).unwrap();

    let is_new1 = store
        .enqueue("d1", "issues", "acme", "widget", 1, "{}")
        .unwrap();
    assert!(is_new1);

    let is_new2 = store
        .enqueue("d1", "issues", "acme", "widget", 1, "{}")
        .unwrap();
    assert!(!is_new2);

    let pending = store.get_pending(10).unwrap();
    assert_eq!(pending.len(), 1);
}

#[test]
fn test_claim_and_finish_flow() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = Store::new(&db_path).unwrap();

    store
        .enqueue("d1", "issues", "acme", "widget", 1, "{}")
        .unwrap();

    assert!(store.claim("d1").unwrap());
    // second claim fails (already running)
    assert!(!store.claim("d1").unwrap());

    store.done("d1", Some("/data/sessions/x")).unwrap();
    let pending = store.get_pending(10).unwrap();
    assert_eq!(pending.len(), 0);
}

#[test]
fn test_audit_tool_call_recorded() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = Store::new(&db_path).unwrap();

    store
        .audit_tool_call("acme", "widget", 1, "/issues/1/comment", "{}", "{}")
        .unwrap();

    store.with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT tool FROM tool_calls").unwrap();
        let mut rows = stmt.query([]).unwrap();
        let row = rows.next().unwrap().unwrap();
        let tool: String = row.get(0).unwrap();
        assert_eq!(tool, "/issues/1/comment");
    });
}

#[test]
fn test_store_enables_wal_and_busy_timeout() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = Store::new(&db_path).unwrap();

    store.with_conn(|conn| {
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");

        let busy: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert_eq!(busy, 5000);
    });
}
