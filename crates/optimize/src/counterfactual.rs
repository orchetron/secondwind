use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;

use secondwind_core::{Role, SegmentKind, Trace};

// Reopen rate from real traces: a block counts as reopened when the assistant later
// emits a token the block introduced (first appearance anywhere) that the preview hid.
// Requiring the block to introduce the token screens out identifiers that recur by chance.

#[derive(Default, Debug, Clone, Copy)]
pub struct ShapeStat {
    pub offloads: u64,
    pub reopens: u64,
}

impl ShapeStat {
    pub fn rate(&self) -> f64 {
        if self.offloads == 0 {
            0.0
        } else {
            self.reopens as f64 / self.offloads as f64
        }
    }
}

#[derive(Default, Debug)]
pub struct ReopenStats {
    pub by_shape: BTreeMap<String, ShapeStat>,
}

impl ReopenStats {
    pub fn merge(&mut self, other: &ReopenStats) {
        for (shape, stat) in &other.by_shape {
            let entry = self.by_shape.entry(shape.clone()).or_default();
            entry.offloads += stat.offloads;
            entry.reopens += stat.reopens;
        }
    }
}

const MIN_TOKEN: usize = 6;
const MAX_RESIDUAL_TOKENS: usize = 400;

pub fn measure(trace: &Trace) -> ReopenStats {
    let first_seen = first_seen_index(trace);
    let assistant_tokens = assistant_token_index(trace);
    let mut stats = ReopenStats::default();

    for (ti, turn) in trace.turns.iter().enumerate() {
        for segment in &turn.segments {
            if !matches!(segment.kind, SegmentKind::ToolResult { .. }) {
                continue;
            }
            let body = &segment.effective;
            let Some(preview) = crate::offload::preview_if_offloaded(body) else {
                continue;
            };
            let residual = residual_tokens(body, &preview);
            if residual.is_empty() {
                continue;
            }
            let reopened = residual.iter().any(|tok| {
                first_seen.get(tok) == Some(&ti) && references_after(&assistant_tokens, tok, ti)
            });
            let stat = stats.by_shape.entry(classify(body)).or_default();
            stat.offloads += 1;
            if reopened {
                stat.reopens += 1;
            }
        }
    }
    stats
}

fn first_seen_index(trace: &Trace) -> HashMap<String, usize> {
    let mut first: HashMap<String, usize> = HashMap::new();
    for (ti, turn) in trace.turns.iter().enumerate() {
        for segment in &turn.segments {
            for tok in significant(&segment.effective) {
                first.entry(tok.to_string()).or_insert(ti);
            }
        }
    }
    first
}

fn residual_tokens(body: &str, preview: &str) -> HashSet<String> {
    let shown: HashSet<&str> = significant(preview).collect();
    let mut residual = HashSet::new();
    for tok in significant(body) {
        if !shown.contains(tok) {
            residual.insert(tok.to_string());
            if residual.len() >= MAX_RESIDUAL_TOKENS {
                break;
            }
        }
    }
    residual
}

fn significant(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| c.is_whitespace())
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| w.len() >= MIN_TOKEN)
}

fn assistant_token_index(trace: &Trace) -> HashMap<String, Vec<usize>> {
    let mut index: HashMap<String, Vec<usize>> = HashMap::new();
    for (ti, turn) in trace.turns.iter().enumerate() {
        if turn.role != Role::Assistant {
            continue;
        }
        let mut seen = HashSet::new();
        for segment in &turn.segments {
            for tok in significant(&segment.effective) {
                if seen.insert(tok) {
                    index.entry(tok.to_string()).or_default().push(ti);
                }
            }
        }
    }
    index
}

fn references_after(index: &HashMap<String, Vec<usize>>, token: &str, after: usize) -> bool {
    index
        .get(token)
        .is_some_and(|turns| turns.iter().any(|&t| t > after))
}

fn classify(body: &str) -> String {
    if body.contains("diff --git") {
        "diff".to_string()
    } else if crate::search::location_map(body, usize::MAX).is_some() {
        "search".to_string()
    } else {
        "other".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secondwind_core::{Origin, Party};
    use secondwind_core::{Provenance, Segment, Turn};

    fn tool_turn(index: usize, body: &str) -> Turn {
        Turn {
            index,
            role: Role::Tool,
            timestamp: None,
            model: None,
            sidechain: false,
            segments: vec![Segment {
                kind: SegmentKind::ToolResult {
                    tool: "grep".into(),
                    id: None,
                },
                original: None,
                effective: body.to_string(),
            }],
            billing: None,
        }
    }

    fn assistant_turn(index: usize, text: &str) -> Turn {
        Turn {
            index,
            role: Role::Assistant,
            timestamp: None,
            model: None,
            sidechain: false,
            segments: vec![Segment {
                kind: SegmentKind::Text,
                original: None,
                effective: text.to_string(),
            }],
            billing: None,
        }
    }

    fn trace(turns: Vec<Turn>) -> Trace {
        Trace {
            id: "t".into(),
            source: "test".into(),
            optimizer: None,
            provenance: Provenance {
                origin: Origin::RealWork,
                party: Party::FirstParty,
            },
            turns,
        }
    }

    fn big_search_body(marker: &str) -> String {
        let mut out = String::new();
        for i in 0..40 {
            out.push_str(&format!(
                "src/module_{i}.rs:{i}:snippet {marker}_{i} here\n"
            ));
        }
        out
    }

    #[test]
    fn residual_referenced_later_counts_as_a_reopen() {
        let body = big_search_body("uniquetoken");
        let t = trace(vec![
            tool_turn(0, &body),
            assistant_turn(1, "I will use uniquetoken_7 from the results"),
        ]);
        let stats = measure(&t);
        let s = stats.by_shape.get("search").unwrap();
        assert_eq!(s.offloads, 1);
        assert_eq!(s.reopens, 1);
    }

    #[test]
    fn residual_never_referenced_is_not_a_reopen() {
        let body = big_search_body("uniquetoken");
        let t = trace(vec![
            tool_turn(0, &body),
            assistant_turn(1, "thanks, moving on to something unrelated entirely"),
        ]);
        let stats = measure(&t);
        let s = stats.by_shape.get("search").unwrap();
        assert_eq!(s.offloads, 1);
        assert_eq!(s.reopens, 0);
    }

    #[test]
    fn a_reference_before_the_result_does_not_count() {
        let body = big_search_body("uniquetoken");
        let t = trace(vec![
            assistant_turn(0, "please search for uniquetoken_7"),
            tool_turn(1, &body),
        ]);
        let stats = measure(&t);
        let s = stats.by_shape.get("search").unwrap();
        assert_eq!(s.reopens, 0);
    }

    #[test]
    fn a_token_the_block_did_not_introduce_does_not_count() {
        let body = big_search_body("uniquetoken");
        let t = trace(vec![
            assistant_turn(0, "context mentions uniquetoken_7 up front"),
            tool_turn(1, &body),
            assistant_turn(2, "later I still cite uniquetoken_7"),
        ]);
        let stats = measure(&t);
        let s = stats.by_shape.get("search").unwrap();
        assert_eq!(s.offloads, 1);
        assert_eq!(s.reopens, 0);
    }
}
