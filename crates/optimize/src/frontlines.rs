// Front-coded line list: each line stores the count of leading chars shared with the previous line plus
// the suffix, so a sorted path listing (find, ls) folds its shared directory prefixes. Complements the
// whole-line dictionary, which only catches identical lines.

use crate::text_columnar::{byte_offset, shared_prefix_chars};

const HEADER: &str = "[fl]\n";
const MIN_LINES: usize = 4;

pub fn encode(raw: &str) -> Option<String> {
    let lines: Vec<&str> = raw.split('\n').collect();
    if lines.len() < MIN_LINES {
        return None;
    }
    let mut prev = "";
    let mut body: Vec<String> = Vec::with_capacity(lines.len());
    for line in &lines {
        let shared = shared_prefix_chars(prev, line);
        body.push(format!("{shared} {}", &line[byte_offset(line, shared)..]));
        prev = line;
    }
    Some(format!("{HEADER}{}", body.join("\n")))
}

pub fn decode(wire: &str) -> Option<String> {
    let body = wire.strip_prefix(HEADER)?;
    let mut out: Vec<String> = Vec::new();
    let mut prev = String::new();
    for entry in body.split('\n') {
        let (shared, suffix) = entry.split_once(' ')?;
        let shared: usize = shared.parse().ok()?;
        let value: String = prev.chars().take(shared).chain(suffix.chars()).collect();
        out.push(value.clone());
        prev = value;
    }
    Some(out.join("\n"))
}

pub struct Frontlines;

impl crate::transform::TextProposer for Frontlines {
    fn id(&self) -> &'static str {
        "frontlines"
    }
    fn encode(&self, raw: &str) -> Option<String> {
        encode(raw)
    }
    fn decode(&self, wire: &str) -> Option<String> {
        decode(wire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_a_sorted_path_listing() {
        let raw = "./crates/analyzers/src/lib.rs\n./crates/analyzers/src/extract.rs\n./crates/analyzers/tests/detectors.rs\n./crates/ledger/src/events.rs";
        let wire = encode(raw).expect("shared prefixes fold");
        assert!(
            wire.len() < raw.len(),
            "wire {} !< raw {}",
            wire.len(),
            raw.len()
        );
        assert_eq!(decode(&wire).as_deref(), Some(raw));
    }

    #[test]
    fn round_trips_edge_cases() {
        for raw in [
            "a\nb\nc\nd",
            "same\nsame\nsame\nsame",
            "\n\n\n\n",
            "  leading spaces\n  leading spaces two\n  x\n  y",
            "caf\u{00e9}/1\ncaf\u{00e9}/2\ncaf\u{00e9}/33\ncaf\u{00e9}/4",
        ] {
            if let Some(wire) = encode(raw) {
                assert_eq!(
                    decode(&wire).as_deref(),
                    Some(raw),
                    "roundtrip failed for {raw:?}"
                );
            }
        }
    }

    #[test]
    fn decode_rejects_a_foreign_wire() {
        assert!(decode("[dict]\nnope").is_none());
    }
}
