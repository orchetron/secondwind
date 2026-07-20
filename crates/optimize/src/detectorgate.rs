use secondwind_analyzers::{Finding, all};
use secondwind_core::{Origin, Party, Provenance, Role, Segment, SegmentKind, Trace, Turn};

// An admitted transform passes this by construction; a fire is a decode bug.
pub fn detector_findings(original: &str, wire: &str) -> Vec<Finding> {
    let trace = Trace {
        id: "gate".into(),
        source: "optimizer".into(),
        optimizer: None,
        provenance: Provenance {
            origin: Origin::Synthetic,
            party: Party::FirstParty,
        },
        turns: vec![Turn {
            index: 0,
            role: Role::User,
            timestamp: None,
            model: None,
            sidechain: false,
            segments: vec![Segment {
                kind: SegmentKind::ToolResult {
                    tool: "optimized".into(),
                    id: None,
                },
                original: Some(original.to_string()),
                effective: wire.to_string(),
            }],
            billing: None,
        }],
    };
    all().iter().flat_map(|a| a.analyze(&trace)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faithful_grep_survives() {
        let original = "src/a.rs:42:let x = 1;\nsrc/a.rs:50:fn main() {";
        assert!(detector_findings(original, original).is_empty());
    }

    #[test]
    fn a_forged_reconstruction_is_caught() {
        let original = "src/a.rs:42:let x = 1;";
        let forged = "src/a.rs:42:let x = 999;";
        assert!(!detector_findings(original, forged).is_empty());
    }
}
