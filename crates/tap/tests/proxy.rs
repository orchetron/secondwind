use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use secondwind_core::{Origin, Party, Provenance, Role, Segment, SegmentKind, Trace, Turn};
use secondwind_sources::Enricher;
use secondwind_tap::{CaptureLog, CaptureRecord, serve_on};

fn mock_upstream() -> (String, thread::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut received = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = stream.read(&mut buf).unwrap();
            received.extend_from_slice(&buf[..n]);
            if let Some(header_end) = find(&received, b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&received[..header_end]).to_lowercase();
                let content_length: usize = headers
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                if received.len() >= header_end + 4 + content_length {
                    break;
                }
            }
        }
        let body = br#"{"id":"msg_test123","type":"message","content":[]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
        stream.write_all(body).unwrap();
        received
    });
    (format!("http://{addr}"), handle)
}

fn capture_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("secondwind-tap-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[test]
fn proxies_and_captures_tool_results() {
    let (upstream, upstream_handle) = mock_upstream();
    let dir = capture_dir();
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let tap_addr = server.server_addr().to_string();
    let dir_for_server = dir.clone();
    thread::spawn(move || {
        let _ = serve_on(server, &upstream, &dir_for_server);
    });

    let request_body = serde_json::json!({
        "model": "claude-sonnet-4-5",
        "messages": [{"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "toolu_tap1", "content": "src/a.rs:42:let x = 2;"}
        ]}]
    })
    .to_string();

    let response = ureq::AgentBuilder::new()
        .build()
        .post(&format!("http://{tap_addr}/v1/messages"))
        .set("content-type", "application/json")
        .set("x-api-key", "test-key-not-real")
        .send_string(&request_body)
        .unwrap();
    assert_eq!(response.status(), 200);
    let response_text = response.into_string().unwrap();
    let response_json: serde_json::Value = serde_json::from_str(&response_text).unwrap();
    assert_eq!(response_json["id"], "msg_test123");

    let forwarded = upstream_handle.join().unwrap();
    let forwarded_text = String::from_utf8_lossy(&forwarded);
    assert!(forwarded_text.contains("toolu_tap1"));
    assert!(forwarded_text.contains("x-api-key: test-key-not-real"));

    let mut log = None;
    for _ in 0..40 {
        let loaded = CaptureLog::load(&dir).unwrap();
        if !loaded.is_empty() {
            log = Some(loaded);
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let log = log.expect("capture record written");
    assert_eq!(log.len(), 1);

    let raw = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| std::fs::read_to_string(e.unwrap().path()).ok())
        .collect::<String>();
    let record: CaptureRecord = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
    assert_eq!(record.message_id.as_deref(), Some("msg_test123"));
    assert_eq!(record.model.as_deref(), Some("claude-sonnet-4-5"));
    assert!(!raw.contains("test-key-not-real"));

    let mut trace = Trace {
        id: "s-tap".into(),
        source: "claude-code".into(),
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
                    tool: "Grep".into(),
                    id: Some("toolu_tap1".into()),
                },
                original: None,
                effective: "src/a.rs:42:let x = 1;".into(),
            }],
            billing: None,
        }],
    };
    assert_eq!(log.enrich(&mut trace), 1);
    let segment = &trace.turns[0].segments[0];
    assert_eq!(segment.original.as_deref(), Some("src/a.rs:42:let x = 1;"));
    assert_eq!(segment.effective, "src/a.rs:42:let x = 2;");

    let findings: Vec<_> = secondwind_analyzers::all()
        .iter()
        .flat_map(|a| a.analyze(&trace))
        .collect();
    assert!(
        findings
            .iter()
            .any(|f| f.class == secondwind_analyzers::ViolationClass::Fabrication)
    );

    let _ = std::fs::remove_dir_all(&dir);
}
