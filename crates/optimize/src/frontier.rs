use crate::netcost::Zone;

pub struct Frontier {
    cached_prefix_tokens: u64,
}

impl Frontier {
    pub fn from_prior_cache_read(cache_read_tokens: u64) -> Self {
        Self {
            cached_prefix_tokens: cache_read_tokens,
        }
    }

    // A block starting inside the cached span is frozen even if it straddles the
    // end, so a rewrite never touches already-cached bytes.
    pub fn zone_of(&self, cumulative_tokens_before: u64) -> Zone {
        if cumulative_tokens_before < self.cached_prefix_tokens {
            Zone::Frozen
        } else {
            Zone::Suffix
        }
    }

    pub fn zones(&self, block_tokens: &[u64]) -> Vec<Zone> {
        let mut cumulative = 0u64;
        let mut out = Vec::with_capacity(block_tokens.len());
        for &tokens in block_tokens {
            out.push(self.zone_of(cumulative));
            cumulative += tokens;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_starting_within_the_cached_span_are_frozen() {
        let frontier = Frontier::from_prior_cache_read(800);
        let zones = frontier.zones(&[400, 400, 400]);
        assert_eq!(zones, vec![Zone::Frozen, Zone::Frozen, Zone::Suffix]);
    }

    #[test]
    fn a_block_starting_inside_the_span_stays_frozen_even_if_it_straddles() {
        let frontier = Frontier::from_prior_cache_read(500);
        let zones = frontier.zones(&[400, 400]);
        assert_eq!(zones, vec![Zone::Frozen, Zone::Frozen]);
    }

    #[test]
    fn no_cache_means_everything_is_suffix() {
        let frontier = Frontier::from_prior_cache_read(0);
        let zones = frontier.zones(&[100, 100]);
        assert_eq!(zones, vec![Zone::Suffix, Zone::Suffix]);
    }
}
