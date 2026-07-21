// A minimal, fast mock model API for load-testing the proxy without a slow upstream capping
// throughput. Answers Anthropic Messages and OpenAI Chat Completions with a canned 200.
//   cargo run -p secondwind --example mock_upstream --release
use axum::{Json, Router, routing::post};
use serde_json::{Value, json};

async fn anthropic() -> Json<Value> {
    Json(json!({
        "id": "msg_bench", "type": "message", "role": "assistant", "model": "bench",
        "content": [{"type": "text", "text": "ok"}], "stop_reason": "end_turn",
        "usage": {"input_tokens": 10, "output_tokens": 2}
    }))
}

async fn openai() -> Json<Value> {
    Json(json!({
        "id": "chatcmpl-bench", "object": "chat.completion", "model": "bench",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 2}
    }))
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/v1/messages", post(anthropic))
        .route("/v1/chat/completions", post(openai));
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:9099".into());
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    eprintln!("mock upstream on {addr}");
    axum::serve(listener, app).await.unwrap();
}
