#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefixHealth {
    Stable,
    Diverged { at_block: usize },
}

pub fn check_stability(prior_prefix: &[String], current: &[String]) -> PrefixHealth {
    for (i, prior_block) in prior_prefix.iter().enumerate() {
        match current.get(i) {
            Some(now) if now == prior_block => {}
            _ => return PrefixHealth::Diverged { at_block: i },
        }
    }
    PrefixHealth::Stable
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn identical_prefix_is_stable() {
        let prior = v(&["a", "b"]);
        let current = v(&["a", "b", "c"]);
        assert_eq!(check_stability(&prior, &current), PrefixHealth::Stable);
    }

    #[test]
    fn a_changed_prefix_block_diverges() {
        let prior = v(&["a", "b"]);
        let current = v(&["a", "B", "c"]);
        assert_eq!(
            check_stability(&prior, &current),
            PrefixHealth::Diverged { at_block: 1 }
        );
    }

    #[test]
    fn a_dropped_prefix_block_diverges() {
        let prior = v(&["a", "b"]);
        let current = v(&["a"]);
        assert_eq!(
            check_stability(&prior, &current),
            PrefixHealth::Diverged { at_block: 1 }
        );
    }
}
