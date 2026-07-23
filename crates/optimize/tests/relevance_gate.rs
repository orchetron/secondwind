use secondwind_optimize::{Optimizer, Outcome};
use serde_json::Value;

// Structural relevance gate: no model, no key. Every value the model might look up must appear
// literally in an inline wire, so a fragment/index/transpose reshaping (unreadable to a model) is
// caught in CI. It is a backstop, not a proof; the authoritative check is the accuracy harness.
fn leaf_values(v: &Value, out: &mut Vec<Value>) {
    match v {
        Value::Array(a) => a.iter().for_each(|x| leaf_values(x, out)),
        Value::Object(m) => m.values().for_each(|x| leaf_values(x, out)),
        Value::Null => {}
        other => out.push(other.clone()),
    }
}

fn appears_literal(value: &Value, wire: &str) -> bool {
    match value {
        Value::String(s) => {
            if s.chars().count() < 4 {
                return true; // trivially short: presence carries no signal
            }
            let escaped = serde_json::to_string(s).unwrap_or_default();
            wire.contains(s.as_str()) || wire.contains(escaped.trim_matches('"'))
        }
        Value::Bool(_) | Value::Number(_) => wire.contains(&value.to_string()),
        _ => true,
    }
}

fn assert_inline_literal(raw: &str) {
    let mut opt = Optimizer::default();
    opt.set_offload_allowed(false);
    let Outcome::Compressed {
        wire, transform, ..
    } = opt.compress_block(raw)
    else {
        return; // verbatim/offload paths are not the inline-readability contract
    };
    let value: Value = serde_json::from_str(raw).unwrap();
    let mut vals = Vec::new();
    leaf_values(&value, &mut vals);
    for v in &vals {
        assert!(
            appears_literal(v, &wire),
            "inline codec `{transform}` fragmented value {v}; the model could not read it back.\nwire head:\n{}",
            &wire[..wire.len().min(400)]
        );
    }
}

fn array(n: usize, row: impl Fn(usize) -> String) -> String {
    let rows: Vec<String> = (0..n).map(row).collect();
    format!("[{}]", rows.join(","))
}

#[test]
fn the_gate_bites_a_fragmented_value() {
    // Prove the check is not vacuous: a value split into a stated prefix + a bare suffix (affix
    // style, unreadable) is not literally present, while the whole value is.
    let value = Value::String("https://api.github.com/repos/cli/cli/issues/13938".into());
    let affix_wire = "prefix=https://api.github.com/repos/cli/cli/issues/\n13938";
    assert!(
        !appears_literal(&value, affix_wire),
        "gate must reject a fragmented value"
    );
    let literal_wire = "url\nhttps://api.github.com/repos/cli/cli/issues/13938";
    assert!(
        appears_literal(&value, literal_wire),
        "and must pass a literal value"
    );
}

#[test]
fn flat_record_arrays_keep_values_literal() {
    assert_inline_literal(&array(8, |i| {
        format!(r#"{{"id":{i},"state":"open","name":"service-node-{i:03}"}}"#)
    }));
}

#[test]
fn nested_and_ragged_arrays_keep_values_literal() {
    assert_inline_literal(&array(8, |i| {
        format!(
            r#"{{"number":{i},"user":{{"login":"dev-{i:02}"}},"title":"fix the retry loop {i}"}}"#
        )
    }));
    assert_inline_literal(&array(8, |i| {
        if i % 2 == 0 {
            format!(r#"{{"number":{i},"state":"open"}}"#)
        } else {
            format!(r#"{{"number":{i},"state":"closed","note":"needs review {i}"}}"#)
        }
    }));
}

#[test]
fn a_document_with_a_tablified_map_keeps_values_literal() {
    let entries: Vec<String> = (0..12)
        .map(|i| format!(r#""node_modules/pkg-{i}":{{"version":"1.2.{i}","license":"MIT"}}"#))
        .collect();
    assert_inline_literal(&format!(
        r#"{{"name":"root","lockfileVersion":3,"packages":{{{}}}}}"#,
        entries.join(",")
    ));
}

#[test]
fn constant_and_url_heavy_columns_keep_values_literal() {
    // Constant repo + per-row urls: exactly where hoisting or affix could fragment a value.
    assert_inline_literal(&array(8, |i| {
        format!(
            r#"{{"number":{i},"repository":"cli/cli","url":"https://api.github.com/repos/cli/cli/issues/{i}","html_url":"https://github.com/cli/cli/issues/{i}"}}"#
        )
    }));
}
