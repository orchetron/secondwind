// Line dictionary codec: replace frequent whole lines (indentation and all) with short legend codes.
// Factors the dominant redundancy in structured config (k8s manifests, CI, compose, TOML: identical
// lines across near-identical stanzas). Made safe by the best-of-N round-trip gate; wire = legend then body.

const HEADER: &str = "[lines]\n";
const SEP: &str = "\n==\n";
const SENTINELS: &[char] = &[
    '\u{a4}', '\u{a7}', '\u{2021}', '\u{b6}', '\u{2317}', '\u{b5}',
];
const MIN_LINE: usize = 4;
const MIN_FREQ: usize = 3;
const MAX_CODES: usize = 400;

pub fn encode(raw: &str) -> Option<String> {
    use std::collections::HashMap;
    // split('\n')/join('\n') round-trips any input exactly, so operating per line is lossless.
    let lines: Vec<&str> = raw.split('\n').collect();
    let mut freq: HashMap<&str, usize> = HashMap::new();
    for line in &lines {
        if line.len() >= MIN_LINE {
            *freq.entry(*line).or_default() += 1;
        }
    }
    let mut cands: Vec<(&str, usize)> = freq.into_iter().filter(|(_, f)| *f >= MIN_FREQ).collect();
    if cands.is_empty() {
        return None;
    }
    cands.sort_by(|a, b| (b.0.len() * b.1).cmp(&(a.0.len() * a.1)).then(a.0.cmp(b.0)));
    cands.truncate(MAX_CODES);

    let sentinel = *SENTINELS.iter().find(|s| !raw.contains(**s))?;
    let mut codes: HashMap<&str, String> = HashMap::new();
    let mut legend = String::new();
    for (i, (line, _)) in cands.iter().enumerate() {
        let code = format!("{sentinel}{i}");
        legend.push_str(&code);
        legend.push('=');
        legend.push_str(line);
        legend.push('\n');
        codes.insert(*line, code);
    }
    let body = lines
        .iter()
        .map(|line| codes.get(*line).map(String::as_str).unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");
    Some(format!("{HEADER}{legend}{SEP}{body}"))
}

pub fn decode(wire: &str) -> Option<String> {
    let rest = wire.strip_prefix(HEADER)?;
    let (legend, body) = rest.split_once(SEP)?;
    let mut pairs: Vec<(&str, &str)> = legend.lines().filter_map(|l| l.split_once('=')).collect();
    if pairs.is_empty() {
        return None;
    }
    pairs.sort_by_key(|b| std::cmp::Reverse(b.0.len())); // longest code first, so §10 before §1
    use std::collections::HashMap;
    let map: HashMap<&str, &str> = pairs.into_iter().collect();
    Some(
        body.split('\n')
            .map(|line| map.get(line).copied().unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

pub struct Lines;

impl crate::transform::TextProposer for Lines {
    fn id(&self) -> &'static str {
        "lines"
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
    fn round_trips_repeated_stanzas() {
        let stanza = "    resources:\n      requests:\n        cpu: 500m\n        memory: 512Mi";
        let raw = (0..8)
            .map(|i| format!("  - name: svc-{i}\n{stanza}"))
            .collect::<Vec<_>>()
            .join("\n");
        let wire = encode(&raw).expect("repeated lines compress");
        assert!(
            wire.len() < raw.len(),
            "the line-dictionary wire is smaller"
        );
        assert_eq!(
            decode(&wire).as_deref(),
            Some(raw.as_str()),
            "decode is byte-exact"
        );
    }

    #[test]
    fn round_trips_edge_cases() {
        for raw in [
            "",
            "one\ntwo",
            "a\n\n\nb",
            "trailing\n",
            "\nleading",
            "same\nsame\nsame\nsame",
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
