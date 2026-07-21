use std::collections::{HashMap, HashSet};

const MIN_ATOM: usize = 6;

// Retention weighted by each value's rarity, so dropping a unique value costs more
// than repeated filler and blinding compression scores low, not like a win.
#[derive(Debug, Clone, Copy)]
pub struct Richness {
    pub atoms: usize,
    pub kept: usize,
    pub retained: f64,
}

// `available` is everything the agent can still see or fetch: compressed output plus any recoverable body.
pub fn score(original: &str, available: &str) -> Richness {
    let mut counts: HashMap<&str, u32> = HashMap::new();
    let mut total = 0u32;
    for atom in significant(original) {
        *counts.entry(atom).or_insert(0) += 1;
        total += 1;
    }
    if counts.is_empty() {
        return Richness {
            atoms: 0,
            kept: 0,
            retained: 1.0,
        };
    }

    // Tokenize `available` once: a value present as a delimited token is an O(1) hit, so the scan
    // stays linear on large uniform output instead of O(atoms x len). A token is a substring, so
    // this equals the substring check but skips the scan on the common path.
    let seen: HashSet<&str> = significant(available).collect();

    let n = total as f64;
    let mut total_weight = 0.0;
    let mut kept_weight = 0.0;
    let mut kept = 0;
    for (atom, &count) in &counts {
        let weight = -(count as f64 / n).log2();
        total_weight += weight;
        if seen.contains(atom) || present(atom, available) {
            kept_weight += weight;
            kept += 1;
        }
    }
    Richness {
        atoms: counts.len(),
        kept,
        retained: if total_weight > 0.0 {
            kept_weight / total_weight
        } else {
            1.0
        },
    }
}

fn present(atom: &str, available: &str) -> bool {
    available.contains(atom)
}

fn significant(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| c.is_whitespace())
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| w.len() >= MIN_ATOM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_retention_scores_one() {
        let original = "authentication service validates every inbound request thoroughly";
        let r = score(original, original);
        assert_eq!(r.kept, r.atoms);
        assert!((r.retained - 1.0).abs() < 1e-9);
    }

    #[test]
    fn dropping_a_rare_value_costs_more_than_dropping_a_common_one() {
        // "common12" appears many times; "uniquevalue" once.
        let mut original = String::new();
        for _ in 0..20 {
            original.push_str("common12 ");
        }
        original.push_str("uniquevalue");

        let without_rare = original.replace("uniquevalue", "");
        let without_common = original.replacen("common12", "", 1);

        let rare_dropped = score(&original, &without_rare).retained;
        let common_dropped = score(&original, &without_common).retained;
        assert!(rare_dropped < common_dropped);
    }

    #[test]
    fn blinding_compression_scores_near_zero() {
        let original = (0..50)
            .map(|i| format!("record_{i}_payload_{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        // A 99%-smaller output that keeps almost nothing.
        let blinded = "record_0_payload_0";
        let r = score(&original, blinded);
        assert!(r.retained < 0.1);
    }
}
