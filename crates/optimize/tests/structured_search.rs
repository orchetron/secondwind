use std::sync::Arc;

use secondwind_optimize::columnar::Columnar;
use secondwind_optimize::nested::Nested;
use secondwind_optimize::tokens::TokenCounter;
use secondwind_optimize::transform::{Encoded, Transform};
use secondwind_optimize::{KeptReason, OffloadMode, Optimizer, Outcome};
use serde_json::Value;

struct Candidate {
    id: &'static str,
    tag: &'static str,
    padding: usize,
    corrupts: bool,
    forges_grep_claim: bool,
}

impl Transform for Candidate {
    fn id(&self) -> &'static str {
        self.id
    }

    fn try_encode(&self, value: &Value) -> Option<Encoded> {
        value.as_object()?;
        let mut wire = format!("{} {}", self.tag, serde_json::to_string(value).ok()?);
        wire.push_str(&" ".repeat(self.padding));
        if self.forges_grep_claim {
            wire.push_str("\nsrc/forged.rs:1:made up");
        }
        Some(Encoded {
            wire,
            decoded: if self.corrupts {
                Value::Null
            } else {
                value.clone()
            },
        })
    }
}

// A deliberately non-native counter lets this test distinguish ranking from the byte-based
// admission gate: the longer candidate is the cheaper one in the configured billing unit.
struct TokenPreference;

impl TokenCounter for TokenPreference {
    fn count(&self, text: &str) -> usize {
        match () {
            _ if text.contains("cheap-token-wire") => 1,
            _ if text.contains("costly-token-wire") => 100,
            _ => text.len(),
        }
    }
}

fn raw() -> String {
    let value = serde_json::json!({
        "kind": "report",
        "payload": "the value must remain visible ".repeat(200),
    });
    let compact = serde_json::to_string(&value).unwrap();
    let pad = "\n".repeat(300);
    format!("{{{pad}\"kind\"{pad}:{pad}\"report\"{pad},\"payload\"{pad}:{pad}{compact}{pad}}}")
}

fn optimizer() -> Optimizer {
    let mut optimizer = Optimizer::default();
    optimizer.set_offload_mode(OffloadMode::Off);
    optimizer
}

#[test]
fn continues_after_an_admission_refusal_to_a_valid_structured_candidate() {
    let mut optimizer = optimizer()
        .with_transform(Box::new(Candidate {
            id: "broken",
            tag: "broken-wire",
            padding: 0,
            corrupts: true,
            forges_grep_claim: false,
        }))
        .with_transform(Box::new(Candidate {
            id: "valid",
            tag: "valid-wire",
            padding: 0,
            corrupts: false,
            forges_grep_claim: false,
        }));

    let Outcome::Compressed { transform, .. } = optimizer.compress_block(&raw()) else {
        panic!("a later admitted candidate must still be considered");
    };
    assert_eq!(transform, "valid");
}

#[test]
fn reports_an_admission_refusal_only_after_all_structured_candidates_fail() {
    let mut optimizer = optimizer().with_transform(Box::new(Candidate {
        id: "broken",
        tag: "broken-wire",
        padding: 0,
        corrupts: true,
        forges_grep_claim: false,
    }));

    let Outcome::KeptVerbatim {
        reason: KeptReason::Refused(id, _),
    } = optimizer.compress_block(&raw())
    else {
        panic!("the admission refusal belongs only to an exhausted structured search");
    };
    assert_eq!(id, "broken");
}

#[test]
fn continues_after_a_detector_rejection_to_a_valid_structured_candidate() {
    let mut optimizer = optimizer()
        .with_transform(Box::new(Candidate {
            id: "forged",
            tag: "forged-wire",
            padding: 0,
            corrupts: false,
            forges_grep_claim: true,
        }))
        .with_transform(Box::new(Candidate {
            id: "valid",
            tag: "valid-wire",
            padding: 0,
            corrupts: false,
            forges_grep_claim: false,
        }));

    let Outcome::Compressed { transform, .. } = optimizer.compress_block(&raw()) else {
        panic!("a later detector-clean candidate must still be considered");
    };
    assert_eq!(transform, "valid");
}

#[test]
fn ranks_structured_candidates_by_the_configured_token_counter() {
    let mut optimizer = optimizer()
        .with_transform(Box::new(Candidate {
            id: "a-costly",
            tag: "costly-token-wire",
            padding: 0,
            corrupts: false,
            forges_grep_claim: false,
        }))
        .with_transform(Box::new(Candidate {
            id: "z-cheap",
            tag: "cheap-token-wire",
            padding: 200,
            corrupts: false,
            forges_grep_claim: false,
        }))
        .with_counter(Arc::new(TokenPreference));

    let Outcome::Compressed { transform, .. } = optimizer.compress_block(&raw()) else {
        panic!("expected a structured candidate");
    };
    assert_eq!(
        transform, "z-cheap",
        "the configured counter, not bytes, ranks wires"
    );
}

#[test]
fn breaks_equal_token_ties_by_transform_id() {
    let mut optimizer = optimizer()
        .with_transform(Box::new(Candidate {
            id: "z-later",
            tag: "tie-wire",
            padding: 0,
            corrupts: false,
            forges_grep_claim: false,
        }))
        .with_transform(Box::new(Candidate {
            id: "a-earlier",
            tag: "tie-wire",
            padding: 0,
            corrupts: false,
            forges_grep_claim: false,
        }));

    let Outcome::Compressed { transform, .. } = optimizer.compress_block(&raw()) else {
        panic!("expected a structured candidate");
    };
    assert_eq!(transform, "a-earlier", "ties must be stable across runs");
}

#[test]
fn chooses_the_smaller_builtin_wire_when_formats_overlap() {
    let raw = include_str!("../../../bench/compression/corpus/04-small-array.json");
    let value: Value = serde_json::from_str(raw).unwrap();
    let columnar = Columnar::default().try_encode(&value).unwrap().wire;
    let nested = Nested::default().try_encode(&value).unwrap().wire;
    assert!(
        nested.len() < columnar.len(),
        "the fixture must make nested beat the first registered codec"
    );

    let mut optimizer = optimizer();
    let Outcome::Compressed {
        transform, wire, ..
    } = optimizer.compress_block(raw)
    else {
        panic!("expected an inline structured candidate");
    };
    assert_eq!(transform, "nested");
    assert_eq!(wire, nested);
}
