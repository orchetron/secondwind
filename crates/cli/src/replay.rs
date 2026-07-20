use std::path::Path;
use std::process::ExitCode;

use secondwind_core::Trace;
use secondwind_optimize::replay::{
    Changed, Model, Reply, Request, context_savings, decisions, dry_run_tokens, run as replay_run,
};
use serde_json::{Value, json};

const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_URL: &str = "https://api.anthropic.com/v1/messages";
const RESOLVE_TOOL: &str = "secondwind_resolve";
const MAX_HOPS: usize = 4;
const OUTPUT_TOKENS_PER_REPLY: u64 = 200;

pub fn run(
    home: &Path,
    decisions_target: usize,
    model: Option<&str>,
    dry_run: bool,
    max_spend: f64,
) -> ExitCode {
    let Some(model) = model
        .map(str::to_string)
        .or_else(|| std::env::var("SECONDWIND_MODEL").ok())
    else {
        eprintln!("secondwind replay: pass --model or set SECONDWIND_MODEL");
        return ExitCode::FAILURE;
    };

    let traces = gather(home, decisions_target);
    let (n, input_tokens) = dry_run_tokens(&traces, decisions_target);
    if n == 0 {
        eprintln!("secondwind replay: no decisions found in discovered traces");
        return ExitCode::FAILURE;
    }
    let output_tokens = n * 2 * OUTPUT_TOKENS_PER_REPLY;
    let projected = cost(&model, input_tokens, output_tokens);

    let (uncompressed, compressed) = context_savings(&traces, decisions_target);
    let savings = if uncompressed == 0 {
        0.0
    } else {
        100.0 * (1.0 - compressed as f64 / uncompressed as f64)
    };
    println!("decisions: {n}");
    println!("context tokens: {uncompressed} -> {compressed} ({savings:.1}% saved)");
    println!("est input tokens: {input_tokens}");
    match projected {
        Some(usd) => println!("projected cost ({model}): ${usd:.2}"),
        None => println!("projected cost: unknown (no rate for {model})"),
    }

    if dry_run {
        return ExitCode::SUCCESS;
    }
    if let Some(usd) = projected
        && usd > max_spend
    {
        eprintln!(
            "secondwind replay: projected ${usd:.2} exceeds --max-spend ${max_spend:.2}; lower --decisions"
        );
        return ExitCode::FAILURE;
    }

    let Ok(key) =
        std::env::var("ANTHROPIC_API_KEY").or_else(|_| std::env::var("SECONDWIND_MODEL_KEY"))
    else {
        eprintln!("secondwind replay: set ANTHROPIC_API_KEY (or SECONDWIND_MODEL_KEY)");
        return ExitCode::FAILURE;
    };
    let url = std::env::var("SECONDWIND_MODEL_URL").unwrap_or_else(|_| DEFAULT_URL.to_string());

    let agent = Endpoint {
        agent: ureq::AgentBuilder::new().build(),
        url,
        key,
        model: model.clone(),
    };
    let (stats, changed) = replay_run(&traces, &agent, decisions_target);

    let mut worse = 0u64;
    let mut equivalent = 0u64;
    let mut better = 0u64;
    for c in &changed {
        match agent.judge(c).as_str() {
            "WORSE" => worse += 1,
            "BETTER" => better += 1,
            _ => equivalent += 1,
        }
    }

    let actual = cost(&model, stats.input_tokens, stats.output_tokens).unwrap_or(0.0);
    let result = json!({
        "decisions": stats.decisions,
        "errors": stats.errors,
        "baseline_match": stats.baseline_match,
        "treatment_match": stats.treatment_match,
        "baseline_match_rate": rate(stats.baseline_match, stats.decisions),
        "treatment_match_rate": rate(stats.treatment_match, stats.decisions),
        "same_choice_rate": rate(stats.same_choice, stats.decisions),
        "noise_floor_rate": rate(stats.same_baseline, stats.decisions),
        "compression_cost": rate(stats.same_baseline, stats.decisions)
            - rate(stats.same_choice, stats.decisions),
        "changed": changed.len(),
        "changed_worse": worse,
        "changed_equivalent": equivalent,
        "changed_better": better,
        "resolved_when_offloaded": stats.resolved_when_offloaded,
        "input_tokens": stats.input_tokens,
        "output_tokens": stats.output_tokens,
        "actual_cost_usd": actual,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&result).expect("serializes")
    );
    ExitCode::SUCCESS
}

// Read discovered traces until they hold at least the target number of decisions,
// so a small run does not parse the whole corpus.
fn gather(home: &Path, decisions_target: usize) -> Vec<Trace> {
    let mut traces = Vec::new();
    let mut found = 0usize;
    for source in secondwind_sources::all() {
        let Ok(files) = source.discover(home) else {
            continue;
        };
        for file in &files {
            if decisions_target != 0 && found >= decisions_target {
                return traces;
            }
            let Ok(outcome) = source.read(file) else {
                continue;
            };
            for trace in outcome.traces {
                found += decisions(&trace).len();
                traces.push(trace);
            }
        }
    }
    traces
}

fn cost(model: &str, input_tokens: u64, output_tokens: u64) -> Option<f64> {
    let rates = secondwind_ledger::rates_for(model)?;
    Some(input_tokens as f64 / 1e6 * rates.input + output_tokens as f64 / 1e6 * rates.output)
}

fn rate(hits: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        hits as f64 / total as f64
    }
}

struct Endpoint {
    agent: ureq::Agent,
    url: String,
    key: String,
    model: String,
}

const RETRIES: u32 = 5;

impl Endpoint {
    fn post(&self, body: &Value) -> Option<Value> {
        for attempt in 0..RETRIES {
            match self
                .agent
                .post(&self.url)
                .set("x-api-key", &self.key)
                .set("anthropic-version", ANTHROPIC_VERSION)
                .set("content-type", "application/json")
                .send_json(body.clone())
            {
                Ok(resp) => return resp.into_json::<Value>().ok(),
                Err(ureq::Error::Status(code, _)) if retryable(code) => {
                    std::thread::sleep(std::time::Duration::from_millis(500 << attempt));
                }
                Err(ureq::Error::Status(code, resp)) => {
                    eprintln!(
                        "secondwind replay: {code}: {}",
                        resp.into_string().unwrap_or_default()
                    );
                    return None;
                }
                Err(err) => {
                    eprintln!("secondwind replay: request failed: {err}");
                    return None;
                }
            }
        }
        None
    }
}

fn retryable(code: u16) -> bool {
    matches!(code, 429 | 500 | 502 | 503 | 529)
}

impl Endpoint {
    // Rate a flipped choice as EQUIVALENT, WORSE, or BETTER against the full-context
    // choice, so the changed decisions can be split into harmful and benign.
    fn judge(&self, c: &Changed) -> String {
        let base = c.baseline_tool.as_deref().unwrap_or("(text reply)");
        let treat = c.treatment_tool.as_deref().unwrap_or("(text reply)");
        let system = "You rate an agent's tool choice made on a compressed context \
            against its choice on the full context. Reply with exactly one word: \
            EQUIVALENT, WORSE, or BETTER.";
        let user = format!(
            "Recent context:\n{}\n\nOn the full context the agent called `{base}`. On a \
             compressed context it called `{treat}`. Rate the compressed choice: \
             EQUIVALENT, WORSE, or BETTER.",
            c.recent
        );
        let body = json!({
            "model": self.model,
            "max_tokens": 10,
            "temperature": 0,
            "system": system,
            "messages": [{"role": "user", "content": user}],
        });
        let Some(resp) = self.post(&body) else {
            return "UNKNOWN".to_string();
        };
        let text = resp["content"]
            .as_array()
            .and_then(|b| b.first())
            .and_then(|b| b["text"].as_str())
            .unwrap_or("")
            .to_uppercase();
        for verdict in ["EQUIVALENT", "WORSE", "BETTER"] {
            if text.contains(verdict) {
                return verdict.to_string();
            }
        }
        "UNKNOWN".to_string()
    }
}

impl Model for Endpoint {
    fn decide(&self, req: Request, resolve: &dyn Fn(&str) -> Option<String>) -> Reply {
        let mut messages = req.messages.to_vec();
        let mut input = 0;
        let mut output = 0;
        let mut resolve_calls = 0;
        let mut failed = false;

        for _ in 0..MAX_HOPS {
            let mut body = json!({
                "model": self.model,
                "max_tokens": 1024,
                "temperature": 0,
                "messages": messages,
            });
            if let Some(system) = req.system {
                body["system"] = json!(system);
            }
            if !req.tools.is_empty() {
                body["tools"] = json!(req.tools);
            }
            let Some(resp) = self.post(&body) else {
                failed = true;
                break;
            };
            if let Some(usage) = resp.get("usage") {
                input += usage
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                output += usage
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
            }
            let content = resp.get("content").and_then(Value::as_array);
            let tool_use = content.and_then(|blocks| {
                blocks
                    .iter()
                    .find(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
            });
            let Some(call) = tool_use else {
                return Reply {
                    tool: None,
                    resolve_calls,
                    input_tokens: input,
                    output_tokens: output,
                    errored: false,
                };
            };
            let name = call.get("name").and_then(Value::as_str).unwrap_or_default();
            if name != RESOLVE_TOOL {
                return Reply {
                    tool: Some(name.to_string()),
                    resolve_calls,
                    input_tokens: input,
                    output_tokens: output,
                    errored: false,
                };
            }
            resolve_calls += 1;
            let marker = call
                .get("input")
                .and_then(|i| i.get("marker"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let body_text = resolve(marker).unwrap_or_else(|| "<<unavailable>>".to_string());
            let id = call.get("id").and_then(Value::as_str).unwrap_or("tu");
            messages.push(json!({"role": "assistant", "content": [call.clone()]}));
            messages.push(json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": body_text,
                }],
            }));
        }
        Reply {
            tool: None,
            resolve_calls,
            input_tokens: input,
            output_tokens: output,
            errored: failed,
        }
    }
}
