use std::path::PathBuf;

use secondwind_analyzers::ViolationClass;
use secondwind_core::Trace;
use secondwind_redact::Redactor;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/receipt-fabrication.json")
}

#[test]
fn published_fixture_reproduces_its_finding() {
    let raw = std::fs::read_to_string(fixture_path()).unwrap();
    let trace: Trace = serde_json::from_str(&raw).unwrap();

    let findings: Vec<_> = secondwind_analyzers::all()
        .iter()
        .flat_map(|a| a.analyze(&trace))
        .collect();

    assert!(findings.iter().any(|f| {
        f.class == ViolationClass::Fabrication
            && f.original.contains("retry_limit = 3")
            && f.effective.contains("retry_limit = 5")
    }));
}

#[test]
fn published_fixture_is_already_clean_under_redaction() {
    let raw = std::fs::read_to_string(fixture_path()).unwrap();
    let mut trace: Trace = serde_json::from_str(&raw).unwrap();
    let report = Redactor::new().redact_trace(&mut trace);
    assert_eq!(report.total(), 0);
}
