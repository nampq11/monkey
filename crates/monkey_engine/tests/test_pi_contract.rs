//! Contract tests against captured real `pi` output.
//!
//! These are the gate on bumping the engine pin in the `Dockerfile`. The unit
//! tests in `adapters/pi_protocol.rs` were written from `docs/rpc.md` and only
//! prove the parser matches that document; these prove it matches the engine.

use futures_util::StreamExt;
use monkey_engine::adapters::pi_protocol::PiEvent;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Cursor;
use tokio_util::codec::{FramedRead, LinesCodec};

const SESSION_FIXTURE: &str = include_str!("fixtures/pi-rpc-session-0.84.4.jsonl");

/// Event types the typed protocol models, and how often the real session
/// emits each.
const MODELLED: [(&str, u32); 3] = [("response", 1), ("agent_end", 1), ("agent_settled", 1)];

/// Event types a real run emits that the protocol deliberately does *not*
/// model. They must land in `PiEvent::Unknown`; treating them as errors would
/// break every live session the moment pi adds an event.
const DELIBERATELY_UNMODELLED: [&str; 7] = [
    "extension_ui_request",
    "agent_start",
    "turn_start",
    "message_start",
    "message_update",
    "message_end",
    "turn_end",
];

fn fixture_lines() -> Vec<String> {
    SESSION_FIXTURE
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Wire `type` tag paired with whether the protocol model claims the line.
fn classify(line: &str) -> (String, bool) {
    let raw: Value = serde_json::from_str(line).expect("fixture line is valid JSON");
    let tag = raw
        .get("type")
        .and_then(Value::as_str)
        .expect("fixture line carries a type tag")
        .to_string();
    let event: PiEvent = serde_json::from_str(line).expect("fixture line parses as an event");
    (tag, !event.is_unknown())
}

#[test]
fn test_every_real_session_line_parses_without_error() {
    let lines = fixture_lines();
    assert_eq!(lines.len(), 20, "fixture changed shape unexpectedly");

    for line in &lines {
        if let Err(error) = serde_json::from_str::<PiEvent>(line) {
            panic!("real pi 0.84.4 line rejected by the protocol model: {error}\n{line}");
        }
    }
}

#[test]
fn test_modelled_event_counts_match_the_captured_session() {
    let mut counts: BTreeMap<(String, bool), u32> = BTreeMap::new();
    for line in fixture_lines() {
        *counts.entry(classify(&line)).or_insert(0) += 1;
    }

    for (tag, expected) in MODELLED {
        let seen = counts
            .get(&(tag.to_string(), true))
            .copied()
            .unwrap_or_default();
        assert_eq!(
            seen, expected,
            "`{tag}` should be modelled {expected} time(s), got {seen}. If pi \
             renamed or reshaped it, bump the engine pin deliberately and \
             re-capture the fixture."
        );
    }

    for tag in DELIBERATELY_UNMODELLED {
        let seen = counts
            .get(&(tag.to_string(), false))
            .copied()
            .unwrap_or_default();
        assert!(
            seen > 0,
            "`{tag}` is expected in the fixture but never appeared; the fixture \
             no longer represents a real run"
        );
    }
}

#[test]
fn test_session_terminates_with_exactly_one_modelled_agent_settled() {
    let settled = fixture_lines()
        .iter()
        .filter_map(|line| serde_json::from_str::<PiEvent>(line).ok())
        .filter(|event| matches!(event, PiEvent::AgentSettled))
        .count();
    assert_eq!(settled, 1, "drain_until_settled relies on exactly one");
}

#[tokio::test]
async fn test_lines_codec_does_not_split_on_unicode_line_separators() {
    // docs/rpc.md forbids generic line readers: U+2028 and U+2029 are legal
    // inside a JSON string, so a reader that treats them as record delimiters
    // silently corrupts payloads. Node's readline is called out by name for
    // exactly this bug.
    let payload = "first\u{2028}second\u{2029}third";
    let encoded = serde_json::to_string(&payload).expect("a string always serialises");
    let bytes = format!("{encoded}\n").into_bytes();

    let mut framed = FramedRead::new(Cursor::new(bytes), LinesCodec::new());
    let frames: Vec<String> = framed
        .by_ref()
        .map(|frame| frame.expect("framing must not error"))
        .collect()
        .await;

    assert_eq!(
        frames.len(),
        1,
        "LinesCodec split a JSON string on a Unicode separator"
    );
    let decoded: String = serde_json::from_str(&frames[0]).expect("the frame is one whole record");
    assert_eq!(decoded, payload);
}

#[tokio::test]
async fn test_crlf_framing_yields_one_event_per_record() {
    // pi promises LF only, but a proxy-buffered stream can hand us CRLF. The
    // adapter trims each line, so what has to survive is the record count and
    // the payload.
    let stream = concat!(
        "{\"type\":\"agent_end\",\"willRetry\":false}\r\n",
        "{\"type\":\"agent_settled\"}\r\n",
    )
    .as_bytes()
    .to_vec();

    let mut framed = FramedRead::new(Cursor::new(stream), LinesCodec::new());
    let mut events = Vec::new();
    while let Some(frame) = framed.next().await {
        let frame = frame.expect("framing must not error");
        events.push(
            serde_json::from_str::<PiEvent>(frame.trim())
                .expect("CRLF must not corrupt the payload"),
        );
    }

    assert_eq!(events.len(), 2);
    assert!(matches!(events[0], PiEvent::AgentEnd { .. }));
    assert!(matches!(events[1], PiEvent::AgentSettled));
}
