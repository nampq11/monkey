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
