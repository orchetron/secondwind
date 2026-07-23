// Prompt-cache preservation benchmark: does rewriting preserve a provider's prefix cache across a
// growing conversation vs sending history verbatim? Models provider prefix caching (cache breaks at
// the first differing byte; min 1024 cacheable tokens; read/create rates from the rate table).
// cargo run -p secondwind-optimize --example cache_bench --release --features tiktoken
use secondwind_ledger::rates_for;
use secondwind_optimize::Optimizer;
use secondwind_optimize::netcost::Zone;
use secondwind_optimize::proxy::{FreezeState, rewrite};
use secondwind_optimize::reconcile::{Predicted, Realized, Reconciliation, reconcile};
use secondwind_optimize::tokens::{Tiktoken, TokenCounter};
use serde_json::{Value, json};
use std::sync::Arc;

// Cache read/create multipliers are DERIVED from the shipped rate table (cache_read/input and
// cache_write_5m/input for the priced model), not hardcoded, so they track the real rates per model.
const MODEL: &str = "claude-sonnet-4-5";
// Provider minimum cacheable prefix (tokens): model-dependent, not a pricing rate so not in the rate
// table. 1024 for the priced model.
const MIN_CACHEABLE: usize = 1024;
const RESOLVER: &str = "mcp__secondwind__resolve";
const TURNS: usize = 12; // conversation length to simulate; a benchmark knob, not a correctness value

struct Turn {
    read: usize,
    create: usize,
}

// Effective input tokens for one turn under the model's own cache economics from the rate table.
fn cost(t: &Turn, read_mult: f64, create_mult: f64) -> f64 {
    t.read as f64 * read_mult + t.create as f64 * create_mult
}

fn common_prefix_len(a: &[u8], b: &[u8]) -> usize {
    let mut i = 0;
    while i < a.len() && i < b.len() && a[i] == b[i] {
        i += 1;
    }
    i
}

// The cache split for one turn: the byte-common prefix with the prior turn is the cached region; tokenize
// it for the read count, the remainder is created. A prefix under the minimum does not cache at all.
fn cache_split(counter: &Tiktoken, prev: &str, cur: &str) -> Turn {
    let mut b = common_prefix_len(prev.as_bytes(), cur.as_bytes());
    while b > 0 && !cur.is_char_boundary(b) {
        b -= 1;
    }
    let read_raw = counter.count(&cur[..b]);
    let total = counter.count(cur);
    let read = if read_raw >= MIN_CACHEABLE {
        read_raw
    } else {
        0
    };
    Turn {
        read,
        create: total.saturating_sub(read),
    }
}

// A stable long preamble so there is a genuine cacheable prefix (> 1024 tokens) that should stay cached
// across every turn unless a rewrite disturbs it.
fn preamble() -> String {
    format!(
        "You are a coding agent. {}",
        "Follow the repository conventions exactly and cite files by path and line. ".repeat(90)
    )
}

fn assistant_tool_use(id: usize) -> Value {
    json!({"role":"assistant","content":[{"type":"tool_use","id":format!("t{id}"),"name":"fetch","input":{}}]})
}

// A records-array tool result: big enough to offload, with a content-covering preview, distinct per turn.
fn tool_result(id: usize) -> Value {
    let rows: Vec<String> = (0..30)
        .map(|i| {
            format!(
                r#"{{"id":{},"turn":{id},"state":"{}","title":"record {i} in turn {id}","body":"{}"}}"#,
                id * 100 + i,
                if i % 2 == 0 { "open" } else { "closed" },
                "context detail ".repeat(6)
            )
        })
        .collect();
    let content = format!("[{}]", rows.join(","));
    json!({"role":"user","content":[{"type":"tool_result","tool_use_id":format!("t{id}"),"content":content}]})
}

fn main() {
    scenario("offload, cache guard on (default)", Some(RESOLVER), true);
    println!();
    scenario(
        "offload, cache guard OFF (maturation)",
        Some(RESOLVER),
        false,
    );
    println!();
    scenario("inline-only (no resolver)", None, true);
}

fn scenario(label: &str, resolver: Option<&str>, guard: bool) {
    println!("### {label}");
    let rates = rates_for(MODEL).expect("priced model in the rate table");
    let read_mult = rates.cache_read / rates.input;
    let create_mult = rates.cache_write_5m / rates.input;
    let freeze = if guard {
        FreezeState::default()
    } else {
        FreezeState::without_cache_guard()
    };
    let counter = Tiktoken::cl100k();
    let mut optimizer = Optimizer::default().with_counter(Arc::new(Tiktoken::cl100k()));

    let mut messages = vec![json!({"role":"user","content": preamble()})];
    let (mut prev_base, mut prev_sw) = (String::new(), String::new());
    let (mut base, mut sw) = (Vec::<Turn>::new(), Vec::<Turn>::new());
    let mut busts = Vec::<usize>::new();

    println!(
        "{:>4}  {:>16}  {:>16}  reconcile",
        "turn", "baseline r/c", "secondwind r/c"
    );
    for turn in 0..TURNS {
        messages.push(assistant_tool_use(turn));
        messages.push(tool_result(turn));

        let base_body = json!({ "messages": messages });
        let base_str = serde_json::to_string(&base_body).unwrap();
        let mut sw_body = base_body.clone();
        rewrite(&mut sw_body, &mut optimizer, resolver, &freeze);
        let sw_str = serde_json::to_string(&sw_body).unwrap();

        if turn > 0 {
            let bt = cache_split(&counter, &prev_base, &base_str);
            let st = cache_split(&counter, &prev_sw, &sw_str);
            // reconcile() flags a turn where creation dominated reads: the prefix shifted, cache busted.
            let verdict = reconcile(
                &Predicted {
                    zone: Zone::Frozen,
                    saved_usd: 0.0,
                    wire_bytes: 0,
                    canonical_bytes: 0,
                },
                &Realized {
                    input_tokens: (st.read + st.create) as u64,
                    cache_read_tokens: st.read as u64,
                    cache_creation_tokens: st.create as u64,
                },
            );
            let flag = if verdict == Reconciliation::CacheBust {
                busts.push(turn);
                "CACHE-BUST"
            } else {
                "held"
            };
            println!(
                "{turn:>4}  {:>7}/{:<8}  {:>7}/{:<8}  {flag}",
                bt.read, bt.create, st.read, st.create
            );
            base.push(bt);
            sw.push(st);
        }
        prev_base = base_str;
        prev_sw = sw_str;
    }

    let sum = |v: &[Turn], f: fn(&Turn) -> usize| v.iter().map(f).sum::<usize>();
    let (br, bc) = (sum(&base, |t| t.read), sum(&base, |t| t.create));
    let (sr, sc) = (sum(&sw, |t| t.read), sum(&sw, |t| t.create));
    let base_cost: f64 = base.iter().map(|t| cost(t, read_mult, create_mult)).sum();
    let sw_cost: f64 = sw.iter().map(|t| cost(t, read_mult, create_mult)).sum();
    let no_cache: f64 = base.iter().map(|t| (t.read + t.create) as f64).sum();

    println!("\n=== totals over {} measured turns ===", base.len());
    println!("no caching (every turn full history @1.0x):   {no_cache:>12.0} eff. input tokens");
    println!(
        "baseline verbatim + provider cache:            {base_cost:>12.0} eff. input tokens   (read {br}, create {bc})"
    );
    println!(
        "secondwind rewrite + provider cache:           {sw_cost:>12.0} eff. input tokens   (read {sr}, create {sc})"
    );
    let vs_base = 100.0 * (base_cost - sw_cost) / base_cost;
    let read_pres = if br > 0 {
        100.0 * sr as f64 / br as f64
    } else {
        0.0
    };
    println!(
        "\nsecondwind vs baseline effective input cost:   {vs_base:+.1}%  (positive = cheaper)"
    );
    println!("cache-read tokens, sw/base (lower = smaller prefix): {read_pres:.1}%");
    println!(
        "cache-bust turns (creation dominated a frozen prefix): {}",
        if busts.is_empty() {
            "none".to_string()
        } else {
            format!("{busts:?}")
        }
    );
}
