#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trace {
    pub id: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optimizer: Option<String>,
    pub provenance: Provenance,
    pub turns: Vec<Turn>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub origin: Origin,
    pub party: Party,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    RealWork,
    Synthetic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Party {
    FirstParty,
    Donated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub index: usize,
    pub role: Role,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sidechain: bool,
    pub segments: Vec<Segment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing: Option<Billing>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub kind: SegmentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original: Option<String>,
    pub effective: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "type")]
pub enum SegmentKind {
    Text,
    Thinking,
    ToolCall {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    ToolResult {
        tool: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Billing {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_5m_tokens: u64,
    pub cache_write_1h_tokens: u64,
}

impl Billing {
    pub fn cache_write_tokens(&self) -> u64 {
        self.cache_write_5m_tokens + self.cache_write_1h_tokens
    }
}

impl Segment {
    pub fn is_modified(&self) -> bool {
        self.original
            .as_deref()
            .is_some_and(|o| o != self.effective)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_round_trips_through_json() {
        let trace = Trace {
            id: "t1".into(),
            source: "claude-code".into(),
            optimizer: None,
            provenance: Provenance {
                origin: Origin::RealWork,
                party: Party::FirstParty,
            },
            turns: vec![Turn {
                index: 0,
                role: Role::User,
                timestamp: Some("2026-07-16T19:00:00Z".into()),
                model: None,
                sidechain: false,
                segments: vec![Segment {
                    kind: SegmentKind::ToolResult {
                        tool: "grep".into(),
                        id: Some("toolu_1".into()),
                    },
                    original: Some("src/main.rs:42:let x = 1;".into()),
                    effective: "src/main.rs:42".into(),
                }],
                billing: Some(Billing {
                    input_tokens: 10,
                    output_tokens: 2,
                    cache_read_tokens: 100,
                    cache_write_5m_tokens: 0,
                    cache_write_1h_tokens: 50,
                }),
            }],
        };

        let json = serde_json::to_string(&trace).unwrap();
        let back: Trace = serde_json::from_str(&json).unwrap();

        assert_eq!(trace, back);
        assert!(trace.turns[0].segments[0].is_modified());
        assert_eq!(trace.turns[0].billing.unwrap().cache_write_tokens(), 50);
    }
}
