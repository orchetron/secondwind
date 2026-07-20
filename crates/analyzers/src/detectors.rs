use secondwind_core::Trace;

use crate::extract;
use crate::{Analyzer, Finding, ViolationClass};

pub struct Fabrication;
pub struct NumericDrift;
pub struct ArtifactLoss;

impl Analyzer for Fabrication {
    fn id(&self) -> &'static str {
        "fabrication"
    }

    fn analyze(&self, trace: &Trace) -> Vec<Finding> {
        let mut findings = Vec::new();
        for turn in &trace.turns {
            for segment in &turn.segments {
                let Some(original) = segment.original.as_deref() else {
                    continue;
                };
                let claims = extract::grep_claims(&segment.effective);
                if claims.is_empty() {
                    continue;
                }
                let truth: std::collections::HashMap<String, String> =
                    extract::grep_claims(original).into_iter().collect();
                for (location, claimed) in claims {
                    match truth.get(&location) {
                        None => findings.push(Finding {
                            class: ViolationClass::Fabrication,
                            trace_id: trace.id.clone(),
                            turn: turn.index,
                            original: String::new(),
                            effective: format!("{location}:{claimed}"),
                            detail: format!("{location} is claimed but absent from the original"),
                        }),
                        Some(actual) if actual != &claimed => findings.push(Finding {
                            class: ViolationClass::Fabrication,
                            trace_id: trace.id.clone(),
                            turn: turn.index,
                            original: format!("{location}:{actual}"),
                            effective: format!("{location}:{claimed}"),
                            detail: format!("content at {location} differs from the original"),
                        }),
                        Some(_) => {}
                    }
                }
            }
        }
        findings
    }
}

impl Analyzer for NumericDrift {
    fn id(&self) -> &'static str {
        "numeric-drift"
    }

    fn analyze(&self, trace: &Trace) -> Vec<Finding> {
        let mut findings = Vec::new();
        for turn in &trace.turns {
            for segment in &turn.segments {
                let Some(original) = segment.original.as_deref() else {
                    continue;
                };
                let effective = segment.effective.as_str();
                let original_pairs = grouped(extract::keyed_numbers(original));
                let effective_pairs = grouped(extract::keyed_numbers(effective));
                for (key, values) in &original_pairs {
                    let [value] = values.as_slice() else {
                        continue;
                    };
                    match effective_pairs.get(key) {
                        Some(seen) if seen.len() == 1 && seen[0] != *value => {
                            findings.push(Finding {
                                class: ViolationClass::NumericDrift,
                                trace_id: trace.id.clone(),
                                turn: turn.index,
                                original: format!("{key} = {value}"),
                                effective: format!("{key} = {}", seen[0]),
                                detail: format!("value of {key} changed"),
                            });
                        }
                        Some(_) => {}
                        None => {
                            if !effective.contains(value)
                                && referenced_later(trace, turn.index, value)
                            {
                                findings.push(Finding {
                                    class: ViolationClass::NumericDrift,
                                    trace_id: trace.id.clone(),
                                    turn: turn.index,
                                    original: format!("{key} = {value}"),
                                    effective: String::new(),
                                    detail: format!(
                                        "{key} = {value} was dropped and {value} is referenced later"
                                    ),
                                });
                            }
                        }
                    }
                }
                for value in extract::numbers(original) {
                    if !effective.contains(&value) && referenced_later(trace, turn.index, &value) {
                        let already = findings
                            .iter()
                            .any(|f| f.turn == turn.index && f.original.ends_with(&value));
                        if !already {
                            findings.push(Finding {
                                class: ViolationClass::NumericDrift,
                                trace_id: trace.id.clone(),
                                turn: turn.index,
                                original: value.clone(),
                                effective: String::new(),
                                detail: format!("{value} was dropped and is referenced later"),
                            });
                        }
                    }
                }
            }
        }
        findings
    }
}

impl Analyzer for ArtifactLoss {
    fn id(&self) -> &'static str {
        "artifact-loss"
    }

    fn analyze(&self, trace: &Trace) -> Vec<Finding> {
        let mut findings = Vec::new();
        for turn in &trace.turns {
            for segment in &turn.segments {
                let Some(original) = segment.original.as_deref() else {
                    continue;
                };
                for artifact in extract::artifacts(original) {
                    if !segment.effective.contains(&artifact)
                        && referenced_later(trace, turn.index, &artifact)
                    {
                        findings.push(Finding {
                            class: ViolationClass::ArtifactLoss,
                            trace_id: trace.id.clone(),
                            turn: turn.index,
                            original: artifact.clone(),
                            effective: String::new(),
                            detail: format!("{artifact} was dropped and is referenced later"),
                        });
                    }
                }
            }
        }
        findings
    }
}

fn grouped(pairs: Vec<(String, String)>) -> std::collections::HashMap<String, Vec<String>> {
    let mut map: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for (key, value) in pairs {
        map.entry(key).or_default().push(value);
    }
    map
}

fn referenced_later(trace: &Trace, turn_index: usize, needle: &str) -> bool {
    trace
        .turns
        .iter()
        .filter(|t| t.index > turn_index)
        .flat_map(|t| t.segments.iter())
        .any(|s| s.effective.contains(needle))
}
