use secondwind_optimize::{Optimizer, Outcome, richness};

struct Case {
    name: &'static str,
    raw: &'static str,
}

fn corpus() -> Vec<Case> {
    vec![
        Case {
            name: "high-cardinality array",
            raw: include_str!("../../../bench/compression/corpus/01-highcard-array.json"),
        },
        Case {
            name: "low-cardinality array",
            raw: include_str!("../../../bench/compression/corpus/02-lowcard-array.json"),
        },
        Case {
            name: "flat object",
            raw: include_str!("../../../bench/compression/corpus/03-flat-object.json"),
        },
        Case {
            name: "small array",
            raw: include_str!("../../../bench/compression/corpus/04-small-array.json"),
        },
    ]
}

// Savings + fidelity on tool-output shapes. A Compressed outcome is lossless by the admit certificate
// (decode == raw), so it scores full fidelity by construction; offload fidelity is checked against the
// resolved body, the one path where recovery can actually fail.
#[test]
fn compression_savings_and_fidelity() {
    eprintln!(
        "compression benchmark (byte reduction, fidelity verified against the recovered body)"
    );
    eprintln!(
        "  {:<24} {:>8} {:>8} {:>8}  {:<9} values",
        "shape", "in", "out", "saving", "transform"
    );
    for case in corpus() {
        let mut optimizer = Optimizer::default();
        let (effective, transform, available) = match optimizer.compress_block(case.raw) {
            Outcome::Compressed {
                wire, transform, ..
            } => {
                // Certified lossless: the wire decodes to raw, so all values are recoverable.
                (wire, transform.to_string(), case.raw.to_string())
            }
            Outcome::Offloaded { stub, marker, .. } => {
                let body = optimizer.resolve(&marker).unwrap_or_default();
                let available = format!("{stub}\n{body}");
                (stub, "offload".to_string(), available)
            }
            Outcome::KeptVerbatim { .. } => (
                case.raw.to_string(),
                "verbatim".to_string(),
                case.raw.to_string(),
            ),
        };

        let fidelity = richness::score(case.raw, &available);
        let saving = 100.0 * (1.0 - effective.len() as f64 / case.raw.len() as f64);
        eprintln!(
            "  {:<24} {:>8} {:>8} {saving:>7.1}%  {transform:<9} {}/{}",
            case.name,
            case.raw.len(),
            effective.len(),
            fidelity.kept,
            fidelity.atoms
        );

        assert_eq!(
            fidelity.kept, fidelity.atoms,
            "{}: {} of {} values were neither inline nor recoverable",
            case.name, fidelity.kept, fidelity.atoms
        );
    }
}
