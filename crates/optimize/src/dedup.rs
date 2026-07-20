//! Cross-turn dedup: replace a byte-identical repeat of an earlier block with a short reference. Built but deliberately NOT wired in:
//! the repeat already sits in the provider's cached prefix (~0.1x), so swapping it shifts later bytes, busts the cache, and re-bills
//! the whole suffix at ~10x (net loss). Live path re-emits the frozen wire instead; if revived, gate behind NetCostGate (netcost.rs).

use std::collections::HashMap;

pub enum DedupOutcome {
    FirstSeen,
    Reference { marker: String, target: String },
}

#[derive(Default)]
pub struct Session {
    retained: HashMap<String, String>,
}

impl Session {
    // Repeat of a retained block becomes a reference; the earlier copy stays verbatim, so nothing is lost.
    pub fn observe(&mut self, raw: &str) -> DedupOutcome {
        let key = hash(raw);
        if self.retained.contains_key(&key) {
            let marker = format!("<<swref:{}>>", &key[..12]);
            if marker.len() < raw.len() {
                return DedupOutcome::Reference {
                    marker,
                    target: key,
                };
            }
        }
        self.retained
            .entry(key.clone())
            .or_insert_with(|| raw.to_string());
        DedupOutcome::FirstSeen
    }

    pub fn resolve(&self, target: &str) -> Option<&str> {
        self.retained.get(target).map(String::as_str)
    }
}

fn hash(raw: &str) -> String {
    blake3::hash(raw.as_bytes()).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_occurrence_is_retained() {
        let mut session = Session::default();
        assert!(matches!(
            session.observe("big block"),
            DedupOutcome::FirstSeen
        ));
    }

    #[test]
    fn a_repeat_becomes_a_resolvable_reference() {
        let mut session = Session::default();
        let block = "x".repeat(200);
        session.observe(&block);
        match session.observe(&block) {
            DedupOutcome::Reference { marker, target } => {
                assert!(marker.len() < block.len());
                assert_eq!(session.resolve(&target), Some(block.as_str()));
            }
            DedupOutcome::FirstSeen => panic!("expected a reference"),
        }
    }

    #[test]
    fn a_distinct_block_is_not_referenced() {
        let mut session = Session::default();
        session.observe(&"a".repeat(200));
        assert!(matches!(
            session.observe(&"b".repeat(200)),
            DedupOutcome::FirstSeen
        ));
    }

    #[test]
    fn a_repeat_too_small_to_help_is_kept() {
        let mut session = Session::default();
        session.observe("hi");
        assert!(matches!(session.observe("hi"), DedupOutcome::FirstSeen));
    }
}
