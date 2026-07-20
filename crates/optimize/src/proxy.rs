use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use serde_json::Value;

use crate::offload::OffloadStore;
use crate::shape::pick_shaper;
use crate::{KeptReason, Optimizer, Outcome, certificate};

// The marker an offloaded stub carries, for checking recoverability against the store.
fn offload_marker(stub: &str) -> Option<&str> {
    let start = stub.find("<<swload:")?;
    let end = stub[start..].find(">>")? + start + 2;
    Some(&stub[start..end])
}

// A rewritten leaf is lossless when it is unchanged, or an inline wire that verifies against the
// original, or an offload stub whose marker resolves back to the original. Nothing else passes.
fn leaf_is_lossless(original: &str, rewritten: &str, store: &dyn OffloadStore) -> bool {
    original == rewritten
        || certificate::verify(rewritten, &certificate::certify(original))
        || offload_marker(rewritten)
            .and_then(|m| store.resolve(m))
            .as_deref()
            == Some(original)
}

/// True only if the rewrite differs from the original in string leaves that provably reconstruct or
/// recover; any structural change or unverifiable leaf makes it false. The proxy runs this before
/// forwarding and falls back to the exact original on failure: closed on correctness, open on availability.
pub fn losslessly_equivalent(
    original: &Value,
    rewritten: &Value,
    store: &dyn OffloadStore,
) -> bool {
    match (original, rewritten) {
        (Value::String(o), Value::String(r)) => leaf_is_lossless(o, r, store),
        (Value::Object(o), Value::Object(r)) => {
            o.len() == r.len()
                && o.iter().all(|(k, ov)| {
                    r.get(k)
                        .is_some_and(|rv| losslessly_equivalent(ov, rv, store))
                })
        }
        (Value::Array(o), Value::Array(r)) => {
            o.len() == r.len()
                && o.iter()
                    .zip(r)
                    .all(|(ov, rv)| losslessly_equivalent(ov, rv, store))
        }
        (o, r) => o == r,
    }
}

// One rewritten block, in the unit the gate priced. first_seen is false on a recurred block, so a
// caller counts each block once, never again on a cached resend.
#[derive(Debug, Clone)]
pub struct BlockStat {
    pub key: String,
    pub transform: String,
    pub input_units: usize,
    pub output_units: usize,
    pub saved_usd: f64,
    pub inline: bool,
    pub atoms: u64,
    pub cert: String,
    pub first_seen: bool,
    // Empty when the block was compressed or offloaded (the body changed). Set to a reason slug when
    // the block was seen but left verbatim, so the ledger can show refused/kept blocks and why.
    pub kept_reason: String,
}

// A block's on-wire form, re-emitted byte-identical on resends so the cached prefix
// never changes. compressed is false for a verbatim block, whose content is untouched.
#[derive(Debug, Clone)]
pub struct Frozen {
    pub wire: String,
    pub compressed: bool,
    pub transform: String,
    pub input_units: usize,
    pub output_units: usize,
    pub saved_usd: f64,
    pub inline: bool,
    pub atoms: u64,
    pub cert: String,
}

impl Frozen {
    fn verbatim() -> Self {
        Self {
            wire: String::new(),
            compressed: false,
            transform: String::new(),
            input_units: 0,
            output_units: 0,
            saved_usd: 0.0,
            inline: true,
            atoms: 0,
            cert: String::new(),
        }
    }
}

// Cross-request memory. `frozen` holds each block's chosen wire (Arc) for byte-identical resends
// that keep the cached prefix stable; `seen` is separate so bounding the wire cache never re-books a
// counted block. Poison-tolerant locks: one panicked request never wedges the rest.
#[derive(Default)]
pub struct FreezeState {
    frozen: RwLock<HashMap<String, Arc<Frozen>>>,
    seen: RwLock<HashSet<String>>,
    // Count-once for kept/refused blocks, separate from `seen` so booking a verbatim block never
    // consumes the first-sight token a later compression of the same bytes needs.
    seen_kept: RwLock<HashSet<String>>,
}

impl FreezeState {
    fn get(&self, key: &str) -> Option<Arc<Frozen>> {
        self.frozen
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned()
    }

    fn store(&self, key: String, frozen: Frozen) {
        self.frozen
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, Arc::new(frozen));
    }

    // True the first time a key is counted, false ever after, independent of whether `frozen`
    // still holds its wire, so evicting the wire cache never double-books the ledger.
    fn book_once(&self, key: &str) -> bool {
        self.seen
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_string())
    }

    // First time a kept/refused block is counted; false ever after.
    fn book_kept_once(&self, key: &str) -> bool {
        self.seen_kept
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_string())
    }

    // Bounds both maps. The wire cache clears at the smaller cap (its entries are heavy); the
    // count-once set clears far less often, so count-once survives many wire-cache clears.
    pub fn bound(&self, frozen_cap: usize, seen_cap: usize) {
        let mut frozen = self.frozen.write().unwrap_or_else(|e| e.into_inner());
        if frozen.len() >= frozen_cap {
            frozen.clear();
        }
        drop(frozen);
        let mut seen = self.seen.write().unwrap_or_else(|e| e.into_inner());
        if seen.len() >= seen_cap {
            seen.clear();
        }
        drop(seen);
        let mut seen_kept = self.seen_kept.write().unwrap_or_else(|e| e.into_inner());
        if seen_kept.len() >= seen_cap {
            seen_kept.clear();
        }
    }
}

// Rewrites a request in place: compresses each tool-output block (a RequestShaper locates them,
// whatever the wire shape). Inline always applies; offload only when the agent carries a resolver.
// Verbatim blocks untouched, so a request only ever shrinks. One stat per rewritten block.
pub fn rewrite(
    body: &mut Value,
    optimizer: &mut Optimizer,
    resolver_override: Option<&str>,
    memory: &FreezeState,
) -> Vec<BlockStat> {
    let shaper = pick_shaper(body);
    // resolver_override declares a resolver the agent carries but does not expose inline (a lazily-loaded
    // MCP host), so offload still fires and stubs nudge the model to load the host's own tool. Verified:
    // injecting a tool is a dead end (host rejects an unregistered call), so we declare, never inject.
    let resolver = resolver_override
        .map(str::to_string)
        .or_else(|| shaper.resolver(body));
    let exempt = resolver
        .as_deref()
        .map(|r| shaper.exempt_ids(body, r))
        .unwrap_or_default();
    // With a resolver, the latest request text ranks rows so relevant ones stay inline.
    let query = resolver.as_ref().and_then(|_| shaper.latest_query(body));
    let has_resolver = resolver.is_some();
    let resolver_name = resolver.unwrap_or_default();
    // Without a resolver an offload could never be surfaced, so prefer inline lossless codecs
    // over an offload that would only be kept verbatim.
    optimizer.set_offload_allowed(has_resolver);

    let mut stats = Vec::new();
    shaper.rewrite_tool_outputs(body, &exempt, &mut |raw: &str, age: u32| {
        block_rewrite(
            raw,
            optimizer,
            query.as_deref(),
            has_resolver,
            &resolver_name,
            memory,
            age,
            &mut stats,
        )
    });
    stats
}

// Turns a fresh block is held whole before it may be offloaded: offloading one the model still needs
// this turn forces a resolve round-trip (inline has no such cost, is not held). Sits just past the
// 1-2 turns a tool output is typically acted on.
const HOLD_TURNS: u32 = 4;

// The wire-agnostic compression of one tool-output block: freeze lookup, gate, and
// stat. Returns the bytes to write back, or None to leave the block unchanged.
#[allow(clippy::too_many_arguments)]
fn block_rewrite(
    raw: &str,
    optimizer: &mut Optimizer,
    query: Option<&str>,
    has_resolver: bool,
    resolver_name: &str,
    memory: &FreezeState,
    age: u32,
    stats: &mut Vec<BlockStat>,
) -> Option<String> {
    let key = block_key(raw);

    // Re-emit a block's frozen bytes; book_once still returns false so a resend is never re-counted
    // even if the wire cache was cleared under cap since.
    if let Some(frozen) = memory.get(&key) {
        if frozen.compressed {
            // A fresh block must not be served an offload stub (from this or another session that aged
            // the same bytes): it would force a resolve round-trip this turn. Inline is safe fresh or
            // aged, always served.
            if !frozen.inline && age < HOLD_TURNS {
                return None;
            }
            let first_seen = memory.book_once(&key);
            stats.push(BlockStat {
                key,
                transform: frozen.transform.clone(),
                input_units: frozen.input_units,
                output_units: frozen.output_units,
                saved_usd: frozen.saved_usd,
                inline: frozen.inline,
                atoms: frozen.atoms,
                cert: frozen.cert.clone(),
                first_seen,
                kept_reason: String::new(),
            });
            return Some(frozen.wire.clone());
        }
        return None;
    }

    // Fresh block: shape it once, then freeze the result.
    let input_units = optimizer.count(raw);
    let (atoms, cert) = crate::proof(raw);
    let outcome = match query {
        Some(q) if !q.trim().is_empty() => optimizer.compress_block_with_query(raw, q),
        _ => optimizer.compress_block(raw),
    };
    match outcome {
        Outcome::Compressed {
            wire,
            transform,
            saved_usd,
            ..
        } => {
            let output_units = optimizer.count(&wire);
            memory.store(
                key.clone(),
                Frozen {
                    wire: wire.clone(),
                    compressed: true,
                    transform: transform.to_string(),
                    input_units,
                    output_units,
                    saved_usd,
                    inline: true,
                    atoms,
                    cert: cert.clone(),
                },
            );
            let first_seen = memory.book_once(&key);
            stats.push(BlockStat {
                key,
                transform: transform.to_string(),
                input_units,
                output_units,
                saved_usd,
                inline: true,
                atoms,
                cert,
                first_seen,
                kept_reason: String::new(),
            });
            Some(wire)
        }
        Outcome::Offloaded { .. } if has_resolver && age < HOLD_TURNS => {
            // Fresh: the model still needs it this turn, so keep it whole. Deliberately not frozen,
            // so a later turn offloads it once aged.
            None
        }
        Outcome::Offloaded { stub, .. } if has_resolver => {
            // Aged and recoverable: offload behind the marker. The hint is appended after pricing
            // the stub, so re-price the real wire.
            let named = format!(
                "{stub}\n[secondwind offloaded the full output. To read it verbatim, call the \
                 secondwind `{resolver_name}` tool with the exact marker above. If that tool is not \
                 already loaded, use tool_search to load the secondwind server first, then call it.]"
            );
            if let Some(usd) = optimizer.saving_usd(raw, &named) {
                let output_units = optimizer.count(&named);
                memory.store(
                    key.clone(),
                    Frozen {
                        wire: named.clone(),
                        compressed: true,
                        transform: "offload".to_string(),
                        input_units,
                        output_units,
                        saved_usd: usd,
                        inline: false,
                        atoms,
                        cert: cert.clone(),
                    },
                );
                let first_seen = memory.book_once(&key);
                stats.push(BlockStat {
                    key,
                    transform: "offload".to_string(),
                    input_units,
                    output_units,
                    saved_usd: usd,
                    inline: false,
                    atoms,
                    cert,
                    first_seen,
                    kept_reason: String::new(),
                });
                Some(named)
            } else {
                push_kept(stats, memory, &key, "no_saving", input_units, atoms);
                memory.store(key, Frozen::verbatim());
                None
            }
        }
        Outcome::KeptVerbatim { reason } => {
            // NotApplicable just means "not a compression candidate" (e.g. too small); only book a
            // deliberate refusal of a real candidate, so the trail stays signal, not noise.
            if !matches!(reason, KeptReason::NotApplicable) {
                push_kept(stats, memory, &key, reason.as_str(), input_units, atoms);
            }
            memory.store(key, Frozen::verbatim());
            None
        }
        // An offload that reached here carries no resolver, so it can never be surfaced.
        Outcome::Offloaded { .. } => {
            push_kept(stats, memory, &key, "no_resolver", input_units, atoms);
            memory.store(key, Frozen::verbatim());
            None
        }
    }
}

// Books a block seen but left verbatim, once, with its reason, so the ledger can report refused/kept
// blocks. output == input and no certificate, since nothing changed.
fn push_kept(
    stats: &mut Vec<BlockStat>,
    memory: &FreezeState,
    key: &str,
    reason: &str,
    units: usize,
    atoms: u64,
) {
    if memory.book_kept_once(key) {
        stats.push(BlockStat {
            key: key.to_string(),
            transform: "kept".to_string(),
            input_units: units,
            output_units: units,
            saved_usd: 0.0,
            inline: true,
            atoms,
            cert: String::new(),
            first_seen: true,
            kept_reason: reason.to_string(),
        });
    }
}

fn block_key(raw: &str) -> String {
    blake3::hash(raw.as_bytes()).to_hex()[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Most tests exercise a single request, so fresh memory each call models a new session;
    // tests that need cross-turn stability call rewrite with a shared FreezeState.
    fn run(body: &mut Value, optimizer: &mut Optimizer, resolver: Option<&str>) -> Vec<BlockStat> {
        rewrite(body, optimizer, resolver, &FreezeState::default())
    }

    fn uniform_tool_result() -> String {
        let rows: Vec<String> = (0..40)
            .map(|i| format!(r#"{{"id":{i},"svc":"svc-{i}","port":{}}}"#, 7000 + i))
            .collect();
        format!("[{}]", rows.join(","))
    }

    fn bulk() -> String {
        "x".repeat(20_000)
    }

    // Appends enough assistant turns to age every preceding tool output past HOLD_TURNS, so a block
    // that would be held while fresh becomes eligible to offload.
    fn age_past_hold(body: &mut Value) {
        let messages = body["messages"].as_array_mut().expect("messages array");
        for _ in 0..HOLD_TURNS {
            messages.push(json!({"role": "assistant", "content": "reasoning step"}));
        }
    }

    #[test]
    fn with_a_resolver_bulk_offloads_and_the_stub_names_the_tool() {
        let mut body = json!({
            "tools": [{"name": "mcp__secondwind__resolve", "description": "swload fetch"}],
            "messages": [{"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": bulk()}
            ]}]
        });
        age_past_hold(&mut body);
        let mut optimizer = Optimizer::default();
        let stats = run(&mut body, &mut optimizer, Some("mcp__secondwind__resolve"));

        assert_eq!(stats.len(), 1);
        assert!(!stats[0].inline);
        let content = body["messages"][0]["content"][0]["content"]
            .as_str()
            .unwrap();
        assert!(content.contains("<<swload:"));
        assert!(
            content.contains("mcp__secondwind__resolve"),
            "the stub names the resolve tool"
        );
        assert!(
            content.contains("offloaded"),
            "the stub explains the marker"
        );
        assert_eq!(
            body["tools"].as_array().unwrap().len(),
            1,
            "nothing injected"
        );
    }

    #[test]
    fn without_a_resolver_bulk_stays_verbatim_and_no_marker_can_strand() {
        let mut body = json!({
            "tools": [{"name": "Read"}],
            "messages": [{"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": bulk()}
            ]}]
        });
        let before = body.clone();
        let mut optimizer = Optimizer::default();
        let stats = run(&mut body, &mut optimizer, None);
        assert!(stats.iter().all(|s| s.inline));
        assert_eq!(body["messages"], before["messages"]);
    }

    #[test]
    fn inline_compression_applies_with_or_without_a_resolver() {
        let mut body = json!({
            "messages": [{"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": uniform_tool_result()}
            ]}]
        });
        let mut optimizer = Optimizer::default();
        let stats = run(&mut body, &mut optimizer, None);
        assert_eq!(stats.len(), 1);
        assert!(stats[0].inline);
        assert!(stats[0].saved_usd > 0.0);
    }

    #[test]
    fn a_resolved_body_is_never_reoffloaded() {
        let resolver = "mcp__secondwind__resolve";
        let mut body = json!({
            "tools": [{"name": resolver, "description": "swload fetch"}],
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_r", "name": resolver,
                     "input": {"marker": "<<swload:abc>>"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_r", "content": bulk()}
                ]}
            ]
        });
        let before = body.clone();
        let mut optimizer = Optimizer::default();
        let stats = run(&mut body, &mut optimizer, Some(resolver));
        assert!(stats.is_empty(), "the resolve answer must stay verbatim");
        assert_eq!(body, before);
    }

    #[test]
    fn a_query_keeps_relevant_rows_inline_and_offloads_the_rest() {
        let resolver = "mcp__secondwind__resolve";
        let mut rows: Vec<String> = (0..40)
            .map(|i| format!(r#"{{"id":{i},"note":"shipping record {i} for delivery"}}"#))
            .collect();
        rows.push(r#"{"id":900,"note":"authentication token rotated for admin"}"#.into());
        let block = format!("[{}]", rows.join(","));

        let mut body = json!({
            "tools": [{"name": resolver, "description": "swload fetch"}],
            "messages": [
                {"role": "user", "content": "what happened with the authentication token"},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": block}
                ]}
            ]
        });
        age_past_hold(&mut body);
        let mut optimizer = Optimizer::default();
        let stats = run(&mut body, &mut optimizer, Some(resolver));

        assert_eq!(stats.len(), 1);
        assert!(!stats[0].inline, "the split offloads the remainder");
        let content = body["messages"][1]["content"][0]["content"]
            .as_str()
            .unwrap();
        assert!(
            content.contains("authentication token rotated"),
            "relevant row stays inline"
        );
        assert!(content.contains("<<swload:"), "the rest is recoverable");
        assert!(content.contains("less relevant"));
    }

    #[test]
    fn a_byte_exact_repeat_re_emits_identical_bytes() {
        let resolver = "mcp__secondwind__resolve";
        let block = bulk();
        let mut body = json!({
            "tools": [{"name": resolver, "description": "swload fetch"}],
            "messages": [
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": block}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t2", "content": block}
                ]}
            ]
        });
        age_past_hold(&mut body);
        let mut optimizer = Optimizer::default();
        let stats = run(&mut body, &mut optimizer, Some(resolver));
        assert_eq!(stats.len(), 2);
        // Counted once: only the first is fresh; the repeat carries the frozen form.
        assert!(stats[0].first_seen);
        assert!(!stats[1].first_seen);

        // Cache-stable: the repeat is byte-identical to the first, never a different,
        // shorter reference that would shift the prefix and bust the provider cache.
        let first = body["messages"][0]["content"][0]["content"]
            .as_str()
            .unwrap();
        let second = body["messages"][1]["content"][0]["content"]
            .as_str()
            .unwrap();
        assert_eq!(first, second);
        assert!(first.contains("<<swload:"));
        let marker = first
            .lines()
            .find(|l| l.contains("<<swload:"))
            .and_then(|l| l.split_whitespace().find(|w| w.starts_with("<<swload:")))
            .unwrap();
        assert_eq!(optimizer.resolve(marker).as_deref(), Some(block.as_str()));
    }

    #[test]
    fn a_fresh_block_is_held_whole_and_matures_to_offload_once_aged() {
        let resolver = "mcp__secondwind__resolve";
        let tool_msg = json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "t1", "content": bulk()}
        ]});

        // Fresh: the model still needs it this turn, so it stays whole rather than offload (which
        // would force a resolve round-trip and, since it is verbatim, keep the cache prefix stable).
        let mut fresh = json!({
            "tools": [{"name": resolver, "description": "swload fetch"}],
            "messages": [tool_msg.clone()]
        });
        let before = fresh.clone();
        let stats = run(&mut fresh, &mut Optimizer::default(), Some(resolver));
        assert!(stats.is_empty(), "a fresh block is not offloaded");
        assert_eq!(fresh["messages"], before["messages"], "kept byte-for-byte");

        // Aged: the model has moved on, so it matures to a recoverable offload.
        let mut aged = json!({
            "tools": [{"name": resolver, "description": "swload fetch"}],
            "messages": [tool_msg]
        });
        age_past_hold(&mut aged);
        let stats = run(&mut aged, &mut Optimizer::default(), Some(resolver));
        assert_eq!(stats.len(), 1);
        assert!(!stats[0].inline, "an aged block offloads");
    }

    #[test]
    fn a_block_offloaded_by_an_aged_session_is_not_served_to_a_fresh_one() {
        // The freeze is global by content, but age is per-session position: a block one session
        // aged and offloaded must not be handed as an offload stub to another session where the
        // same bytes are still fresh.
        let resolver = "mcp__secondwind__resolve";
        let memory = FreezeState::default();
        let tool_msg = json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "t1", "content": bulk()}
        ]});

        let mut aged = json!({
            "tools": [{"name": resolver, "description": "swload fetch"}],
            "messages": [tool_msg.clone()]
        });
        age_past_hold(&mut aged);
        let aged_stats = rewrite(
            &mut aged,
            &mut Optimizer::default(),
            Some(resolver),
            &memory,
        );
        assert!(
            !aged_stats[0].inline,
            "the aged session froze an offload globally"
        );

        let mut fresh = json!({
            "tools": [{"name": resolver, "description": "swload fetch"}],
            "messages": [tool_msg]
        });
        let before = fresh.clone();
        let fresh_stats = rewrite(
            &mut fresh,
            &mut Optimizer::default(),
            Some(resolver),
            &memory,
        );
        assert!(
            fresh_stats.is_empty(),
            "the fresh session keeps its block whole"
        );
        assert_eq!(
            fresh["messages"], before["messages"],
            "no offload stub leaked in"
        );
    }

    #[test]
    fn a_resend_across_turns_keeps_byte_identical_bytes() {
        let freeze = FreezeState::default();
        let mut optimizer = Optimizer::default();
        let block = uniform_tool_result();
        let turn = || {
            json!({
                "messages": [{"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": block}
                ]}]
            })
        };

        let mut first = turn();
        let s1 = rewrite(&mut first, &mut optimizer, None, &freeze);
        assert_eq!(s1.len(), 1);
        assert!(s1[0].first_seen);

        let mut second = turn();
        let s2 = rewrite(&mut second, &mut optimizer, None, &freeze);
        assert_eq!(s2.len(), 1);
        assert!(!s2[0].first_seen, "a resend is counted once, not re-booked");

        assert_eq!(
            first["messages"][0]["content"][0]["content"],
            second["messages"][0]["content"][0]["content"],
            "the resend must re-emit byte-identical bytes so the cached prefix holds"
        );
    }

    // Run with: SW_BENCH=1 cargo test -p secondwind-optimize --features tiktoken bench_fresh -- --nocapture
    // Measures the FRESH-compression cost per block (parse + shape + columnar + admit + CLMH +
    // priced), single-threaded, with distinct blocks so none hit the freeze cache.
    #[test]
    fn bench_fresh_compression_throughput() {
        if std::env::var_os("SW_BENCH").is_none() {
            return;
        }
        use std::time::Instant;
        let make = |seed: usize| -> Value {
            let rows: Vec<Value> = (0..60)
                .map(|i| {
                    json!({
                        "name": format!("pod-{seed}-{i}"),
                        "ns": format!("team-{}", seed % 8),
                        "status": "Running",
                        "restarts": i % 5,
                        "ip": format!("10.0.{}.{}", seed % 256, i),
                    })
                })
                .collect();
            json!({"messages": [{"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t",
                 "content": serde_json::to_string(&rows).unwrap()}
            ]}]})
        };

        #[cfg(feature = "tiktoken")]
        let (mut optimizer, unit) = (
            Optimizer::default()
                .with_counter(std::sync::Arc::new(crate::tokens::Tiktoken::cl100k())),
            "tiktoken",
        );
        #[cfg(not(feature = "tiktoken"))]
        let (mut optimizer, unit) = (Optimizer::default(), "bytes");

        let n = 2000usize;
        let bodies: Vec<Value> = (0..n).map(make).collect();
        let one = serde_json::to_string(&bodies[0]).unwrap();
        let memory = FreezeState::default();

        // Isolate the tokenizer: it is counted ~4x per block today (raw + wire, in block_rewrite
        // and again in priced), so if it dominates, removing the redundant counts is the win.
        let sample_rows: Vec<Value> = (0..60)
            .map(|i| json!({"name": format!("pod-{i}"), "ns": "team-1", "status": "Running", "restarts": i % 5, "ip": format!("10.0.0.{i}")}))
            .collect();
        let content = serde_json::to_string(&sample_rows).unwrap();
        let t0 = Instant::now();
        for _ in 0..n {
            std::hint::black_box(optimizer.count(&content));
        }
        let count_us = t0.elapsed().as_secs_f64() * 1e6 / n as f64;
        eprintln!(
            "\n  bare tokenizer count: {count_us:.1} us/call ({} B)",
            content.len()
        );

        let start = Instant::now();
        let mut compressed = 0usize;
        for mut body in bodies {
            if !rewrite(&mut body, &mut optimizer, None, &memory).is_empty() {
                compressed += 1;
            }
        }
        let elapsed = start.elapsed();
        let per_block = elapsed.as_secs_f64() * 1e6 / n as f64;
        let per_sec = n as f64 / elapsed.as_secs_f64();
        eprintln!(
            "\nBENCH fresh compression ({unit}, ~{} B/block): {n} blocks in {:?}\n  {per_block:.1} us/block, {per_sec:.0} blocks/sec/core, {compressed}/{n} compressed",
            one.len(),
            elapsed,
        );
        eprintln!(
            "  => ~{:.0} fresh-compression req/sec on 8 bounded threads (extrapolated)\n",
            per_sec * 8.0
        );
    }

    #[test]
    fn count_once_survives_a_wire_cache_clear() {
        let mut optimizer = Optimizer::default();
        let block = uniform_tool_result();
        let turn = || {
            json!({
                "messages": [{"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": block}
                ]}]
            })
        };
        let memory = FreezeState::default();

        let mut first = turn();
        assert!(rewrite(&mut first, &mut optimizer, None, &memory)[0].first_seen);

        // Clear the wire cache as the cap would, but keep the count-once set.
        memory.bound(0, usize::MAX);

        let mut again = turn();
        let s2 = rewrite(&mut again, &mut optimizer, None, &memory);
        assert_eq!(s2.len(), 1);
        assert!(
            !s2[0].first_seen,
            "clearing the wire cache must never re-book an already-counted block"
        );
    }

    #[test]
    fn a_thin_margin_offload_never_grows_the_wire() {
        use crate::prose::{ProseShrinker, Span};

        // Drops only a ~130-byte slice, so the stub beats the original by less than
        // the appended hint costs.
        struct DropTiny;
        impl ProseShrinker for DropTiny {
            fn keep(&self, text: &str) -> Option<Vec<Span>> {
                let n = text.len();
                let a = n / 2;
                let mut b = (a + 130).min(n);
                while b < n && !text.is_char_boundary(b) {
                    b += 1;
                }
                Some(vec![Span { start: 0, end: a }, Span { start: b, end: n }])
            }
        }

        let resolver = "mcp__secondwind__resolve";
        let raw = "The service validates every request and logs it. ".repeat(32);
        let mut body = json!({
            "tools": [{"name": resolver, "description": "swload fetch"}],
            "messages": [{"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": raw}
            ]}]
        });
        let mut optimizer = Optimizer::default().with_prose_shrinker(std::sync::Arc::new(DropTiny));
        let _ = run(&mut body, &mut optimizer, Some(resolver));

        let content = body["messages"][0]["content"][0]["content"]
            .as_str()
            .unwrap();
        assert!(
            content.len() <= raw.len(),
            "wire grew from {} to {} bytes",
            raw.len(),
            content.len()
        );
    }

    #[test]
    fn a_repeat_without_a_resolver_is_not_referenced() {
        let block = bulk();
        let mut body = json!({
            "tools": [{"name": "Read"}],
            "messages": [
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": block}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t2", "content": block}
                ]}
            ]
        });
        let before = body.clone();
        let mut optimizer = Optimizer::default();
        let stats = run(&mut body, &mut optimizer, None);
        assert!(stats.iter().all(|s| s.inline));
        assert_eq!(
            body["messages"], before["messages"],
            "no stranded reference"
        );
    }

    #[test]
    fn a_request_with_no_tool_results_is_untouched() {
        let mut body = json!({
            "messages": [{"role": "user", "content": "just text"}]
        });
        let before = body.clone();
        let mut optimizer = Optimizer::default();
        let stats = run(&mut body, &mut optimizer, None);
        assert!(stats.is_empty());
        assert_eq!(body, before);
    }

    #[test]
    fn incompressible_tool_results_are_left_verbatim() {
        let mut body = json!({
            "messages": [{"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "short"}
            ]}]
        });
        let mut optimizer = Optimizer::default();
        let stats = run(&mut body, &mut optimizer, None);
        assert!(stats.is_empty());
        assert_eq!(body["messages"][0]["content"][0]["content"], "short");
    }

    #[test]
    fn a_refused_block_is_booked_kept_once_with_a_reason() {
        use crate::prose::{ProseShrinker, Span};
        // Drops only a tiny slice, so the aged offload stub beats the original by less than the
        // resolver hint costs: the gate refuses it (no net saving) and books it as seen-but-kept.
        struct DropTiny;
        impl ProseShrinker for DropTiny {
            fn keep(&self, text: &str) -> Option<Vec<Span>> {
                let n = text.len();
                let a = n / 2;
                let mut b = (a + 130).min(n);
                while b < n && !text.is_char_boundary(b) {
                    b += 1;
                }
                Some(vec![Span { start: 0, end: a }, Span { start: b, end: n }])
            }
        }
        let resolver = "mcp__secondwind__resolve";
        let raw = "The service validates every request and logs it. ".repeat(32);
        let mut before = json!({
            "tools": [{"name": resolver, "description": "swload fetch"}],
            "messages": [{"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": raw}
            ]}]
        });
        age_past_hold(&mut before);
        let mk = || Optimizer::default().with_prose_shrinker(std::sync::Arc::new(DropTiny));
        let memory = FreezeState::default();

        let mut body = before.clone();
        let stats = rewrite(&mut body, &mut mk(), Some(resolver), &memory);
        assert_eq!(stats.len(), 1, "the kept block is booked");
        assert_eq!(
            stats[0].kept_reason, "no_saving",
            "the refusal reason is surfaced"
        );
        assert!(stats[0].first_seen);
        assert_eq!(
            stats[0].input_units, stats[0].output_units,
            "a kept block changed nothing"
        );
        assert_eq!(body["messages"], before["messages"], "body untouched");

        // Counted once: a resend of the same kept block is not re-booked.
        let mut again = before.clone();
        assert!(
            rewrite(&mut again, &mut mk(), Some(resolver), &memory).is_empty(),
            "booked once"
        );

        // A compressed block carries no kept_reason, so `changed` can tell them apart.
        let mut comp = json!({"messages": [{"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "t2", "content": uniform_tool_result()}
        ]}]});
        let cs = rewrite(
            &mut comp,
            &mut Optimizer::default(),
            None,
            &FreezeState::default(),
        );
        assert_eq!(cs.len(), 1);
        assert!(
            cs[0].kept_reason.is_empty(),
            "a compressed block has no kept reason"
        );
    }

    #[test]
    fn openai_shaped_tool_messages_are_compressed() {
        let raw = uniform_tool_result();
        let mut body = json!({
            "model": "gpt-5",
            "messages": [
                {"role": "user", "content": "list the services"},
                {"role": "assistant", "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "list_services"}}
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": raw}
            ]
        });
        let mut optimizer = Optimizer::default();
        let stats = run(&mut body, &mut optimizer, None);
        assert_eq!(stats.len(), 1, "the tool message is compressed");
        assert!(stats[0].inline);
        let out = body["messages"][2]["content"].as_str().unwrap();
        assert!(out.len() < raw.len(), "content shrank");
        // The user and assistant messages are untouched.
        assert_eq!(body["messages"][0]["content"], "list the services");
    }

    #[test]
    fn the_whole_request_proof_passes_a_verified_rewrite_and_fails_a_tamper_or_drop() {
        use crate::offload::Store;
        let store = Store::default();
        let raw = uniform_tool_result();
        let original = json!({
            "model": "gpt-5",
            "messages": [
                {"role": "user", "content": "list the services"},
                {"role": "tool", "tool_call_id": "call_1", "content": raw}
            ]
        });
        let mut rewritten = original.clone();
        let mut optimizer = Optimizer::default();
        assert!(
            !run(&mut rewritten, &mut optimizer, None).is_empty(),
            "the tool output compresses"
        );
        assert!(
            losslessly_equivalent(&original, &rewritten, &store),
            "the honest rewrite proves lossless"
        );
        assert!(
            losslessly_equivalent(&original, &original, &store),
            "an untouched request trivially passes"
        );

        // A tampered leaf that no longer reconstructs the original must fail.
        let mut tampered = rewritten.clone();
        tampered["messages"][1]["content"] = json!("garbage that reconstructs nothing");
        assert!(
            !losslessly_equivalent(&original, &tampered, &store),
            "a lossy tamper is caught"
        );

        // A dropped message (structural change) must fail.
        let mut dropped = rewritten.clone();
        dropped["messages"].as_array_mut().unwrap().truncate(1);
        assert!(
            !losslessly_equivalent(&original, &dropped, &store),
            "a dropped message is caught"
        );
    }

    #[test]
    fn openai_resolver_result_stays_verbatim() {
        let resolver = "secondwind_resolve";
        let raw = bulk();
        let mut body = json!({
            "tools": [{"type": "function", "function": {"name": resolver, "description": "swload fetch"}}],
            "messages": [
                {"role": "assistant", "tool_calls": [
                    {"id": "call_r", "type": "function", "function": {"name": resolver}}
                ]},
                {"role": "tool", "tool_call_id": "call_r", "content": raw}
            ]
        });
        let before = body.clone();
        let mut optimizer = Optimizer::default();
        let stats = run(&mut body, &mut optimizer, None);
        assert!(stats.is_empty(), "a resolve answer is never re-offloaded");
        assert_eq!(body, before);
    }

    #[test]
    fn openai_responses_api_outputs_are_compressed() {
        let raw = uniform_tool_result();
        let mut body = json!({
            "model": "gpt-5",
            "input": [
                {"role": "user", "content": "list services"},
                {"type": "function_call", "call_id": "fc_1", "name": "list", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "fc_1", "output": raw}
            ]
        });
        let mut optimizer = Optimizer::default();
        let stats = run(&mut body, &mut optimizer, None);
        assert_eq!(stats.len(), 1, "the function_call_output is compressed");
        assert!(stats[0].inline);
        let out = body["input"][2]["output"].as_str().unwrap();
        assert!(out.len() < raw.len(), "output shrank");
        assert_eq!(body["input"][0]["content"], "list services");
    }

    #[test]
    fn bedrock_converse_tool_results_are_compressed() {
        let raw = uniform_tool_result();
        let mut body = json!({
            "toolConfig": {"tools": [{"toolSpec": {"name": "list_services"}}]},
            "messages": [
                {"role": "user", "content": [{"text": "list the services"}]},
                {"role": "assistant", "content": [
                    {"toolUse": {"toolUseId": "tu_1", "name": "list_services", "input": {}}}
                ]},
                {"role": "user", "content": [
                    {"toolResult": {"toolUseId": "tu_1", "content": [{"text": raw}], "status": "success"}}
                ]}
            ]
        });
        let mut optimizer = Optimizer::default();
        let stats = run(&mut body, &mut optimizer, None);
        assert_eq!(stats.len(), 1, "the toolResult is compressed");
        assert!(stats[0].inline);
        let out = body["messages"][2]["content"][0]["toolResult"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(out.len() < raw.len(), "output shrank");
        assert_eq!(
            body["messages"][0]["content"][0]["text"],
            "list the services"
        );
    }

    #[test]
    fn bedrock_resolver_result_stays_verbatim() {
        let resolver = "secondwind_resolve";
        let raw = bulk();
        let mut body = json!({
            "toolConfig": {"tools": [{"toolSpec": {"name": resolver, "description": "swload fetch"}}]},
            "messages": [
                {"role": "assistant", "content": [
                    {"toolUse": {"toolUseId": "tu_r", "name": resolver, "input": {}}}
                ]},
                {"role": "user", "content": [
                    {"toolResult": {"toolUseId": "tu_r", "content": [{"text": raw}]}}
                ]}
            ]
        });
        let before = body.clone();
        let mut optimizer = Optimizer::default();
        let stats = run(&mut body, &mut optimizer, None);
        assert!(stats.is_empty(), "a resolve answer is never re-offloaded");
        assert_eq!(body, before);
    }
}
