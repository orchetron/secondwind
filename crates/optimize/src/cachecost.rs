use secondwind_core::{SegmentKind, Trace};
use secondwind_ledger::Rates;

use crate::{Optimizer, Outcome};

// Net dollars a compression removes, priced two ways: naive (every resend billed as
// full input) vs cache-adjusted (first send at input rate, resends at cache-read rate).
// The gap shows how much prompt caching deflates a headline savings number.

#[derive(Default, Debug, Clone, Copy)]
pub struct CacheSavings {
    pub blocks: u64,
    pub original_tokens: u64,
    pub compressed_tokens: u64,
    pub naive_usd: f64,
    pub cache_adjusted_usd: f64,
}

impl CacheSavings {
    pub fn merge(&mut self, other: &CacheSavings) {
        self.blocks += other.blocks;
        self.original_tokens += other.original_tokens;
        self.compressed_tokens += other.compressed_tokens;
        self.naive_usd += other.naive_usd;
        self.cache_adjusted_usd += other.cache_adjusted_usd;
    }
}

pub fn estimate(trace: &Trace, rates: &Rates) -> CacheSavings {
    let input = rates.input / 1e6;
    let cache_read = rates.cache_read / 1e6;
    let turns = trace.turns.len();
    let mut out = CacheSavings::default();

    for (t, turn) in trace.turns.iter().enumerate() {
        for segment in &turn.segments {
            if !matches!(segment.kind, SegmentKind::ToolResult { .. }) {
                continue;
            }
            let original = segment.original.as_deref().unwrap_or(&segment.effective);
            let before = tokens(original.len());
            let after = tokens(compressed_len(original));
            if after >= before {
                continue;
            }

            let saved = before - after;
            // A block at turn t is resent on every later turn; those resends are cache reads.
            let reuses = (turns - t - 1) as f64;
            out.blocks += 1;
            out.original_tokens += before;
            out.compressed_tokens += after;
            out.naive_usd += saved as f64 * input * (1.0 + reuses);
            out.cache_adjusted_usd += saved as f64 * (input + cache_read * reuses);
        }
    }
    out
}

fn tokens(bytes: usize) -> u64 {
    (bytes / 4) as u64
}

fn compressed_len(raw: &str) -> usize {
    match Optimizer::default().compress_block(raw) {
        Outcome::Compressed { wire, .. } => wire.len(),
        Outcome::Offloaded { stub, .. } => stub.len(),
        Outcome::KeptVerbatim { .. } => raw.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secondwind_core::{Origin, Party, Provenance, Role, Segment, Turn};

    fn rates() -> Rates {
        Rates {
            input: 5.0,
            output: 25.0,
            cache_read: 0.5,
            cache_write_5m: 6.25,
            cache_write_1h: 10.0,
        }
    }

    fn tool_turn(index: usize, body: &str) -> Turn {
        Turn {
            index,
            role: Role::User,
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

    fn big() -> String {
        (0..40)
            .map(|i| {
                format!(
                    "The handler_{i} module validates each request against the session store first. "
                )
            })
            .collect()
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

    #[test]
    fn a_compressible_block_saves_and_cache_deflates_it() {
        let t = trace(vec![
            tool_turn(0, &big()),
            tool_turn(1, "small"),
            tool_turn(2, "small"),
        ]);
        let s = estimate(&t, &rates());
        assert_eq!(s.blocks, 1);
        assert!(s.compressed_tokens < s.original_tokens);
        assert!(s.naive_usd > 0.0);
        // Reuses priced at the cheap cache-read rate, so the honest figure is lower.
        assert!(s.cache_adjusted_usd < s.naive_usd);
    }

    #[test]
    fn an_incompressible_block_is_skipped() {
        let s = estimate(&trace(vec![tool_turn(0, "short")]), &rates());
        assert_eq!(s.blocks, 0);
        assert_eq!(s.naive_usd, 0.0);
    }
}
