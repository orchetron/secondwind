use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};

use secondwind_core::{
    Billing, Origin, Party, Provenance, Role, Segment, SegmentKind, Trace, Turn,
};
use serde::Deserialize;

use crate::{ReadError, ReadOutcome, Source};

pub struct ClaudeCode;

const KNOWN_KINDS: &[&str] = &["assistant", "user", "system"];

// Transcripts nest (a project can hold its own projects tree), so discovery walks
// the whole subtree, not one level.
fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_jsonl(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        }
    }
    Ok(())
}

impl Source for ClaudeCode {
    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn search_root(&self, home: &Path) -> PathBuf {
        home.join(".claude").join("projects")
    }

    fn discover(&self, home: &Path) -> io::Result<Vec<PathBuf>> {
        let mut found = Vec::new();
        collect_jsonl(&self.search_root(home), &mut found)?;
        found.sort();
        Ok(found)
    }

    fn read(&self, path: &Path) -> Result<ReadOutcome, ReadError> {
        let file = fs::File::open(path)?;
        let reader = io::BufReader::new(file);
        let fallback_id = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".into());

        let mut outcome = ReadOutcome::default();
        let mut sessions: Vec<(String, Vec<Turn>)> = Vec::new();
        let mut tool_names: HashMap<String, String> = HashMap::new();
        let mut billed_requests: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for (line_no, line) in reader.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let envelope: Envelope = match serde_json::from_str(&line) {
                Ok(e) => e,
                Err(err) => {
                    let kind = serde_json::from_str::<KindOnly>(&line)
                        .map(|k| k.kind)
                        .unwrap_or_default();
                    if KNOWN_KINDS.contains(&kind.as_str()) {
                        return Err(ReadError::Drift {
                            source_id: self.id(),
                            path: path.to_path_buf(),
                            line: line_no + 1,
                            detail: format!("{kind} record no longer parses: {err}"),
                        });
                    }
                    *outcome
                        .skipped_record_types
                        .entry(if kind.is_empty() {
                            "unparseable".into()
                        } else {
                            kind
                        })
                        .or_insert(0) += 1;
                    continue;
                }
            };

            if !KNOWN_KINDS.contains(&envelope.kind.as_str()) {
                *outcome
                    .skipped_record_types
                    .entry(envelope.kind.clone())
                    .or_insert(0) += 1;
                continue;
            }

            let Some(message) = envelope.message else {
                continue;
            };

            let role = match envelope.kind.as_str() {
                "assistant" => Role::Assistant,
                "system" => Role::System,
                _ => Role::User,
            };

            let segments = message
                .content
                .map(|c| segments_from(c, &mut tool_names))
                .unwrap_or_default();
            if segments.is_empty() && message.usage.is_none() {
                continue;
            }

            let session_id = envelope.session_id.unwrap_or_else(|| fallback_id.clone());
            let turns = match sessions.iter_mut().find(|(id, _)| *id == session_id) {
                Some((_, turns)) => turns,
                None => {
                    sessions.push((session_id, Vec::new()));
                    &mut sessions.last_mut().expect("just pushed").1
                }
            };

            let billing = match (&envelope.request_id, message.usage) {
                (Some(request_id), Some(usage)) => {
                    if billed_requests.insert(request_id.clone()) {
                        Some(Billing::from(usage))
                    } else {
                        None
                    }
                }
                (None, usage) => usage.map(Billing::from),
                _ => None,
            };

            turns.push(Turn {
                index: turns.len(),
                role,
                timestamp: envelope.timestamp,
                model: message.model,
                sidechain: envelope.is_sidechain,
                segments,
                billing,
            });
        }

        outcome.traces = sessions
            .into_iter()
            .filter(|(_, turns)| !turns.is_empty())
            .map(|(id, turns)| Trace {
                id,
                source: self.id().into(),
                optimizer: None,
                provenance: Provenance {
                    origin: Origin::RealWork,
                    party: Party::FirstParty,
                },
                turns,
            })
            .collect();
        Ok(outcome)
    }
}

fn segments_from(content: Content, tool_names: &mut HashMap<String, String>) -> Vec<Segment> {
    let blocks = match content {
        Content::Text(text) => {
            if text.is_empty() {
                return Vec::new();
            }
            return vec![Segment {
                kind: SegmentKind::Text,
                original: None,
                effective: text,
            }];
        }
        Content::Blocks(blocks) => blocks,
    };

    let mut segments = Vec::new();
    for block in blocks {
        match block {
            Block::Text { text } => segments.push(Segment {
                kind: SegmentKind::Text,
                original: None,
                effective: text,
            }),
            Block::Thinking { thinking } => segments.push(Segment {
                kind: SegmentKind::Thinking,
                original: None,
                effective: thinking,
            }),
            Block::ToolUse { id, name, input } => {
                tool_names.insert(id.clone(), name.clone());
                segments.push(Segment {
                    kind: SegmentKind::ToolCall { name, id: Some(id) },
                    original: None,
                    effective: value_to_text(input),
                });
            }
            Block::ToolResult {
                tool_use_id,
                content,
            } => {
                let tool = tool_use_id
                    .as_deref()
                    .and_then(|id| tool_names.get(id).cloned())
                    .unwrap_or_else(|| "unknown".into());
                segments.push(Segment {
                    kind: SegmentKind::ToolResult {
                        tool,
                        id: tool_use_id,
                    },
                    original: None,
                    effective: value_to_text(content),
                });
            }
            Block::Unknown => {}
        }
    }
    segments
}

fn value_to_text(value: serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s,
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[derive(Deserialize)]
struct KindOnly {
    #[serde(rename = "type", default)]
    kind: String,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "requestId", default)]
    request_id: Option<String>,
    #[serde(rename = "sessionId", default)]
    session_id: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(rename = "isSidechain", default)]
    is_sidechain: bool,
    #[serde(default)]
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    content: Option<Content>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Blocks(Vec<Block>),
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Block {
    Text {
        #[serde(default)]
        text: String,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    ToolResult {
        #[serde(default)]
        tool_use_id: Option<String>,
        #[serde(default)]
        content: serde_json::Value,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_creation: Option<CacheCreation>,
}

#[derive(Deserialize)]
struct CacheCreation {
    #[serde(default)]
    ephemeral_5m_input_tokens: u64,
    #[serde(default)]
    ephemeral_1h_input_tokens: u64,
}

impl From<Usage> for Billing {
    fn from(usage: Usage) -> Self {
        let (w5m, w1h) = match usage.cache_creation {
            Some(c) => (c.ephemeral_5m_input_tokens, c.ephemeral_1h_input_tokens),
            None => (usage.cache_creation_input_tokens, 0),
        };
        Billing {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_input_tokens,
            cache_write_5m_tokens: w5m,
            cache_write_1h_tokens: w1h,
        }
    }
}
