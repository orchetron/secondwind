use std::collections::HashMap;
use std::sync::Mutex;

use secondwind_optimize::relevance::Embedder;
use serde_json::{Value, json};

// Cosine-similarity weight, scaled to dominate the lexical base so the endpoint's
// ranking (not BM25) orders the rows.
const WEIGHT: f64 = 10.0;

// Relevance embedder backed by any standard /embeddings endpoint. Vectors cached by
// content, so a block re-sent every turn embeds once.
pub struct EndpointEmbedder {
    agent: ureq::Agent,
    url: String,
    model: String,
    api_key: Option<String>,
    cache: Mutex<HashMap<String, Vec<f32>>>,
}

impl EndpointEmbedder {
    pub fn new(url: String, model: String, api_key: Option<String>) -> Self {
        Self {
            agent: ureq::AgentBuilder::new().build(),
            url,
            model,
            api_key,
            cache: Mutex::new(HashMap::new()),
        }
    }

    // Unit-normalized embedding per text, from cache or one batched call for the misses.
    // None on any endpoint failure: relevance falls back to lexical, never errors the request.
    fn embed(&self, texts: &[&str]) -> Option<Vec<Vec<f32>>> {
        let missing: Vec<&str> = {
            let cache = self.cache.lock().unwrap();
            texts
                .iter()
                .copied()
                .filter(|t| !cache.contains_key(*t))
                .collect()
        };
        if !missing.is_empty() {
            let fetched = self.fetch(&missing)?;
            let mut cache = self.cache.lock().unwrap();
            for (text, vector) in missing.iter().zip(fetched) {
                cache.insert((*text).to_string(), vector);
            }
        }
        let cache = self.cache.lock().unwrap();
        texts.iter().map(|t| cache.get(*t).cloned()).collect()
    }

    fn fetch(&self, texts: &[&str]) -> Option<Vec<Vec<f32>>> {
        let mut request = self
            .agent
            .post(&self.url)
            .set("content-type", "application/json");
        if let Some(key) = &self.api_key {
            request = request.set("authorization", &format!("Bearer {key}"));
        }
        let body = json!({ "model": self.model, "input": texts });
        let response = request.send_json(body).ok()?;
        let parsed: Value = response.into_json().ok()?;
        let data = parsed.get("data")?.as_array()?;
        if data.len() != texts.len() {
            return None;
        }
        data.iter()
            .map(|entry| {
                let raw: Vec<f32> = entry
                    .get("embedding")?
                    .as_array()?
                    .iter()
                    .filter_map(|x| x.as_f64().map(|v| v as f32))
                    .collect();
                Some(normalize(raw))
            })
            .collect()
    }
}

impl Embedder for EndpointEmbedder {
    // A strong dense model ranks alone; the lexical base only degrades it.
    fn dominant(&self) -> bool {
        true
    }

    fn semantic(&self, query: &str, rows: &[&str]) -> Vec<f64> {
        let mut texts = Vec::with_capacity(rows.len() + 1);
        texts.push(query);
        texts.extend_from_slice(rows);
        let Some(vectors) = self.embed(&texts) else {
            return vec![0.0; rows.len()];
        };
        let query_vec = &vectors[0];
        vectors[1..]
            .iter()
            .map(|row| WEIGHT * dot(query_vec, row).max(0.0) as f64)
            .collect()
    }
}

fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Mock /embeddings endpoint: "buy"/"purchase" texts on one axis, everything else on
    // another; counts how many texts it was asked to embed.
    fn mock() -> (String, Arc<AtomicUsize>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let embedded = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&embedded);
        std::thread::spawn(move || {
            for mut request in server.incoming_requests() {
                let mut body = String::new();
                let _ = request.as_reader().read_to_string(&mut body);
                let parsed: Value = serde_json::from_str(&body).unwrap();
                let inputs = parsed["input"].as_array().unwrap();
                counter.fetch_add(inputs.len(), Ordering::SeqCst);
                let data: Vec<Value> = inputs
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let text = t.as_str().unwrap();
                        let e = if text.contains("purchase") || text.contains("buy") {
                            [1.0, 0.0]
                        } else {
                            [0.0, 1.0]
                        };
                        json!({"embedding": e, "index": i})
                    })
                    .collect();
                let header =
                    tiny_http::Header::from_bytes(&b"content-type"[..], &b"application/json"[..])
                        .unwrap();
                let _ = request.respond(
                    tiny_http::Response::from_string(json!({"data": data}).to_string())
                        .with_header(header),
                );
            }
        });
        (format!("http://{addr}"), embedded)
    }

    #[test]
    fn ranks_by_endpoint_cosine_and_caches() {
        let (url, embedded) = mock();
        let embedder = EndpointEmbedder::new(url, "m".into(), None);
        let rows = [
            "the purchase order was approved",
            "the weather is nice today",
        ];

        let scores = embedder.semantic("buy", &rows);
        assert!(
            scores[0] > scores[1],
            "the purchase row outranks the weather row"
        );
        assert_eq!(
            embedded.load(Ordering::SeqCst),
            3,
            "query plus two rows embedded once"
        );

        let _ = embedder.semantic("buy", &rows);
        assert_eq!(
            embedded.load(Ordering::SeqCst),
            3,
            "the second call is served from cache"
        );
    }
}
