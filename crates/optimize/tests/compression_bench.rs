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

// Savings + fidelity on a corpus of tool-output shapes. Fidelity = every significant value present
// inline or recoverable via the offload marker; the test fails on any lost value, so savings are
// only ever reported next to verified-lossless output.
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
        let (effective, transform, recovered) = match optimizer.compress_block(case.raw) {
            Outcome::Compressed {
                wire, transform, ..
            } => (wire, transform.to_string(), None),
            Outcome::Offloaded { stub, marker, .. } => {
                let body = optimizer.resolve(&marker);
                (stub, "offload".to_string(), body)
            }
            Outcome::KeptVerbatim { .. } => (case.raw.to_string(), "verbatim".to_string(), None),
        };

        let available = match &recovered {
            Some(body) => format!("{effective}\n{body}"),
            None => effective.clone(),
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
