#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use secondwind_core::{SegmentKind, Trace};
use secondwind_sources::Enricher;
use serde::{Deserialize, Serialize};

const SNIFF_LIMIT: usize = 65536;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedToolResult {
    pub tool_use_id: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureRecord {
    pub captured_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    pub tool_results: Vec<CapturedToolResult>,
}

pub fn tool_results_of(body: &serde_json::Value) -> Vec<CapturedToolResult> {
    let mut results = Vec::new();
    let Some(messages) = body.get("messages").and_then(|m| m.as_array()) else {
        return results;
    };
    for message in messages {
        let Some(blocks) = message.get("content").and_then(|c| c.as_array()) else {
            continue;
        };
        for block in blocks {
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                continue;
            }
            let Some(id) = block.get("tool_use_id").and_then(|i| i.as_str()) else {
                continue;
            };
            results.push(CapturedToolResult {
                tool_use_id: id.to_string(),
                content: block_content_text(block.get("content")),
            });
        }
    }
    results
}

fn block_content_text(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

pub fn scan_message_id(head: &[u8]) -> Option<String> {
    let needle = b"\"id\":\"msg_";
    let pos = head.windows(needle.len()).position(|w| w == needle)?;
    let start = pos + "\"id\":\"".len();
    let rest = &head[start..];
    let end = rest.iter().position(|b| *b == b'"')?;
    String::from_utf8(rest[..end].to_vec()).ok()
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub struct CaptureWriter {
    file: Mutex<fs::File>,
}

impl CaptureWriter {
    pub fn create(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join(format!("capture-{}.jsonl", epoch_ms()));
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    pub fn append(&self, record: &CaptureRecord) {
        let Ok(line) = serde_json::to_string(record) else {
            return;
        };
        if let Ok(mut file) = self.file.lock() {
            let _ = writeln!(file, "{line}");
        }
    }
}

pub struct CaptureLog {
    effective_by_tool_use: HashMap<String, String>,
}

impl CaptureLog {
    pub fn default_dir(home: &Path) -> PathBuf {
        home.join(".secondwind").join("capture")
    }

    pub fn load(dir: &Path) -> io::Result<Self> {
        let mut effective_by_tool_use = HashMap::new();
        if dir.is_dir() {
            for entry in fs::read_dir(dir)? {
                let path = entry?.path();
                if path.extension().is_none_or(|e| e != "jsonl") {
                    continue;
                }
                for line in fs::read_to_string(&path)?.lines() {
                    let Ok(record) = serde_json::from_str::<CaptureRecord>(line) else {
                        continue;
                    };
                    for result in record.tool_results {
                        effective_by_tool_use.insert(result.tool_use_id, result.content);
                    }
                }
            }
        }
        Ok(Self {
            effective_by_tool_use,
        })
    }

    pub fn len(&self) -> usize {
        self.effective_by_tool_use.len()
    }

    pub fn is_empty(&self) -> bool {
        self.effective_by_tool_use.is_empty()
    }
}

impl Enricher for CaptureLog {
    fn id(&self) -> &'static str {
        "capture-tap"
    }

    fn enrich(&self, trace: &mut Trace) -> usize {
        let mut enriched = 0;
        for turn in &mut trace.turns {
            for segment in &mut turn.segments {
                if segment.original.is_some() {
                    continue;
                }
                let SegmentKind::ToolResult { id: Some(id), .. } = &segment.kind else {
                    continue;
                };
                let Some(on_wire) = self.effective_by_tool_use.get(id) else {
                    continue;
                };
                if on_wire != &segment.effective {
                    segment.original =
                        Some(std::mem::replace(&mut segment.effective, on_wire.clone()));
                    enriched += 1;
                }
            }
        }
        enriched
    }
}

pub struct TapConfig {
    pub listen: String,
    pub upstream: String,
    pub capture_dir: PathBuf,
}

pub fn serve(config: TapConfig) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = tiny_http::Server::http(&config.listen)?;
    eprintln!(
        "secondwind tap listening on {} \u{2192} {}",
        config.listen, config.upstream
    );
    serve_on(server, &config.upstream, &config.capture_dir)
}

pub fn serve_on(
    server: tiny_http::Server,
    upstream: &str,
    capture_dir: &Path,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let writer = Arc::new(CaptureWriter::create(capture_dir)?);
    let agent = ureq::AgentBuilder::new().build();
    let upstream = upstream.trim_end_matches('/').to_string();

    for mut request in server.incoming_requests() {
        let mut body = Vec::new();
        if request.as_reader().read_to_end(&mut body).is_err() {
            let _ = request.respond(tiny_http::Response::empty(400));
            continue;
        }

        let record = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .map(|json| CaptureRecord {
                captured_at_ms: epoch_ms(),
                model: json
                    .get("model")
                    .and_then(|m| m.as_str())
                    .map(str::to_string),
                message_id: None,
                tool_results: tool_results_of(&json),
            });

        let url = format!("{upstream}{}", request.url());
        let mut forward = agent.request(request.method().as_str(), &url);
        for header in request.headers() {
            let name = header.field.as_str().as_str();
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "host" | "content-length" | "connection" | "accept-encoding"
            ) {
                continue;
            }
            forward = forward.set(name, header.value.as_str());
        }

        let upstream_response = match forward.send_bytes(&body) {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(err) => {
                eprintln!("secondwind tap: upstream error: {err}");
                let _ = request.respond(tiny_http::Response::empty(502));
                continue;
            }
        };

        let status = upstream_response.status();
        let mut headers = Vec::new();
        for name in ["content-type", "anthropic-request-id"] {
            if let Some(value) = upstream_response.header(name)
                && let Ok(h) = tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes())
            {
                headers.push(h);
            }
        }

        let tee = Tee {
            inner: upstream_response.into_reader(),
            sniff: Vec::new(),
            record,
            writer: Arc::clone(&writer),
        };
        let response = tiny_http::Response::new(status.into(), headers, tee, None, None);
        let _ = request.respond(response);
    }
    Ok(())
}

struct Tee {
    inner: Box<dyn Read + Send + Sync + 'static>,
    sniff: Vec<u8>,
    record: Option<CaptureRecord>,
    writer: Arc<CaptureWriter>,
}

impl Read for Tee {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if self.sniff.len() < SNIFF_LIMIT {
            let take = (SNIFF_LIMIT - self.sniff.len()).min(n);
            self.sniff.extend_from_slice(&buf[..take]);
        }
        Ok(n)
    }
}

impl Drop for Tee {
    fn drop(&mut self) {
        if let Some(mut record) = self.record.take() {
            record.message_id = scan_message_id(&self.sniff);
            self.writer.append(&record);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_tool_results_from_request_body() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-5",
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_9", "content": "src/a.rs:1:x"},
                    {"type": "tool_result", "tool_use_id": "toolu_10", "content": [
                        {"type": "text", "text": "line one"},
                        {"type": "text", "text": "line two"}
                    ]}
                ]}
            ]
        });
        let results = tool_results_of(&body);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].tool_use_id, "toolu_9");
        assert_eq!(results[1].content, "line one\nline two");
    }

    #[test]
    fn scans_message_id_from_json_and_sse() {
        assert_eq!(
            scan_message_id(br#"{"id":"msg_abc123","type":"message"}"#).as_deref(),
            Some("msg_abc123")
        );
        let sse = br#"event: message_start
data: {"type":"message_start","message":{"id":"msg_stream1","role":"assistant"}}"#;
        assert_eq!(scan_message_id(sse).as_deref(), Some("msg_stream1"));
        assert_eq!(scan_message_id(b"nothing here"), None);
    }
}
