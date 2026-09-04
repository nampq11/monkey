use monkey_app::dispatch::{TaskKind, classify_and_build_task};
use serde_json::json;

fn make_payload(title: &str, body: &str, labels: Option<Vec<&str>>) -> serde_json::Value {
    let label_objs: Vec<_> = labels
        .unwrap_or_default()
        .into_iter()
        .map(|l| json!({ "name": l }))
        .collect();

    json!({
        "action": "opened",
        "issue": {
            "title": title,
            "body": body,
            "labels": label_objs
        }
    })
}

#[test]
fn test_bug_with_label_goes_to_fix() {
    let payload = make_payload("App crashes", "boost crash error", None);
    let task = classify_and_build_task("issues", &payload).unwrap();
    assert_eq!(task.kind, TaskKind::Fix);
    assert!(task.labels.contains(&"bug".to_string()));
}

#[test]
fn test_question_marked_label_answers() {
    let payload = make_payload("How do I X?", "something", Some(vec!["question"]));
    let task = classify_and_build_task("issues", &payload).unwrap();
    assert_eq!(task.kind, TaskKind::Answer);
}

#[test]
fn test_documentation_goes_to_fix() {
    let payload = make_payload("Fix typo", "docs/readme has typo", None);
    let task = classify_and_build_task("issues", &payload).unwrap();
    assert_eq!(task.kind, TaskKind::Fix);
    assert!(task.labels.contains(&"documentation".to_string()));
}

#[test]
fn test_invalid_goes_to_invalid() {
    let payload = make_payload("Not a real bug", "what is this", Some(vec!["invalid"]));
    let task = classify_and_build_task("issues", &payload).unwrap();
    assert_eq!(task.kind, TaskKind::Invalid);
}

#[test]
fn test_enhancement_goes_to_comment() {
    let payload = make_payload("Add dark mode", "please feature", None);
    let task = classify_and_build_task("issues", &payload).unwrap();
    assert_eq!(task.kind, TaskKind::Comment);
}

#[test]
fn test_fix_prompt_contains_required_sections() {
    let payload = make_payload("Bug on mobile", "buttons overlap", None);
    let task = classify_and_build_task("issues", &payload).unwrap();
    let prompt = task.prompt;
    for section in ["## Repro", "## Cause", "## Fix", "## Verification"] {
        assert!(prompt.contains(section));
    }
}

#[test]
fn test_pull_request_event_is_skipped() {
    let payload = make_payload("Bug in pull request", "crash", None);
    assert!(classify_and_build_task("pull_request", &payload).is_none());
}

#[test]
fn test_unsupported_action_is_skipped() {
    let mut payload = make_payload("App crashes", "bug", None);
    payload["action"] = json!("closed");
    assert!(classify_and_build_task("issues", &payload).is_none());
}

#[test]
fn test_missing_action_is_skipped() {
    let payload = json!({"issue": {"title": "App crashes", "body": "bug"}});
    assert!(classify_and_build_task("issues", &payload).is_none());
}

#[test]
fn test_closed_issue_is_skipped() {
    let mut payload = make_payload("App crashes", "crash error", None);
    payload["issue"]["state"] = json!("closed");
    assert!(classify_and_build_task("issues", &payload).is_none());
}

#[test]
fn test_issue_comment_body_is_included_in_prompt() {
    let payload = json!({
        "action": "created",
        "issue": {
            "title": "App crashes on save",
            "body": "it dies when I hit save",
            "state": "open",
            "labels": [{"name": "bug"}]
        },
        "comment": {"body": "FOLLOW_UP_MARKER it only happens for files over 1GB"}
    });
    let task = classify_and_build_task("issue_comment", &payload).unwrap();
    assert!(task.prompt.contains("FOLLOW_UP_MARKER"));
    // The original report stays available for context.
    assert!(task.prompt.contains("it dies when I hit save"));
}

#[test]
fn test_pull_request_review_body_is_included_in_prompt() {
    let payload = json!({
        "action": "submitted",
        "pull_request": {
            "title": "App crashes on save",
            "body": "it dies when I hit save",
            "state": "open",
            "labels": []
        },
        "review": {"body": "REVIEW_MARKER the crash is in save_dialog()"}
    });
    let task = classify_and_build_task("pull_request_review", &payload).unwrap();
    assert!(task.prompt.contains("REVIEW_MARKER"));
}

#[test]
fn test_only_questions_are_scheduled_for_autoclose() {
    let question = make_payload("How do I X?", "something", Some(vec!["question"]));
    assert!(
        classify_and_build_task("issues", &question)
            .unwrap()
            .autoclose
    );

    // A title question mark is the other question signal.
    let titled = make_payload("How does X work?", "context", None);
    assert!(
        classify_and_build_task("issues", &titled)
            .unwrap()
            .autoclose
    );

    let bug = make_payload("App crashes", "boost crash error", None);
    assert!(!classify_and_build_task("issues", &bug).unwrap().autoclose);

    let enhancement = make_payload("Add dark mode", "please feature", None);
    assert!(
        !classify_and_build_task("issues", &enhancement)
            .unwrap()
            .autoclose
    );
}

#[test]
fn test_followup_without_text_adds_nothing_to_the_prompt() {
    // A review that only leaves line comments carries no top-level body.
    let mut payload = make_payload("App crashes", "crash error", None);
    payload["review"] = json!({ "body": null });

    let original = classify_and_build_task("issues", &payload).unwrap();
    payload["action"] = json!("submitted");
    let resumed = classify_and_build_task("pull_request_review", &payload).unwrap();
    assert_eq!(original.prompt, resumed.prompt);
}
