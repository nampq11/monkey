"""Tests for dispatch classification"""

from monkey.dispatch import classify_and_build_task


class _Settings:
    pass


def _payload(title: str, body: str, labels: list[str] | None = None) -> dict:
    return {
        "issue": {
            "title": title,
            "body": body,
            "labels": [{"name": l} for l in (labels or [])],
        }
    }


def test_bug_with_label_goes_to_fix():
    settings = _Settings()
    task = classify_and_build_task("issues", _payload("App crashes", "boost crash error"), settings)
    assert task is not None
    assert task.kind == "fix"
    assert "bug" in task.labels


def test_question_marked_label_answers():
    settings = _Settings()
    task = classify_and_build_task("issues", _payload("How do I X?", "something", ["question"]), settings)
    assert task.kind == "answer"


def test_documentation_goes_to_fix():
    settings = _Settings()
    task = classify_and_build_task("issues", _payload("Fix typo", "docs/readme has typo"), settings)
    assert task is not None
    assert task.kind == "fix"
    assert "documentation" in task.labels


def test_invalid_goes_to_invalid():
    settings = _Settings()
    task = classify_and_build_task("issues", _payload("Not a real bug", "what is this", ["invalid"]), settings)
    assert task.kind == "invalid"


def test_enhancement_goes_to_comment():
    settings = _Settings()
    task = classify_and_build_task("issues", _payload("Add dark mode", "please feature"), settings)
    assert task.kind == "comment"


def test_fix_prompt_contains_required_sections():
    settings = _Settings()
    task = classify_and_build_task("issues", _payload("Bug on mobile", "buttons overlap"), settings)
    prompt = task.prompt
    for section in ("## Repro", "## Cause", "## Fix", "## Verification"):
        assert section in prompt


def test_closed_action_is_skipped():
    settings = _Settings()
    payload = _payload("App crashes", "boost crash error")
    payload["action"] = "closed"
    task = classify_and_build_task("issues", payload, settings)
    assert task is None
