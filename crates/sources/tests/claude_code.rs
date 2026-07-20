use std::path::PathBuf;

use secondwind_core::{Role, SegmentKind};
use secondwind_sources::claude_code::ClaudeCode;
use secondwind_sources::{ReadError, Source};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn parses_a_session_into_one_trace() {
    let outcome = ClaudeCode.read(&fixture("session.jsonl")).unwrap();

    assert_eq!(outcome.traces.len(), 1);
    let trace = &outcome.traces[0];
    assert_eq!(trace.id, "s-1");
    assert_eq!(trace.source, "claude-code");
    assert_eq!(trace.turns.len(), 4);

    assert_eq!(trace.turns[0].role, Role::User);
    assert_eq!(trace.turns[0].segments[0].effective, "find the retry limit");

    let assistant = &trace.turns[1];
    assert_eq!(assistant.role, Role::Assistant);
    assert_eq!(assistant.model.as_deref(), Some("claude-sonnet-4-5"));
    assert_eq!(assistant.segments.len(), 2);
    assert!(matches!(
        &assistant.segments[1].kind,
        SegmentKind::ToolCall { name, .. } if name == "Grep"
    ));
    let billing = assistant.billing.unwrap();
    assert_eq!(billing.cache_read_tokens, 12000);
    assert_eq!(billing.cache_write_5m_tokens, 100);
    assert_eq!(billing.cache_write_1h_tokens, 200);
    assert_eq!(billing.cache_write_tokens(), 300);

    let result_turn = &trace.turns[2];
    assert!(matches!(
        &result_turn.segments[0].kind,
        SegmentKind::ToolResult { tool, .. } if tool == "Grep"
    ));
    assert_eq!(
        result_turn.segments[0].effective,
        "config/app.rs:88:retry_limit = 3"
    );

    assert_eq!(outcome.skipped_record_types.get("ai-title"), Some(&1));
    assert_eq!(outcome.skipped_record_types.get("attachment"), Some(&1));
}

#[test]
fn usage_repeated_across_records_of_one_request_is_billed_once() {
    let outcome = ClaudeCode.read(&fixture("dedup.jsonl")).unwrap();

    let trace = &outcome.traces[0];
    assert_eq!(trace.turns.len(), 3);
    let billed: Vec<bool> = trace.turns.iter().map(|t| t.billing.is_some()).collect();
    assert_eq!(billed, vec![true, false, true]);
    assert_eq!(trace.turns[0].billing.unwrap().input_tokens, 100);
    assert_eq!(trace.turns[2].billing.unwrap().input_tokens, 200);
}

#[test]
fn known_record_that_stops_parsing_is_a_loud_drift_error() {
    let err = ClaudeCode.read(&fixture("drift.jsonl")).unwrap_err();
    match err {
        ReadError::Drift { line, detail, .. } => {
            assert_eq!(line, 1);
            assert!(detail.contains("assistant"));
        }
        other => panic!("expected drift error, got: {other}"),
    }
}
