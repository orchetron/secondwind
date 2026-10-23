use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::counterfactual::BlockOutcome;

// Measured reopen rates per command class and size, and the line above which offloading a block
// costs more extra turns than it saves. A class or size with too few offloads falls back to the
// class, then to the prior. The built-in table comes from 1,491 real Claude Code sessions;
// `secondwind eval --learn` fits one to the user's own history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReopenPolicy {
    pub version: u32,
    pub threshold: f64,
    pub prior: f64,
    pub classes: BTreeMap<String, ClassRate>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClassRate {
    pub rate: f64,
    pub n: u64,
    #[serde(default)]
    pub sizes: BTreeMap<String, Rate>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Rate {
    pub rate: f64,
    pub n: u64,
}

pub const DEFAULT_THRESHOLD: f64 = 0.4;
// Reopens and offloads.
type Counts = [u64; 2];
const BUILTIN: &str = include_str!("reopen_rates.json");

impl ReopenPolicy {
    pub fn builtin() -> Self {
        serde_json::from_str(BUILTIN).expect("the built-in reopen table parses")
    }

    pub fn from_json(text: &str) -> Option<Self> {
        serde_json::from_str::<Self>(text)
            .ok()
            .filter(|p| p.version == 1 && (0.0..=1.0).contains(&p.threshold))
    }

    // Laplace-smoothed rates from offloaded blocks; a class or size needs `min` offloads to count.
    pub fn learn<'a>(blocks: impl IntoIterator<Item = &'a BlockOutcome>, min: u64) -> Self {
        let mut all: Counts = [0; 2];
        let mut classes: BTreeMap<String, (Counts, BTreeMap<&'static str, Counts>)> =
            BTreeMap::new();
        for b in blocks.into_iter().filter(|b| b.outcome == "offload") {
            let hit = u64::from(b.reopened);
            let entry = classes.entry(b.class.clone()).or_default();
            for counts in [
                &mut all,
                &mut entry.0,
                entry.1.entry(size_band(b.raw_tokens)).or_default(),
            ] {
                counts[0] += hit;
                counts[1] += 1;
            }
        }
        let smooth = |c: Counts| Rate {
            rate: (c[0] + 1) as f64 / (c[1] + 2) as f64,
            n: c[1],
        };
        Self {
            version: 1,
            threshold: DEFAULT_THRESHOLD,
            prior: smooth(all).rate,
            classes: classes
                .into_iter()
                .filter(|(_, (counts, _))| counts[1] >= min)
                .map(|(class, (counts, sizes))| {
                    let whole = smooth(counts);
                    let sizes = sizes
                        .into_iter()
                        .filter(|(_, c)| c[1] >= min)
                        .map(|(band, c)| (band.to_string(), smooth(c)))
                        .collect();
                    (
                        class,
                        ClassRate {
                            rate: whole.rate,
                            n: whole.n,
                            sizes,
                        },
                    )
                })
                .collect(),
        }
    }

    pub fn predicted(&self, class: &str, raw_tokens: u64) -> f64 {
        let Some(c) = self.classes.get(class) else {
            return self.prior;
        };
        c.sizes
            .get(size_band(raw_tokens))
            .map_or(c.rate, |size| size.rate)
    }

    pub fn allows_offload(&self, class: &str, raw_tokens: u64) -> bool {
        self.predicted(class, raw_tokens) < self.threshold
    }
}

pub fn size_band(raw_tokens: u64) -> &'static str {
    match raw_tokens {
        0..500 => "<500",
        500..2000 => "<2k",
        2000..8000 => "<8k",
        _ => "8k+",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(class: &str, raw_tokens: u64, reopened: bool) -> BlockOutcome {
        BlockOutcome {
            tool: "Bash".into(),
            class: class.into(),
            shape: "other".into(),
            outcome: "offload",
            turn: 0,
            raw_bytes: raw_tokens as usize * 4,
            raw_tokens,
            sent_tokens: 10,
            reopened,
            faithful: true,
            context_tokens: 0,
            turns_after: 0,
        }
    }

    #[test]
    fn the_builtin_table_parses_and_ranks_whole_reads_above_searches() {
        let p = ReopenPolicy::builtin();
        assert_eq!(p.threshold, DEFAULT_THRESHOLD);
        assert!(p.predicted("Read", 5000) > p.predicted("Bash:grep |", 300));
        assert!(
            !p.allows_offload("Bash:cat", 5000),
            "a large cat is read whole"
        );
        assert!(p.allows_offload("Bash:grep |", 300), "a small grep is not");
    }

    #[test]
    fn prediction_falls_back_from_size_to_class_to_prior() {
        let mut blocks: Vec<BlockOutcome> = (0..40)
            .map(|i| block("Bash:cat", 3000, i % 4 != 0))
            .collect();
        blocks.extend((0..40).map(|_| block("Bash:cat", 100, false)));
        blocks.extend((0..5).map(|_| block("Bash:rare", 100, true)));
        let p = ReopenPolicy::learn(&blocks, 30);

        assert!(p.predicted("Bash:cat", 3000) > 0.7);
        assert!(p.predicted("Bash:cat", 100) < 0.05);
        let class_rate = p.classes["Bash:cat"].rate;
        assert_eq!(
            p.predicted("Bash:cat", 20_000),
            class_rate,
            "no 8k+ band, so the class"
        );
        assert_eq!(
            p.predicted("Bash:rare", 100),
            p.prior,
            "too few offloads, so the prior"
        );
    }

    #[test]
    fn learn_ignores_blocks_that_were_not_offloaded() {
        let mut inline = block("Bash:cat", 3000, false);
        inline.outcome = "inline";
        let p = ReopenPolicy::learn(&[inline], 1);
        assert!(p.classes.is_empty());
    }

    #[test]
    fn a_policy_round_trips_and_a_bad_one_is_refused() {
        let p = ReopenPolicy::builtin();
        let text = serde_json::to_string(&p).unwrap();
        assert_eq!(ReopenPolicy::from_json(&text), Some(p));
        assert_eq!(
            ReopenPolicy::from_json(r#"{"version":2,"threshold":0.4,"prior":0.4,"classes":{}}"#),
            None
        );
        assert_eq!(
            ReopenPolicy::from_json(r#"{"version":1,"threshold":7,"prior":0.4,"classes":{}}"#),
            None
        );
        assert_eq!(ReopenPolicy::from_json("not json"), None);
    }
}
