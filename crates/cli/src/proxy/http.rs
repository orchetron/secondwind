use std::sync::Arc;

use axum::body::Body;
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use super::{AppState, detect_platform, pipeline};

// Max request body buffered before shaping; larger is refused rather than risk exhausting
// memory under concurrency. Generous for any real agent request.
const MAX_BODY: usize = 64 * 1024 * 1024;

// Hop-by-hop/length headers we never forward; the client's own accept-encoding, user-agent,
// and auth pass through verbatim so the request stays byte-faithful (no proxy fingerprint).
fn is_request_hop(name: &str) -> bool {
    matches!(name, "host" | "content-length" | "connection")
}

fn is_response_hop(name: &str) -> bool {
    // framing headers the client re-derives; everything else is the agent's to see.
    matches!(name, "content-length" | "transfer-encoding" | "connection")
}

// Forwards a normal request upstream and streams the reply straight back. The request body is
// shrunk first; the response path is a byte pipe, no buffering.
pub(crate) async fn forward(state: Arc<AppState>, parts: Parts, body: Body) -> Response {
    let body = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(body) => body.to_vec(),
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };

    let platform = detect_platform(&parts.headers, &state.rules);
    let tenant = super::detect_tenant(&parts.headers);
    let forwarded = pipeline::shape(state.clone(), platform, tenant, body).await;

    let path = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
    let url = format!("{}{}", state.upstream, path);
    let mut request = state.http.request(parts.method.clone(), &url);
    let mut headers = HeaderMap::new();
    for (name, value) in parts.headers.iter() {
        if !is_request_hop(name.as_str()) {
            headers.append(name.clone(), value.clone());
        }
    }
    request = request.headers(headers).body(forwarded);

    let response = match request.send().await {
        Ok(response) => response,
        Err(err) => {
            eprintln!("secondwind serve: upstream error: {err}");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };

    let mut out = Response::builder().status(response.status());
    for (name, value) in response.headers().iter() {
        if !is_response_hop(name.as_str()) {
            out = out.header(name, value);
        }
    }
    out.body(Body::from_stream(response.bytes_stream()))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}
