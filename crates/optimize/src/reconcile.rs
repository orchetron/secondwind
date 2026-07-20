use crate::netcost::Zone;

#[derive(Debug, Clone, Copy)]
pub struct Predicted {
    pub zone: Zone,
    pub saved_usd: f64,
    pub wire_bytes: usize,
    pub canonical_bytes: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct Realized {
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reconciliation {
    Held,
    CacheBust,
}

// A frozen rewrite should keep reads on the cached prefix; creation tokens
// dominating reads means the prefix was rebuilt, so the transform busted cache.
pub fn reconcile(predicted: &Predicted, realized: &Realized) -> Reconciliation {
    if predicted.zone == Zone::Frozen
        && realized.cache_creation_tokens > realized.cache_read_tokens
        && realized.cache_creation_tokens > 0
    {
        return Reconciliation::CacheBust;
    }
    Reconciliation::Held
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frozen() -> Predicted {
        Predicted {
            zone: Zone::Frozen,
            saved_usd: 1.0,
            wire_bytes: 100,
            canonical_bytes: 200,
        }
    }

    #[test]
    fn healthy_reuse_holds() {
        let held = reconcile(
            &frozen(),
            &Realized {
                input_tokens: 10,
                cache_read_tokens: 9000,
                cache_creation_tokens: 0,
            },
        );
        assert_eq!(held, Reconciliation::Held);
    }

    #[test]
    fn creation_dominating_reads_on_frozen_is_a_bust() {
        let busted = reconcile(
            &frozen(),
            &Realized {
                input_tokens: 10,
                cache_read_tokens: 100,
                cache_creation_tokens: 9000,
            },
        );
        assert_eq!(busted, Reconciliation::CacheBust);
    }

    #[test]
    fn suffix_transforms_never_report_a_bust() {
        let suffix = Predicted {
            zone: Zone::Suffix,
            ..frozen()
        };
        let out = reconcile(
            &suffix,
            &Realized {
                input_tokens: 10,
                cache_read_tokens: 0,
                cache_creation_tokens: 9000,
            },
        );
        assert_eq!(out, Reconciliation::Held);
    }
}
