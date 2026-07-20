// Dictionary codec: replace frequent whitespace-delimited words with short legend codes, so
// repetitive text the structured codecs punt on (process tables, dep trees, mixed logs) still shrinks
// losslessly. Aggressive, made safe by the best-of-N round-trip gate; wire is a legend then the body.

const HEADER: &str = "[dict]\n";
const SEP: &str = "\n--\n";
// Sentinels tried in order; the first not present in the block prefixes every code, so a code can never collide with real content.
const SENTINELS: &[char] = &[
    '\u{a4}', '\u{a7}', '\u{2021}', '\u{b6}', '\u{2317}', '\u{b5}',
];
const MIN_WORD: usize = 5;
const MIN_FREQ: usize = 3;
const MAX_CODES: usize = 400;

// Split into alternating whitespace / non-whitespace runs (concatenation restores the input); each non-whitespace run is a candidate word.
fn runs(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let space = rest.starts_with(|c: char| c.is_whitespace());
        let end = rest
            .find(|c: char| c.is_whitespace() != space)
            .unwrap_or(rest.len());
        out.push(&rest[..end]);
        rest = &rest[end..];
    }
    out
}

pub fn encode(raw: &str) -> Option<String> {
    use std::collections::HashMap;
    let segments = runs(raw);
    let mut freq: HashMap<&str, usize> = HashMap::new();
    for seg in &segments {
        if seg.len() >= MIN_WORD && !seg.starts_with(|c: char| c.is_whitespace()) {
            *freq.entry(*seg).or_default() += 1;
        }
    }
    let mut cands: Vec<(&str, usize)> = freq.into_iter().filter(|(_, f)| *f >= MIN_FREQ).collect();
    if cands.is_empty() {
        return None;
    }
    // Encode the words that save the most first (length times how often they repeat).
    cands.sort_by(|a, b| (b.0.len() * b.1).cmp(&(a.0.len() * a.1)).then(a.0.cmp(b.0)));
    cands.truncate(MAX_CODES);

    let sentinel = *SENTINELS.iter().find(|s| !raw.contains(**s))?;
    let mut codes: HashMap<&str, String> = HashMap::new();
    let mut legend = String::new();
    for (i, (word, _)) in cands.iter().enumerate() {
        let code = format!("{sentinel}{i}");
        legend.push_str(&code);
        legend.push('=');
        legend.push_str(word);
        legend.push('\n');
        codes.insert(*word, code);
    }
    let mut body = String::with_capacity(raw.len());
    for seg in &segments {
        match codes.get(*seg) {
            Some(code) => body.push_str(code),
            None => body.push_str(seg),
        }
    }
    Some(format!("{HEADER}{legend}{SEP}{body}"))
}

pub fn decode(wire: &str) -> Option<String> {
    let rest = wire.strip_prefix(HEADER)?;
    let (legend, body) = rest.split_once(SEP)?;
    // code -> word, longest code first so a shorter code is never a prefix-collision of a longer one.
    let mut pairs: Vec<(&str, &str)> = legend.lines().filter_map(|l| l.split_once('=')).collect();
    if pairs.is_empty() {
        return None;
    }
    pairs.sort_by_key(|b| std::cmp::Reverse(b.0.len()));
    use std::collections::HashMap;
    let map: HashMap<&str, &str> = pairs.into_iter().collect();
    let mut out = String::with_capacity(body.len());
    for seg in runs(body) {
        match map.get(seg) {
            Some(word) => out.push_str(word),
            None => out.push_str(seg),
        }
    }
    Some(out)
}

// The built-in proposer that routes the dictionary codec through the best-of-N search.
pub struct Dict;

impl crate::transform::TextProposer for Dict {
    fn id(&self) -> &'static str {
        "dict"
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
    fn round_trips_a_repetitive_table() {
        let raw: String = (0..80)
            .map(|i| {
                format!("root     {i:>5}  0.0  0.1  running   /usr/local/bin/service --config prod")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let wire = encode(&raw).expect("repetition compresses");
        assert!(
            wire.len() < raw.len(),
            "the dictionary wire must be smaller"
        );
        assert_eq!(
            decode(&wire).as_deref(),
            Some(raw.as_str()),
            "decode must be byte-exact"
        );
    }

    #[test]
    fn round_trips_edge_cases() {
        for raw in [
            "",
            "one two three",
            "\t\tmixed   whitespace\n\n  runs\t",
            "repeat repeat repeat repeat unique",
            "trailing spaces   \n   leading",
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
    fn declines_when_nothing_repeats() {
        assert!(encode("all distinct words here now").is_none());
    }

    #[test]
    fn decode_rejects_a_foreign_wire() {
        assert!(decode("SWTC\tnot a dict wire").is_none());
    }
}
