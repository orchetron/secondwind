use secondwind_ledger::rates_for;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    // Inside the cached prefix: reads bill at 0.1x, rewrites at the write premium.
    Frozen,
    // Fresh, uncached: bills at full input rate.
    Suffix,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    Save { usd: f64 },
    Refuse(Reason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    FrozenWriteWouldExceedRead,
    NotWorthIt,
    UnknownModel,
}

// Coarse on purpose: underestimating savings only refuses a marginal win, it
// never approves a losing transform.
const CHARS_PER_TOKEN: f64 = 4.0;

pub struct NetCostGate {
    model: String,
    remaining_turns: u32,
}

impl NetCostGate {
    pub fn new(model: impl Into<String>, remaining_turns: u32) -> Self {
        Self {
            model: model.into(),
            remaining_turns: remaining_turns.max(1),
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    // Reprice against a different model so a proxy can follow the model each request names.
    pub fn set_model(&mut self, model: impl Into<String>) {
        self.model = model.into();
    }

    // Byte inputs, converted to tokens by a coarse ratio.
    pub fn score(&self, zone: Zone, before_bytes: usize, after_bytes: usize) -> Verdict {
        self.price(
            zone,
            before_bytes as f64 / CHARS_PER_TOKEN,
            after_bytes as f64 / CHARS_PER_TOKEN,
        )
    }

    // Token inputs from a real tokenizer, priced directly on the billed unit.
    pub fn score_tokens(&self, zone: Zone, before_tokens: usize, after_tokens: usize) -> Verdict {
        self.price(zone, before_tokens as f64, after_tokens as f64)
    }

    fn price(&self, zone: Zone, before: f64, after: f64) -> Verdict {
        let Some(rates) = rates_for(&self.model) else {
            return Verdict::Refuse(Reason::UnknownModel);
        };
        if after >= before {
            return Verdict::Refuse(Reason::NotWorthIt);
        }

        let saved_tokens = before - after;
        let write_tokens = after;
        let per = |tokens: f64, rate: f64| tokens * rate / 1e6;

        match zone {
            Zone::Suffix => Verdict::Save {
                usd: per(saved_tokens, rates.input),
            },
            Zone::Frozen => {
                let read_saved = per(saved_tokens, rates.cache_read) * self.remaining_turns as f64;
                let write_penalty = per(write_tokens, rates.cache_write_5m);
                if write_penalty >= read_saved {
                    Verdict::Refuse(Reason::FrozenWriteWouldExceedRead)
                } else {
                    Verdict::Save {
                        usd: read_saved - write_penalty,
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffix_saving_is_priced_at_input_rate() {
        let gate = NetCostGate::new("claude-sonnet-4-5", 1);
        match gate.score(Zone::Suffix, 40_000, 24_000) {
            Verdict::Save { usd } => assert!(usd > 0.0),
            other => panic!("expected save, got {other:?}"),
        }
    }

    #[test]
    fn compressing_frozen_prefix_is_refused_when_write_exceeds_read() {
        let gate = NetCostGate::new("claude-sonnet-4-5", 1);
        assert_eq!(
            gate.score(Zone::Frozen, 40_000, 24_000),
            Verdict::Refuse(Reason::FrozenWriteWouldExceedRead)
        );
    }

    #[test]
    fn frozen_can_pay_off_over_many_reuses() {
        let long = NetCostGate::new("claude-sonnet-4-5", 500);
        assert!(matches!(
            long.score(Zone::Frozen, 40_000, 24_000),
            Verdict::Save { .. }
        ));
    }

    #[test]
    fn unknown_model_refuses() {
        let gate = NetCostGate::new("gpt-5.6-terra", 1);
        assert_eq!(
            gate.score(Zone::Suffix, 100, 10),
            Verdict::Refuse(Reason::UnknownModel)
        );
    }

    #[test]
    fn no_shrink_is_refused() {
        let gate = NetCostGate::new("claude-sonnet-4-5", 1);
        assert_eq!(
            gate.score(Zone::Suffix, 100, 100),
            Verdict::Refuse(Reason::NotWorthIt)
        );
    }
}
