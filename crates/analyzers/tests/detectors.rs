use secondwind_analyzers::{Retention, ViolationClass, all};
use secondwind_core::{Origin, Party, Provenance, Role, Segment, SegmentKind, Trace, Turn};

fn tool_result(original: &str, effective: &str) -> Segment {
    Segment {
        kind: SegmentKind::ToolResult {
            tool: "Grep".into(),
            id: None,
        },
        original: Some(original.into()),
        effective: effective.into(),
    }
}

fn plain(kind: SegmentKind, effective: &str) -> Segment {
    Segment {
        kind,
        original: None,
        effective: effective.into(),
    }
}

fn trace(turns: Vec<Vec<Segment>>) -> Trace {
    Trace {
        id: "t".into(),
        source: "test".into(),
        optimizer: None,
        provenance: Provenance {
            origin: Origin::Synthetic,
            party: Party::FirstParty,
        },
        turns: turns
            .into_iter()
            .enumerate()
            .map(|(index, segments)| Turn {
                index,
                role: Role::User,
                timestamp: None,
                model: None,
                sidechain: false,
                segments,
                billing: None,
            })
            .collect(),
    }
}

fn findings_of(trace: &Trace) -> Vec<(ViolationClass, String)> {
    all()
        .iter()
        .flat_map(|a| a.analyze(trace))
        .map(|f| (f.class, f.detail))
        .collect()
}

#[test]
fn fabricated_grep_content_is_flagged() {
    let t = trace(vec![vec![tool_result(
        "src/a.rs:42:let x = 1;\nsrc/a.rs:50:fn main() {",
        "src/a.rs:42:let x = 2;\nsrc/b.rs:7:let y = 3;",
    )]]);
    let found = findings_of(&t);
    assert!(
        found
            .iter()
            .any(|(c, d)| *c == ViolationClass::Fabrication && d.contains("differs"))
    );
    assert!(
        found
            .iter()
            .any(|(c, d)| *c == ViolationClass::Fabrication && d.contains("absent"))
    );
}

#[test]
fn faithful_compression_produces_no_findings() {
    let t = trace(vec![vec![tool_result(
        "src/a.rs:42:let x = 1;\nsrc/a.rs:50:fn main() {\nnoise line",
        "src/a.rs:42:let x = 1;",
    )]]);
    assert!(findings_of(&t).is_empty());
}

#[test]
fn altered_keyed_number_is_a_violation_without_downstream_reference() {
    let t = trace(vec![vec![tool_result(
        "config: retry_limit = 3",
        "config: retry_limit = 5",
    )]]);
    let found = findings_of(&t);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, ViolationClass::NumericDrift);
}

#[test]
fn dropped_number_is_gated_on_downstream_reference() {
    let ungated = trace(vec![vec![tool_result(
        "listening on port 8443",
        "listening",
    )]]);
    assert!(findings_of(&ungated).is_empty());

    let gated = trace(vec![
        vec![tool_result("listening on port 8443", "listening")],
        vec![plain(
            SegmentKind::ToolCall {
                name: "Bash".into(),
                id: None,
            },
            "curl https://localhost:8443/health",
        )],
    ]);
    let found = findings_of(&gated);
    assert!(
        found
            .iter()
            .any(|(c, d)| *c == ViolationClass::NumericDrift && d.contains("8443"))
    );
}

#[test]
fn dropped_path_is_gated_on_downstream_reference() {
    let gated = trace(vec![
        vec![tool_result(
            "error in /etc/app/config.yaml near line 9",
            "an error occurred near line 9",
        )],
        vec![plain(
            SegmentKind::ToolCall {
                name: "Read".into(),
                id: None,
            },
            "{\"file_path\": \"/etc/app/config.yaml\"}",
        )],
    ]);
    let found = findings_of(&gated);
    assert!(
        found
            .iter()
            .any(|(c, d)| *c == ViolationClass::ArtifactLoss && d.contains("config.yaml"))
    );

    let ungated = trace(vec![vec![tool_result(
        "error in /etc/app/config.yaml near line 9",
        "an error occurred near line 9",
    )]]);
    assert!(
        findings_of(&ungated)
            .iter()
            .all(|(c, _)| *c != ViolationClass::ArtifactLoss)
    );
}

#[test]
fn grep_lines_from_different_files_are_not_numeric_drift() {
    let t = trace(vec![vec![tool_result(
        "src/a.rs:10:alpha\nsrc/c.rs:99:gamma",
        "src/a.rs:10:alpha",
    )]]);
    assert!(
        findings_of(&t)
            .iter()
            .all(|(c, _)| *c != ViolationClass::NumericDrift)
    );
}

#[test]
fn retention_counts_bare_drops_without_calling_them_violations() {
    let t = trace(vec![vec![tool_result(
        "port 8443 and path src/lib.rs and count 77",
        "path src/lib.rs",
    )]]);
    let mut retention = Retention::default();
    retention.add_trace(&t);

    assert_eq!(retention.numerics_total, 2);
    assert_eq!(retention.numerics_kept, 0);
    assert_eq!(retention.artifacts_total, 1);
    assert_eq!(retention.artifacts_kept, 1);
    assert!(findings_of(&t).is_empty());
}
