use std::collections::BTreeMap;
use std::fmt::Write as _;

use secondwind_analyzers::{Retention, ViolationClass};
use secondwind_core::{Origin, Party, Trace};
use secondwind_ledger::LedgerBuilder;
use serde::Serialize;

use crate::commas;

#[derive(Debug, Serialize)]
pub struct Row {
    pub optimizer: String,
    pub traces: usize,
    pub turns: usize,
    pub real_work_traces: usize,
    pub synthetic_traces: usize,
    pub donated_traces: usize,
    pub paired_segments: usize,
    pub fabrication: usize,
    pub numeric_drift: usize,
    pub artifact_loss: usize,
    pub retention: Retention,
    pub billed_tokens: u64,
    pub single_fleet: bool,
}

pub fn build(traces: &[Trace]) -> Vec<Row> {
    let detectors = secondwind_analyzers::all();
    let mut groups: BTreeMap<String, Vec<&Trace>> = BTreeMap::new();
    for trace in traces {
        let key = trace.optimizer.clone().unwrap_or_else(|| "none".into());
        groups.entry(key).or_default().push(trace);
    }

    groups
        .into_iter()
        .map(|(optimizer, group)| {
            let mut ledger = LedgerBuilder::default();
            let mut retention = Retention::default();
            let mut fabrication = 0;
            let mut numeric_drift = 0;
            let mut artifact_loss = 0;
            let mut paired_segments = 0;
            for trace in &group {
                ledger.add(trace);
                retention.add_trace(trace);
                paired_segments += trace
                    .turns
                    .iter()
                    .flat_map(|t| t.segments.iter())
                    .filter(|s| s.original.is_some())
                    .count();
                for detector in &detectors {
                    for finding in detector.analyze(trace) {
                        match finding.class {
                            ViolationClass::Fabrication => fabrication += 1,
                            ViolationClass::NumericDrift => numeric_drift += 1,
                            ViolationClass::ArtifactLoss => artifact_loss += 1,
                        }
                    }
                }
            }
            let summary = ledger.summary();
            let donated = group
                .iter()
                .filter(|t| t.provenance.party == Party::Donated)
                .count();
            Row {
                optimizer,
                traces: group.len(),
                turns: group.iter().map(|t| t.turns.len()).sum(),
                real_work_traces: group
                    .iter()
                    .filter(|t| t.provenance.origin == Origin::RealWork)
                    .count(),
                synthetic_traces: group
                    .iter()
                    .filter(|t| t.provenance.origin == Origin::Synthetic)
                    .count(),
                donated_traces: donated,
                paired_segments,
                fabrication,
                numeric_drift,
                artifact_loss,
                retention,
                billed_tokens: summary.billed_tokens,
                single_fleet: donated == 0,
            }
        })
        .collect()
}

pub fn to_markdown(rows: &[Row], method_version: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# secondwind scoreboard\n");
    let _ = writeln!(
        out,
        "Measured by [secondwind](https://github.com/secondwind) under docs/METHOD.md {method_version}. Every number re-runs with `secondwind repro <trace.json>` against the corpus files in this repository.\n"
    );
    let _ = writeln!(
        out,
        "| optimizer | traces | corpus | paired segments | fabrication | numeric drift | artifact loss | numerics kept | artifacts kept | billed tokens | sample |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|---|---|---|---|---|");
    for row in rows {
        let corpus = format!(
            "{} real-work / {} synthetic / {} donated",
            row.real_work_traces, row.synthetic_traces, row.donated_traces
        );
        let sample = if row.single_fleet {
            "single-fleet"
        } else {
            "multi-fleet"
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {}/{} | {}/{} | {} | {} |",
            row.optimizer,
            row.traces,
            corpus,
            commas(row.paired_segments as u64),
            row.fabrication,
            row.numeric_drift,
            row.artifact_loss,
            commas(row.retention.numerics_kept),
            commas(row.retention.numerics_total),
            commas(row.retention.artifacts_kept),
            commas(row.retention.artifacts_total),
            commas(row.billed_tokens),
            sample,
        );
    }
    let _ = writeln!(
        out,
        "\nRate and token figures from a single-fleet sample describe that corpus only; they are labeled and are not generalization claims. Violation receipts are existence proofs and hold at any sample size."
    );
    out
}

pub fn to_json(rows: &[Row]) -> String {
    serde_json::to_string_pretty(rows).expect("rows serialize")
}

#[cfg(test)]
mod tests {
    use super::*;
    use secondwind_core::{Provenance, Role, Segment, SegmentKind, Turn};

    fn trace(
        optimizer: Option<&str>,
        party: Party,
        effective: &str,
        original: Option<&str>,
    ) -> Trace {
        Trace {
            id: "t".into(),
            source: "test".into(),
            optimizer: optimizer.map(str::to_string),
            provenance: Provenance {
                origin: Origin::RealWork,
                party,
            },
            turns: vec![Turn {
                index: 0,
                role: Role::User,
                timestamp: None,
                model: None,
                sidechain: false,
                segments: vec![Segment {
                    kind: SegmentKind::ToolResult {
                        tool: "Grep".into(),
                        id: None,
                    },
                    original: original.map(str::to_string),
                    effective: effective.into(),
                }],
                billing: None,
            }],
        }
    }

    #[test]
    fn groups_by_optimizer_and_counts_violations() {
        let traces = vec![
            trace(
                Some("acme"),
                Party::FirstParty,
                "src/a.rs:42:let x = 2;",
                Some("src/a.rs:42:let x = 1;"),
            ),
            trace(None, Party::Donated, "plain", None),
        ];
        let rows = build(&traces);
        assert_eq!(rows.len(), 2);

        let acme = rows.iter().find(|r| r.optimizer == "acme").unwrap();
        assert_eq!(acme.fabrication, 1);
        assert_eq!(acme.paired_segments, 1);
        assert!(acme.single_fleet);

        let markdown = to_markdown(&rows, "v0.1.0");
        assert!(markdown.contains("| acme | 1 |"));
        assert!(markdown.contains("single-fleet"));
        assert!(markdown.contains("existence proofs"));
    }
}
