// Line-period transpose for repeating multi-line records (git log, kubectl describe): lines group by
// position modulo a period into columns, each picking its own codec, best period wins. The grouping is
// a pure permutation, lossless for any input; only the saving depends on the period. Gated on a saving.

use crate::text_columnar::{cheapest, decode_value_column, generic_candidates};

const HEADER: &str = "SWREC";
const MIN_LINES: usize = 8;
const MAX_PERIOD: usize = 16;

pub struct Encoded {
    pub wire: String,
    pub decoded: String,
}

pub fn try_encode(raw: &str, cost: &dyn Fn(&str) -> usize) -> Option<Encoded> {
    let lines: Vec<&str> = raw.split('\n').collect();
    if lines.len() < MIN_LINES {
        return None;
    }
    let mut best: Option<Encoded> = None;
    for period in 2..=MAX_PERIOD.min(lines.len() / 2) {
        let wire = encode_period(&lines, period, cost);
        if cost(&wire) >= cost(raw) {
            continue;
        }
        if decode(&wire).as_deref() != Some(raw) {
            continue;
        }
        if best.as_ref().is_none_or(|b| cost(&wire) < cost(&b.wire)) {
            best = Some(Encoded {
                wire,
                decoded: raw.to_string(),
            });
        }
    }
    best
}

fn encode_period(lines: &[&str], period: usize, cost: &dyn Fn(&str) -> usize) -> String {
    let mut wire = format!("{HEADER}\t{}\t{}\n", lines.len(), period);
    for p in 0..period {
        let column: Vec<&str> = lines.iter().skip(p).step_by(period).copied().collect();
        wire.push_str(&cheapest(generic_candidates(&column), cost));
    }
    wire
}

pub fn decode(wire: &str) -> Option<String> {
    let mut lines = wire.split('\n');
    let mut head = lines.next()?.split('\t');
    if head.next()? != HEADER {
        return None;
    }
    let n: usize = head.next()?.parse().ok()?;
    let period: usize = head.next()?.parse().ok()?;
    if period == 0 {
        return None;
    }

    let mut columns: Vec<Vec<String>> = Vec::with_capacity(period);
    for p in 0..period {
        let count = n / period + usize::from(p < n % period);
        let tag = lines.next()?;
        columns.push(decode_value_column(tag, &mut lines, count)?);
    }

    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        out.push(columns[i % period].get(i / period)?.clone());
    }
    Some(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(s: &str) -> usize {
        s.len()
    }

    #[test]
    fn compresses_a_fixed_period_record_log() {
        let mut raw = String::new();
        for i in 0..20 {
            raw.push_str(&format!(
                "commit {i:040x}\nAuthor: dev <dev@example.com>\nDate:   2026-07-{:02}\n\n    change number {i}\n\n",
                (i % 28) + 1
            ));
        }
        let raw = raw.trim_end();
        let out = try_encode(raw, &bytes).expect("periodic records compress");
        assert!(
            out.wire.len() < raw.len(),
            "wire {} !< raw {}",
            out.wire.len(),
            raw.len()
        );
        assert_eq!(decode(&out.wire).as_deref(), Some(raw));
    }

    #[test]
    fn round_trips_when_length_is_not_a_multiple_of_the_period() {
        let raw = "a1\nb1\nc1\na2\nb2\nc2\na3\nb3\nc3\na4\nb4"; // 11 lines, period 3
        if let Some(out) = try_encode(raw, &bytes) {
            assert_eq!(decode(&out.wire).as_deref(), Some(raw));
        }
        // the transpose itself must be exact even when it does not save
        let wire = encode_period(&raw.split('\n').collect::<Vec<_>>(), 3, &bytes);
        assert_eq!(decode(&wire).as_deref(), Some(raw));
    }

    #[test]
    fn abstains_on_short_or_aperiodic_input() {
        assert!(try_encode("a\nb\nc", &bytes).is_none());
        assert!(
            try_encode(
                "wholly unique line one\nand a very different second line here\nthird",
                &bytes
            )
            .is_none()
        );
    }
}
