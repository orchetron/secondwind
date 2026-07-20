use std::sync::Arc;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as Ts;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::frame::CloseFrame as TsClose;

use super::AppState;

// Handshake headers we regenerate for the upstream dial; everything else the client sent
// (auth, account id, betas, user-agent, origin, attestation) is forwarded verbatim so the
// upstream sees the agent's own handshake.
fn is_handshake_header(name: &str) -> bool {
    matches!(
        name,
        "host"
            | "connection"
            | "upgrade"
            | "content-length"
            | "sec-websocket-key"
            | "sec-websocket-version"
            | "sec-websocket-extensions"
            | "sec-websocket-protocol"
    )
}

// https base -> wss + client path, mirroring the HTTP forward's {upstream}{path} join so both
// transports resolve the same endpoint.
fn ws_upstream(base: &str, path: &str) -> String {
    let base = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    };
    format!("{base}{path}")
}

fn requested_protocols(headers: &HeaderMap) -> Vec<String> {
    headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .map(|raw| {
            raw.split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn upstream_headers(headers: &HeaderMap) -> Vec<(HeaderName, HeaderValue)> {
    headers
        .iter()
        .filter(|(name, _)| !is_handshake_header(name.as_str()))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

// Accepts the client upgrade, echoing back any subprotocol it asked for, then relays.
pub(crate) fn handle(upgrade: WebSocketUpgrade, parts: Parts, state: Arc<AppState>) -> Response {
    let path = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/")
        .to_string();
    let ws_url = ws_upstream(&state.upstream, &path);
    let headers = upstream_headers(&parts.headers);
    let protocols = requested_protocols(&parts.headers);
    let platform = super::detect_platform(&parts.headers, &state.rules);
    let tenant = super::detect_tenant(&parts.headers);
    upgrade
        .protocols(protocols.clone())
        .on_upgrade(move |client| {
            relay(client, ws_url, headers, protocols, state, platform, tenant)
        })
}

fn build_request(
    ws_url: &str,
    headers: &[(HeaderName, HeaderValue)],
    protocols: &[String],
) -> Result<
    tokio_tungstenite::tungstenite::handshake::client::Request,
    tokio_tungstenite::tungstenite::Error,
> {
    let mut request = ws_url.into_client_request()?;
    let out = request.headers_mut();
    for (name, value) in headers {
        out.insert(name.clone(), value.clone());
    }
    if !protocols.is_empty()
        && let Ok(value) = HeaderValue::from_str(&protocols.join(", "))
    {
        out.insert("sec-websocket-protocol", value);
    }
    Ok(request)
}

// Dials the upstream WebSocket and pumps frames both ways until either side closes; the
// client-to-upstream direction shapes request frames, the reverse is a verbatim passthrough.
async fn relay(
    client: WebSocket,
    ws_url: String,
    headers: Vec<(HeaderName, HeaderValue)>,
    protocols: Vec<String>,
    state: Arc<AppState>,
    platform: String,
    tenant: String,
) {
    let request = match build_request(&ws_url, &headers, &protocols) {
        Ok(request) => request,
        Err(err) => {
            eprintln!("secondwind serve: ws request: {err}");
            return;
        }
    };
    let (upstream, _) = match connect_async(request).await {
        Ok(pair) => pair,
        Err(err) => {
            eprintln!("secondwind serve: ws upstream: {err}");
            return;
        }
    };

    let (mut client_tx, mut client_rx) = client.split();
    let (mut upstream_tx, mut upstream_rx) = upstream.split();

    // Client -> upstream runs as its own task so a client half-close never cancels the response
    // still streaming below; the relay ends when the upstream closes the turn.
    let debug = std::env::var_os("SECONDWIND_WS_DEBUG").is_some();
    let to_upstream = tokio::spawn(async move {
        while let Some(Ok(message)) = client_rx.next().await {
            if debug {
                trace_frame(&message);
            }
            let outbound = shape_outbound(message, &state, &platform, &tenant).await;
            if upstream_tx.send(outbound).await.is_err() {
                break;
            }
        }
        let _ = upstream_tx.close().await;
    });

    while let Some(Ok(message)) = upstream_rx.next().await {
        let Some(message) = to_axum(message) else {
            continue;
        };
        if client_tx.send(message).await.is_err() {
            break;
        }
    }
    let _ = client_tx.close().await;
    to_upstream.abort();
}

// SECONDWIND_WS_DEBUG only: previews each client-to-upstream frame's envelope and content.
fn trace_frame(message: &Message) {
    match message {
        Message::Text(text) => {
            let text = text.as_str();
            let kind = serde_json::from_str::<serde_json::Value>(text)
                .ok()
                .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_string))
                .unwrap_or_else(|| "text".to_string());
            let preview: String = text.chars().take(360).collect();
            eprintln!(
                "secondwind ws frame: type={kind} bytes={} {preview}",
                text.len()
            );
            if let Some(path) = std::env::var_os("SECONDWIND_WS_DUMP") {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                {
                    let _ = writeln!(f, "{text}");
                }
            }
        }
        Message::Binary(data) => eprintln!("secondwind ws frame: binary bytes={}", data.len()),
        Message::Ping(_) => eprintln!("secondwind ws frame: ping"),
        Message::Pong(_) => eprintln!("secondwind ws frame: pong"),
        Message::Close(_) => eprintln!("secondwind ws frame: close"),
    }
}

// Shapes a text frame through the same pipeline as HTTP; other frame types pass straight through.
async fn shape_outbound(
    message: Message,
    state: &Arc<AppState>,
    platform: &str,
    tenant: &str,
) -> Ts {
    if let Message::Text(text) = &message {
        let shaped = super::pipeline::shape(
            state.clone(),
            platform.to_string(),
            tenant.to_string(),
            text.as_str().as_bytes().to_vec(),
        )
        .await;
        let shaped = String::from_utf8(shaped)
            .unwrap_or_else(|err| String::from_utf8_lossy(err.as_bytes()).into_owned());
        return Ts::text(shaped);
    }
    to_tungstenite(message)
}

fn to_tungstenite(message: Message) -> Ts {
    match message {
        Message::Text(text) => Ts::text(text.as_str()),
        Message::Binary(data) => Ts::Binary(data),
        Message::Ping(data) => Ts::Ping(data),
        Message::Pong(data) => Ts::Pong(data),
        Message::Close(frame) => Ts::Close(frame.map(|frame| TsClose {
            code: frame.code.into(),
            reason: frame.reason.as_str().into(),
        })),
    }
}

fn to_axum(message: Ts) -> Option<Message> {
    Some(match message {
        Ts::Text(text) => Message::Text(text.as_str().into()),
        Ts::Binary(data) => Message::Binary(data),
        Ts::Ping(data) => Message::Ping(data),
        Ts::Pong(data) => Message::Pong(data),
        Ts::Close(frame) => Message::Close(frame.map(|frame| CloseFrame {
            code: frame.code.into(),
            reason: frame.reason.as_str().into(),
        })),
        // Raw frames are never surfaced by the read half, per the tungstenite maintainers.
        Ts::Frame(_) => return None,
    })
}
