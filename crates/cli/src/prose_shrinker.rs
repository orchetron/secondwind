use std::collections::HashMap;
use std::sync::Mutex;

use secondwind_optimize::prose::{ProseShrinker, Span};
use serde_json::{Value, json};

// Extractive keep/drop endpoint: returns which spans to keep, cached by content.
pub struct EndpointShrinker {
    agent: ureq::Agent,
    url: String,
    model: String,
    api_key: Option<String>,
    cache: Mutex<HashMap<String, Vec<(usize, usize)>>>,
}

impl EndpointShrinker {
    pub fn new(url: String, model: String, api_key: Option<String>) -> Self {
        Self {
            agent: ureq::AgentBuilder::new().build(),
            url,
            model,
            api_key,
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn fetch(&self, text: &str) -> Option<Vec<(usize, usize)>> {
        let mut request = self
            .agent
            .post(&self.url)
            .set("content-type", "application/json");
        if let Some(key) = &self.api_key {
            request = request.set("authorization", &format!("Bearer {key}"));
        }
        let response = request
            .send_json(json!({ "model": self.model, "input": text }))
            .ok()?;
        parse_spans(&response.into_json().ok()?)
    }
}

impl ProseShrinker for EndpointShrinker {
    fn keep(&self, text: &str) -> Option<Vec<Span>> {
        if let Some(hit) = self.cache.lock().unwrap().get(text) {
            return Some(to_spans(hit));
        }
        let pairs = self.fetch(text)?;
        self.cache
            .lock()
            .unwrap()
            .insert(text.to_string(), pairs.clone());
        Some(to_spans(&pairs))
    }
}

fn to_spans(pairs: &[(usize, usize)]) -> Vec<Span> {
    pairs
        .iter()
        .map(|&(start, end)| Span { start, end })
        .collect()
}

// Keep-spans as keep:[[s,e]] or spans/tokens:[{start,end}]; the shrink validates bounds.
fn parse_spans(parsed: &Value) -> Option<Vec<(usize, usize)>> {
    if let Some(pairs) = parsed.get("keep").and_then(Value::as_array) {
        return Some(pairs.iter().filter_map(pair).collect());
    }
    for field in ["spans", "tokens"] {
        if let Some(items) = parsed.get(field).and_then(Value::as_array) {
            return Some(items.iter().filter_map(object_span).collect());
        }
    }
    None
}

fn pair(v: &Value) -> Option<(usize, usize)> {
    let a = v.as_array()?;
    Some((a.first()?.as_u64()? as usize, a.get(1)?.as_u64()? as usize))
}

fn object_span(v: &Value) -> Option<(usize, usize)> {
    Some((
        v.get("start")?.as_u64()? as usize,
        v.get("end")?.as_u64()? as usize,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn mock(spans: Value) -> (String, Arc<AtomicUsize>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        std::thread::spawn(move || {
            for request in server.incoming_requests() {
                counter.fetch_add(1, Ordering::SeqCst);
                let header =
                    tiny_http::Header::from_bytes(&b"content-type"[..], &b"application/json"[..])
                        .unwrap();
                let _ = request.respond(
                    tiny_http::Response::from_string(spans.to_string()).with_header(header),
                );
            }
        });
        (format!("http://{addr}"), calls)
    }

    #[test]
    fn returns_keep_spans_and_caches_by_content() {
        let (url, calls) = mock(json!({ "keep": [[0, 4], [10, 20]] }));
        let shrinker = EndpointShrinker::new(url, "m".into(), None);

        let spans = shrinker.keep("some prose block text here").unwrap();
        assert_eq!(spans.len(), 2);
        assert_eq!((spans[0].start, spans[0].end), (0, 4));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let _ = shrinker.keep("some prose block text here");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "second call served from cache"
        );
    }

    #[test]
    fn parses_object_span_shape() {
        let spans = parse_spans(&json!({ "tokens": [{"start": 2, "end": 7}] })).unwrap();
        assert_eq!(spans, vec![(2, 7)]);
    }

    #[test]
    fn none_when_the_endpoint_is_unreachable() {
        let shrinker = EndpointShrinker::new("http://127.0.0.1:9/nope".into(), "m".into(), None);
        assert!(shrinker.keep("text").is_none());
    }
}
