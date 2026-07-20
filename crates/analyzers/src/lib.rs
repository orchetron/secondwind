#![forbid(unsafe_code)]

use std::fmt;

use secondwind_core::Trace;
use serde::{Deserialize, Serialize};

pub mod extract;

mod detectors;
pub use detectors::{ArtifactLoss, Fabrication, NumericDrift};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ViolationClass {
    Fabrication,
    NumericDrift,
    ArtifactLoss,
}

impl fmt::Display for ViolationClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            ViolationClass::Fabrication => "fabrication",
            ViolationClass::NumericDrift => "numeric-drift",
            ViolationClass::ArtifactLoss => "artifact-loss",
        };
        f.write_str(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub class: ViolationClass,
    pub trace_id: String,
    pub turn: usize,
    pub original: String,
    pub effective: String,
    pub detail: String,
}

pub trait Analyzer {
    fn id(&self) -> &'static str;
    fn analyze(&self, trace: &Trace) -> Vec<Finding>;
}

pub fn all() -> Vec<Box<dyn Analyzer>> {
    vec![
        Box::new(Fabrication),
        Box::new(NumericDrift),
        Box::new(ArtifactLoss),
    ]
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Retention {
    pub numerics_total: u64,
    pub numerics_kept: u64,
    pub artifacts_total: u64,
    pub artifacts_kept: u64,
    pub big_drop_segments: u64,
    pub worst_drop_percent: u8,
}

const BIG_DROP_THRESHOLD_PERCENT: u64 = 50;
const BIG_DROP_MIN_ITEMS: u64 = 10;

impl Retention {
    pub fn add_trace(&mut self, trace: &Trace) {
        for turn in &trace.turns {
            for segment in &turn.segments {
                let Some(original) = segment.original.as_deref() else {
                    continue;
                };
                let mut seg_total = 0u64;
                let mut seg_kept = 0u64;
                for value in extract::numbers_all(original) {
                    self.numerics_total += 1;
                    seg_total += 1;
                    if segment.effective.contains(&value) {
                        self.numerics_kept += 1;
                        seg_kept += 1;
                    }
                }
                for artifact in extract::artifacts_all(original) {
                    self.artifacts_total += 1;
                    seg_total += 1;
                    if segment.effective.contains(&artifact) {
                        self.artifacts_kept += 1;
                        seg_kept += 1;
                    }
                }
                if seg_total >= BIG_DROP_MIN_ITEMS {
                    let dropped = seg_total - seg_kept;
                    let drop_percent = dropped * 100 / seg_total;
                    if drop_percent >= BIG_DROP_THRESHOLD_PERCENT {
                        self.big_drop_segments += 1;
                        self.worst_drop_percent = self.worst_drop_percent.max(drop_percent as u8);
                    }
                }
            }
        }
    }

    pub fn observed(&self) -> bool {
        self.numerics_total + self.artifacts_total > 0
    }
}
