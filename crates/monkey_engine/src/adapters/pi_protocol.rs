//! Typed models for pi's JSON-lines RPC protocol.
//!
//! Before this module every read of pi's stdout was `Value` string-poking
//! (`val.get("type").and_then(Value::as_str) == Some("agent_settled")`). That
//! degrades a renamed field into a *timeout*, because an event whose tag no
//! longer matches is simply never recognised and the drain loop keeps waiting.
//! Parsing into `PiEvent` instead means a tag change is visible at the moment
//! it happens rather than after the run deadline.
//!
//! The contract is defined by pi, not by monkey, so these models mirror
//! `docs/rpc.md` of the pinned engine version and are checked against a real
//! captured transcript in `tests/test_pi_contract.rs`. Bumping the engine pin
//! in the Dockerfile is expected to turn that test red.

use serde::Deserialize;
use serde_json::Value;

/// One JSON line received from pi's stdout.
///
/// The two `rename_all` attributes are doing different jobs and both are
/// required: on the enum it renames *variant tags* (`AgentSettled` matches the
/// wire value `agent_settled`), while on a struct variant it renames that
/// variant's *field names* (`will_retry` matches `willRetry`). pi uses
/// snake_case event tags but camelCase field names, so omitting either one
/// silently yields `None`/`false` for every camelCase field rather than an
/// error.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PiEvent {
    /// The session-level run is finished: no retry, compaction retry, or
    /// queued follow-up remains. This is the only correct stop signal;
    /// `agent_end` fires earlier and can still be followed by more work.
    AgentSettled,

    /// One low-level agent run completed. `will_retry` distinguishes a pause
    /// before an automatic retry from a genuine end of run.
    #[serde(rename_all = "camelCase")]
    AgentEnd {
        #[serde(default)]
        will_retry: bool,
    },

    /// Terminal outcome of pi's automatic retry loop after a transient
    /// provider error. `success: false` with a `final_error` means the run is
    /// over and failed, even though the process still settles normally.
    #[serde(rename_all = "camelCase")]
    AutoRetryEnd {
        success: bool,
        #[serde(default)]
        final_error: Option<String>,
    },

    /// Reply to a command we sent, correlated by `id`.
    Response(PiResponse),

    /// Anything pi may emit that this version does not model.
    ///
    /// `#[serde(other)]` is the forward-compatibility mechanism: pi streams
    /// roughly two dozen event types during a normal run and a future version
    /// can add more. An unmodelled event must be collected and ignored, never
    /// a parse error, otherwise upgrading the engine breaks triage at runtime.
    #[serde(other)]
    Unknown,
}

/// The `{"type":"response", ...}` envelope.
///
/// `id` is optional because the protocol says so, not as a laxness
/// compromise: pi only echoes `id` when the request carried one it could read.
/// Both documented failure envelopes omit it, including the `command: "parse"`
/// case where the engine could not extract our id at all. Callers matching a
/// response to a pending request must therefore treat `None` as "could be
/// mine", or a rejected command turns into a timeout.
///
/// `success`, by contrast, is deliberately *not* defaulted. Every one of the
/// 40 response examples in the pinned engine's docs carries it, so a response
/// without it means the contract has genuinely changed and we want a hard
/// parse error rather than a silent `false`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiResponse {
    #[serde(default)]
    pub id: Option<String>,
    pub success: bool,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    /// Command-specific; interpreted by the caller that issued the request.
    #[serde(default)]
    pub data: Value,
}

impl PiEvent {
    /// The wire `type` tag of an unmodelled event, for logging drift.
    ///
    /// `#[serde(other)]` discards the payload, so callers that need the tag of
    /// an `Unknown` event must read it back out of the raw `Value` they kept
    /// alongside this event.
    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Result<PiEvent, serde_json::Error> {
        serde_json::from_str(line)
    }

    #[test]
    fn test_agent_settled_parses_to_its_own_variant() {
        assert!(matches!(
            parse(r#"{"type":"agent_settled"}"#).unwrap(),
            PiEvent::AgentSettled
        ));
    }

    #[test]
    fn test_unmodelled_event_degrades_to_unknown_not_error() {
        // The whole point of `#[serde(other)]`: a future pi release adding an
        // event type must not break an in-flight triage run.
        let event = parse(r#"{"type":"some_future_pi_event","whatever":1}"#).unwrap();
        assert!(event.is_unknown());
    }

    #[test]
    fn test_camel_case_fields_bind_to_snake_case_rust_names() {
        // Guards the two-level `rename_all` requirement. Without the
        // variant-level attribute these silently become `false` / `None`.
        let event = parse(r#"{"type":"agent_end","messages":[],"willRetry":true}"#).unwrap();
        match event {
            PiEvent::AgentEnd { will_retry } => assert!(will_retry),
            other => panic!("expected AgentEnd, got {other:?}"),
        }

        let event = parse(
            r#"{"type":"auto_retry_end","success":false,"attempt":3,"finalError":"529 overloaded"}"#,
        )
        .unwrap();
        match event {
            PiEvent::AutoRetryEnd {
                success,
                final_error,
            } => {
                assert!(!success);
                assert_eq!(final_error.as_deref(), Some("529 overloaded"));
            }
            other => panic!("expected AutoRetryEnd, got {other:?}"),
        }
    }

    #[test]
    fn test_response_envelope_keeps_success_command_and_data() {
        let event = parse(
            r#"{"type":"response","command":"get_session_stats","success":true,"id":"stats-1","data":{"sessionFile":"/x.jsonl"}}"#,
        )
        .unwrap();
        match event {
            PiEvent::Response(response) => {
                assert_eq!(response.id.as_deref(), Some("stats-1"));
                assert!(response.success);
                assert_eq!(response.command.as_deref(), Some("get_session_stats"));
                assert_eq!(response.data["sessionFile"], "/x.jsonl");
            }
            other => panic!("expected Response, got {other:?}"),
        }
    }

    #[test]
    fn test_rejected_response_keeps_the_engine_reason() {
        let event = parse(
            r#"{"type":"response","command":"set_model","success":false,"error":"Model not found: invalid/model"}"#,
        )
        .unwrap();
        match event {
            PiEvent::Response(response) => {
                assert!(!response.success);
                assert_eq!(
                    response.error.as_deref(),
                    Some("Model not found: invalid/model")
                );
            }
            other => panic!("expected Response, got {other:?}"),
        }
    }

    #[test]
    fn test_rejected_response_without_id_still_parses() {
        // pi cannot echo an id it could not parse out of the request, so a
        // rejection may arrive with no id at all. Rejecting that shape would
        // hide the engine's error behind a correlation timeout.
        let event = parse(
            r#"{"type":"response","command":"parse","success":false,"error":"Failed to parse command: Unexpected token..."}"#,
        )
        .unwrap();
        match event {
            PiEvent::Response(response) => {
                assert_eq!(response.id, None);
                assert!(!response.success);
                assert_eq!(response.command.as_deref(), Some("parse"));
            }
            other => panic!("expected Response, got {other:?}"),
        }
    }

    #[test]
    fn test_response_without_success_is_a_hard_error() {
        // Soft on extensions, hard on breakage: `success` is on every
        // documented response, so its absence means the envelope changed.
        let error = parse(r#"{"type":"response","id":"stats-1","data":{}}"#).unwrap_err();
        assert!(
            error.to_string().contains("missing field"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_non_object_line_is_a_hard_error() {
        assert!(parse("not-json").is_err());
        assert!(parse(r#"{"no_type_field":1}"#).is_err());
    }
}
