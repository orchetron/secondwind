use secondwind_optimize::admit::{Refusal, admit};
use secondwind_optimize::transform::Transform;
use secondwind_optimize::{CorruptingTransform, KeptReason, Optimizer, Outcome};

const UNIFORM: &str = r#"[
  {"id":1,"svc":"alpha","port":7001,"status":"FAIL"},
  {"id":2,"svc":"bravo","port":7002,"status":"ok"},
  {"id":3,"svc":"charlie","port":7003,"status":"ok"}
]"#;

fn big_object() -> String {
    let rows: Vec<String> = (0..200)
        .map(|i| {
            format!(
                r#""svc_{i}":"port {} path /srv/app_{i}/cfg.yaml""#,
                7000 + i
            )
        })
        .collect();
    format!("{{{}}}", rows.join(","))
}

#[test]
fn faithful_columnar_transform_is_admitted_and_smaller() {
    let mut opt = Optimizer::default();
    match opt.compress_block(UNIFORM) {
        Outcome::Compressed {
            wire,
            transform,
            certificate,
            saved_usd,
        } => {
            assert_eq!(transform, "columnar");
            assert!(certificate.wire_bytes < certificate.canonical_bytes);
            assert!(wire.contains("7001") && wire.contains("FAIL"));
            assert_eq!(certificate.clmh_before, certificate.clmh_after);
            assert!(saved_usd > 0.0);
        }
        _ => panic!("expected compression"),
    }
}

#[test]
fn value_corrupting_transform_is_refused() {
    let value: serde_json::Value = serde_json::from_str(UNIFORM).unwrap();
    let encoded = CorruptingTransform.try_encode(&value).unwrap();
    let err = admit(&value, &encoded, |v| CorruptingTransform.try_encode(v)).unwrap_err();
    assert_eq!(err, Refusal::ClmhMismatch);
}

#[test]
fn non_json_is_kept_verbatim() {
    let mut opt = Optimizer::default();
    match opt.compress_block("not json at all") {
        Outcome::KeptVerbatim {
            reason: KeptReason::NotApplicable,
        } => {}
        _ => panic!("expected verbatim"),
    }
}

#[test]
fn a_large_flat_object_offloads_recoverably() {
    let mut opt = Optimizer::default();
    let raw = big_object();
    match opt.compress_block(&raw) {
        Outcome::Offloaded {
            stub,
            marker,
            saved_usd,
        } => {
            assert!(stub.len() < raw.len());
            assert!(saved_usd > 0.0);
            let resolved = opt.resolve(&marker).expect("marker resolves");
            assert_eq!(
                resolved, raw,
                "offload resolves to the exact original bytes"
            );
        }
        _ => panic!("expected offload"),
    }
}

#[test]
fn compressed_wire_survives_our_own_auditor() {
    let original: serde_json::Value = serde_json::from_str(UNIFORM).unwrap();
    let mut opt = Optimizer::default();
    let Outcome::Compressed { wire, .. } = opt.compress_block(UNIFORM) else {
        panic!("expected compression");
    };

    let trace = secondwind_core::Trace {
        id: "opt".into(),
        source: "test".into(),
        optimizer: Some("secondwind".into()),
        provenance: secondwind_core::Provenance {
            origin: secondwind_core::Origin::Synthetic,
            party: secondwind_core::Party::FirstParty,
        },
        turns: vec![
            secondwind_core::Turn {
                index: 0,
                role: secondwind_core::Role::User,
                timestamp: None,
                model: None,
                sidechain: false,
                segments: vec![secondwind_core::Segment {
                    kind: secondwind_core::SegmentKind::ToolResult {
                        tool: "Grep".into(),
                        id: Some("toolu_1".into()),
                    },
                    original: Some(serde_json::to_string(&original).unwrap()),
                    effective: wire,
                }],
                billing: None,
            },
            secondwind_core::Turn {
                index: 1,
                role: secondwind_core::Role::Assistant,
                timestamp: None,
                model: None,
                sidechain: false,
                segments: vec![secondwind_core::Segment {
                    kind: secondwind_core::SegmentKind::Text,
                    original: None,
                    effective: "service alpha on port 7001 is FAIL".into(),
                }],
                billing: None,
            },
        ],
    };

    let findings: Vec<_> = secondwind_analyzers::all()
        .iter()
        .flat_map(|a| a.analyze(&trace))
        .collect();
    assert!(
        findings.is_empty(),
        "optimizer output tripped the auditor: {findings:?}"
    );
}
