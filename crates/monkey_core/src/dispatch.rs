use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Fix,
    Answer,
    Comment,
    Invalid,
    Skip,
}

impl std::fmt::Display for TaskKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fix => write!(f, "fix"),
            Self::Answer => write!(f, "answer"),
            Self::Comment => write!(f, "comment"),
            Self::Invalid => write!(f, "invalid"),
            Self::Skip => write!(f, "skip"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub kind: TaskKind,
    pub prompt: String,
    pub pr_body: String,
    pub comment: String,
    pub labels: Vec<String>,
    pub autoclose: bool,
}

pub fn classify_and_build_task(event_type: &str, payload: &Value) -> Option<Task> {
    let action = payload.get("action").and_then(|value| value.as_str());
    if !is_supported_event_action(event_type, action) {
        return None;
    }
    let empty_obj = Value::Object(serde_json::Map::new());
    let issue = payload
        .get("issue")
        .or_else(|| payload.get("pull_request"))
        .unwrap_or(&empty_obj);

    // Skip closed issues and pull requests so duplicate or delayed webhook events
    // never trigger unnecessary sandbox checkouts or duplicate triage runs.
    if issue.get("state").and_then(|s| s.as_str()) == Some("closed") {
        return None;
    }

    let title = issue.get("title").and_then(|t| t.as_str()).unwrap_or("");
    let body = issue.get("body").and_then(|b| b.as_str()).unwrap_or("");
    let labels: Vec<String> = issue
        .get("labels")
        .and_then(|l| l.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|val| {
                    val.get("name")
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_string())
                })
                .collect()
        })
        .unwrap_or_default();

    let combined = format!("{}\n{}", title, body).to_lowercase();

    if labels.iter().any(|l| l == "question") || title.contains('?') {
        return Some(task(
            TaskKind::Answer,
            question_prompt(title, body),
            &["question"],
        ));
    }

    if labels.iter().any(|l| l == "invalid") {
        return Some(task(
            TaskKind::Invalid,
            invalid_prompt(title, body),
            &["invalid"],
        ));
    }

    if labels.iter().any(|l| l == "duplicate") {
        return Some(task(
            TaskKind::Invalid,
            duplicate_prompt(title, body),
            &["duplicate"],
        ));
    }

    let is_bug = has_any(
        &combined,
        &[
            "bug",
            "error",
            "crash",
            "fail",
            "broken",
            "exception",
            "regression",
        ],
    );
    let is_doc = has_any(
        &combined,
        &["documentation", "doc", "typo", "readme", "docs"],
    );

    let number = issue.get("number").and_then(|n| n.as_i64());
    if is_bug || is_doc {
        let label = if is_bug { "bug" } else { "documentation" };
        return Some(task(
            TaskKind::Fix,
            fix_prompt(title, body, number),
            &[label],
        ));
    }

    let is_enh = has_any(
        &combined,
        &[
            "feature",
            "enhancement",
            "proposal",
            "suggestion",
            "request",
        ],
    );
    if is_enh {
        return Some(task(
            TaskKind::Comment,
            enhancement_prompt(title, body),
            &["enhancement"],
        ));
    }

    // Fallback: enhancement-ish default (comment only).
    Some(task(
        TaskKind::Comment,
        enhancement_prompt(title, body),
        &[],
    ))
}

fn task(kind: TaskKind, prompt: String, labels: &[&str]) -> Task {
    Task {
        kind,
        prompt,
        pr_body: String::new(),
        comment: String::new(),
        labels: labels.iter().map(|l| l.to_string()).collect(),
        autoclose: false,
    }
}

pub fn fix_prompt(title: &str, body: &str, number: Option<i64>) -> String {
    let fix_target = match number {
        Some(n) => format!("Fixes #{}", n),
        None => "Fixes #<issue-number>".to_string(),
    };
    format!(
        "You are triaging a bug report in this repository. Your job is to \
        actually fix it, not just analyze it. Follow these steps IN ORDER and \
        do NOT stop until you reach step 6.\n\n\
        Title: {}\n\nBody:\n{}\n\n\
        1. REPRODUCE: run the app/tests or write a small script to confirm the bug. Show the actual failure.\n\
        2. FIND THE CAUSE: read the relevant source and pinpoint the exact line/condition responsible.\n\
        3. FIX: EDIT the source file(s) with the edit/write tools. Make a small, targeted change. You MUST change the code - analysis alone is not a fix.\n\
        4. VERIFY: run the tests (or repro) and show they now pass.\n\
        5. COMMIT: stage and commit your changes with a clear message. This is required; a fix without a commit is incomplete.\n\
        6. REPORT: produce a final message with exactly these sections:\n\
        ## Repro\n## Cause\n## Fix\n## Verification\n\
        and end with a line: {}.\n\n\
        IMPORTANT: Do not merely explore and summarize. You must make a code \
        change and commit it. If there is genuinely nothing to fix, say so \
        explicitly in the ## Cause section and do not fabricate a fix.",
        title, body, fix_target
    )
}

pub fn question_prompt(title: &str, body: &str) -> String {
    format!(
        "A user asked a question in this repository. Answer helpfully and \
        concisely, citing the relevant code where possible.\n\
        Question: {}\n\n{}\n\n\
        Produce your answer as a single comment below.",
        title, body
    )
}

pub fn invalid_prompt(title: &str, body: &str) -> String {
    format!(
        "This issue was marked invalid. Briefly (1-2 sentences) explain why \
        and what the reporter should do instead.\nTitle: {}\n\n{}",
        title, body
    )
}

pub fn duplicate_prompt(title: &str, body: &str) -> String {
    format!(
        "This issue was marked duplicate. Briefly point the reporter to the \
        existing (duplicate) thread.\nTitle: {}\n\n{}",
        title, body
    )
}

pub fn enhancement_prompt(title: &str, body: &str) -> String {
    format!(
        "This is a feature/proposal. Acknowledge it and summarize the request, \
        note feasibility or a plan, without making changes.\n\
        Title: {}\n\n{}\n\n\
        Produce your response as a single comment below.",
        title, body
    )
}

fn is_supported_event_action(event_type: &str, action: Option<&str>) -> bool {
    matches!(
        (event_type, action),
        (
            "issues",
            Some("opened" | "edited" | "labeled" | "unlabeled" | "reopened")
        ) | ("issue_comment", Some("created"))
            | ("pull_request_review", Some("submitted"))
    )
}
fn has_any(text: &str, keywords: &[&str]) -> bool {
    keywords.iter().any(|k| text.contains(k))
}
