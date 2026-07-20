#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use secondwind_core::Trace;
use serde::Serialize;

pub mod events;

#[derive(Debug, Clone, Copy)]
pub struct Rates {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
}

const USD_PER_MTOK: &[(&str, Rates)] = &[
    (
        "claude-fable-5",
        Rates {
            input: 10.0,
            output: 50.0,
            cache_read: 1.0,
            cache_write_5m: 12.5,
            cache_write_1h: 20.0,
        },
    ),
    (
        "claude-mythos-5",
        Rates {
            input: 10.0,
            output: 50.0,
            cache_read: 1.0,
            cache_write_5m: 12.5,
            cache_write_1h: 20.0,
        },
    ),
    (
        "claude-opus-4-8",
        Rates {
            input: 5.0,
            output: 25.0,
            cache_read: 0.5,
            cache_write_5m: 6.25,
            cache_write_1h: 10.0,
        },
    ),
    (
        "claude-sonnet-5",
        Rates {
            input: 2.0,
            output: 10.0,
            cache_read: 0.2,
            cache_write_5m: 2.5,
            cache_write_1h: 4.0,
        },
    ),
    (
        "claude-sonnet-4-6",
        Rates {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write_5m: 3.75,
            cache_write_1h: 6.0,
        },
    ),
    (
        "claude-sonnet-4-5",
        Rates {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write_5m: 3.75,
            cache_write_1h: 6.0,
        },
    ),
    (
        "claude-haiku-4-5",
        Rates {
            input: 1.0,
            output: 5.0,
            cache_read: 0.1,
            cache_write_5m: 1.25,
            cache_write_1h: 2.0,
        },
    ),
];

pub fn rates_for(model: &str) -> Option<&'static Rates> {
    let mut best: Option<(&str, &Rates)> = None;
    for (prefix, rates) in USD_PER_MTOK {
        if model.starts_with(prefix) && best.is_none_or(|(b, _)| prefix.len() > b.len()) {
            best = Some((prefix, rates));
        }
    }
    best.map(|(_, r)| r)
}

#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq)]
pub struct ModelSpend {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_5m_tokens: u64,
    pub cache_write_1h_tokens: u64,
    pub actual_usd: f64,
    pub without_caching_usd: f64,
}

#[derive(Debug, Default, Serialize)]
pub struct LedgerSummary {
    pub actual_usd: f64,
    pub without_caching_usd: f64,
    pub caching_saved_usd: f64,
    // Provider-counted tokens (input+output+cache) for effective traffic. Rate-free,
    // so exact even for unpriced models.
    pub billed_tokens: u64,
    pub by_model: BTreeMap<String, ModelSpend>,
    pub unpriced_models: BTreeMap<String, u64>,
}

#[derive(Debug, Default)]
pub struct LedgerBuilder {
    by_model: BTreeMap<String, ModelSpend>,
    unpriced: BTreeMap<String, u64>,
    total_tokens: u64,
}

impl LedgerBuilder {
    pub fn add(&mut self, trace: &Trace) {
        for turn in &trace.turns {
            let Some(billing) = turn.billing else {
                continue;
            };
            let model = turn.model.as_deref().unwrap_or("unknown");
            let total_tokens = billing.input_tokens
                + billing.output_tokens
                + billing.cache_read_tokens
                + billing.cache_write_tokens();
            self.total_tokens += total_tokens;

            let Some(rates) = rates_for(model) else {
                *self.unpriced.entry(model.to_string()).or_insert(0) += total_tokens;
                continue;
            };

            let per = |tokens: u64, usd_per_mtok: f64| tokens as f64 * usd_per_mtok / 1e6;
            let actual = per(billing.input_tokens, rates.input)
                + per(billing.output_tokens, rates.output)
                + per(billing.cache_read_tokens, rates.cache_read)
                + per(billing.cache_write_5m_tokens, rates.cache_write_5m)
                + per(billing.cache_write_1h_tokens, rates.cache_write_1h);
            let without_caching = per(
                billing.input_tokens + billing.cache_read_tokens + billing.cache_write_tokens(),
                rates.input,
            ) + per(billing.output_tokens, rates.output);

            let entry = self.by_model.entry(model.to_string()).or_default();
            entry.input_tokens += billing.input_tokens;
            entry.output_tokens += billing.output_tokens;
            entry.cache_read_tokens += billing.cache_read_tokens;
            entry.cache_write_5m_tokens += billing.cache_write_5m_tokens;
            entry.cache_write_1h_tokens += billing.cache_write_1h_tokens;
            entry.actual_usd += actual;
            entry.without_caching_usd += without_caching;
        }
    }

    pub fn summary(self) -> LedgerSummary {
        let actual_usd: f64 = self.by_model.values().map(|m| m.actual_usd).sum();
        let without_caching_usd: f64 = self.by_model.values().map(|m| m.without_caching_usd).sum();
        LedgerSummary {
            actual_usd,
            without_caching_usd,
            caching_saved_usd: without_caching_usd - actual_usd,
            billed_tokens: self.total_tokens,
            by_model: self.by_model,
            unpriced_models: self.unpriced.into_iter().filter(|(_, t)| *t > 0).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secondwind_core::{Billing, Origin, Party, Provenance, Role, Trace, Turn};

    fn trace_with(model: &str, billing: Billing) -> Trace {
        Trace {
            id: "t".into(),
            source: "test".into(),
            optimizer: None,
            provenance: Provenance {
                origin: Origin::Synthetic,
                party: Party::FirstParty,
            },
            turns: vec![Turn {
                index: 0,
                role: Role::Assistant,
                timestamp: None,
                model: Some(model.into()),
                sidechain: false,
                segments: Vec::new(),
                billing: Some(billing),
            }],
        }
    }

    #[test]
    fn prices_cached_and_uncached_paths() {
        let mut builder = LedgerBuilder::default();
        builder.add(&trace_with(
            "claude-sonnet-4-5-20250929",
            Billing {
                input_tokens: 1_000_000,
                output_tokens: 1_000_000,
                cache_read_tokens: 1_000_000,
                cache_write_5m_tokens: 1_000_000,
                cache_write_1h_tokens: 0,
            },
        ));
        let summary = builder.summary();

        assert!((summary.actual_usd - (3.0 + 15.0 + 0.3 + 3.75)).abs() < 1e-9);
        assert!((summary.without_caching_usd - (3.0 * 3.0 + 15.0)).abs() < 1e-9);
        assert!((summary.caching_saved_usd - (6.0 - 4.05)).abs() < 1e-9);
        assert!(summary.unpriced_models.is_empty());
    }

    #[test]
    fn unknown_models_are_reported_not_guessed() {
        let mut builder = LedgerBuilder::default();
        builder.add(&trace_with(
            "gpt-5.6-terra",
            Billing {
                input_tokens: 500,
                output_tokens: 100,
                cache_read_tokens: 0,
                cache_write_5m_tokens: 0,
                cache_write_1h_tokens: 0,
            },
        ));
        let summary = builder.summary();

        assert_eq!(summary.actual_usd, 0.0);
        assert_eq!(summary.unpriced_models.get("gpt-5.6-terra"), Some(&600));
    }

    #[test]
    fn longest_prefix_wins() {
        assert!((rates_for("claude-sonnet-4-6").unwrap().input - 3.0).abs() < 1e-9);
        assert!((rates_for("claude-sonnet-5-20260201").unwrap().input - 2.0).abs() < 1e-9);
        assert!(rates_for("claude-nonexistent").is_none());
    }
}
