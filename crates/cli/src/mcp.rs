use std::io::{BufRead, Write};
use std::path::Path;
use std::process::ExitCode;

use secondwind_ledger::events;
use secondwind_optimize::tokens::{Tiktoken, TokenCounter};
use secondwind_optimize::{Optimizer, Outcome};
use serde_json::{Value, json};

const PROTOCOL_VERSION: &str = "2024-11-05";

// Minimal MCP stdio server exposing the optimizer as two tools: `optimize` (compress a
// block, optionally query-aware) and `resolve` (fetch an offloaded block back). One
// long-lived Optimizer holds the offload store across calls; real tokenizer = billed unit.
pub fn serve(home: &Path) -> ExitCode {
    let counter = std::sync::Arc::new(Tiktoken::cl100k());
    let model = crate::configured_model();
    let mut optimizer = Optimizer::default()
        .with_counter(counter.clone())
        .with_model(&model)
        .with_store(secondwind_optimize::offload::Store::persistent(
            crate::proxy::store_dir(home),
            crate::proxy::OFFLOAD_TTL,
        ));
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = request.get("id").cloned();
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");

        let response = match method {
            "initialize" => Some(result(
                id,
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "secondwind", "version": env!("CARGO_PKG_VERSION") },
                }),
            )),
            "tools/list" => Some(result(id, json!({ "tools": tool_defs() }))),
            "tools/call" => Some(handle_call(
                id,
                &request,
                &mut optimizer,
                counter.as_ref(),
                home,
            )),
            _ if id.is_some() => Some(result(id, json!({}))),
            _ => None,
        };
        if let Some(response) = response {
            let _ = writeln!(stdout, "{response}");
            let _ = stdout.flush();
        }
    }
    ExitCode::SUCCESS
}

fn tool_defs() -> Vec<Value> {
    vec![
        json!({
            "name": "optimize",
            "description": "Losslessly compress a block of tool output. Values stay present \
                inline or recoverable via resolve; nothing is dropped.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "block": { "type": "string", "description": "The tool output to compress." },
                    "query": { "type": "string", "description": "Optional request text; relevant rows are kept inline." }
                },
                "required": ["block"]
            }
        }),
        json!({
            "name": "resolve",
            "description": "Fetch the full original content behind an offload marker of the \
                form <<swload:...>>.",
            "inputSchema": {
                "type": "object",
                "properties": { "marker": { "type": "string" } },
                "required": ["marker"]
            }
        }),
    ]
}

fn handle_call(
    id: Option<Value>,
    request: &Value,
    optimizer: &mut Optimizer,
    counter: &Tiktoken,
    home: &std::path::Path,
) -> Value {
    let params = &request["params"];
    let name = params["name"].as_str().unwrap_or("");
    let args = &params["arguments"];

    match name {
        "optimize" => {
            let block = args["block"].as_str().unwrap_or("");
            let outcome = match args["query"].as_str() {
                Some(query) if !query.is_empty() => {
                    optimizer.compress_block_with_query(block, query)
                }
                _ => optimizer.compress_block(block),
            };
            let (text, transform, saved_usd, inline) = match outcome {
                Outcome::Compressed {
                    wire,
                    transform,
                    saved_usd,
                    ..
                } => (wire, transform, saved_usd, true),
                Outcome::Offloaded {
                    stub, saved_usd, ..
                } => (stub, "offload", saved_usd, false),
                Outcome::KeptVerbatim { .. } => (block.to_string(), "verbatim", 0.0, true),
            };
            if transform != "verbatim" {
                let (atoms, cert) = secondwind_optimize::proof(block);
                let model = optimizer.model().to_string();
                events::record(
                    home,
                    &events::Event {
                        at_ms: events::now_ms(),
                        surface: "mcp".into(),
                        transform: transform.into(),
                        input_tokens: counter.count(block) as u64,
                        output_tokens: counter.count(&text) as u64,
                        saved_usd,
                        verified: secondwind_optimize::certificate::verify(
                            &text,
                            &secondwind_optimize::certificate::certify(block),
                        ),
                        inline,
                        atoms,
                        cert,
                        model: model.clone(),
                        platform: "mcp".into(),
                        tenant: String::new(),
                        kept_reason: String::new(),
                        req_id: String::new(),
                    },
                );
            }
            result(id, content(&text))
        }
        "resolve" => {
            let marker = args["marker"].as_str().unwrap_or("");
            match optimizer.resolve(marker) {
                Some(body) => result(id, content(&body)),
                None => result(id, error_content("marker not found or expired")),
            }
        }
        _ => result(id, error_content("unknown tool")),
    }
}

fn content(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }] })
}

fn error_content(message: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": message }], "isError": true })
}

fn result(id: Option<Value>, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}
