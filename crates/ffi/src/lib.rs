// The single C ABI over the secondwind core; every language binds to these same functions, so no
// language is special-cased. Contract: UTF-8 JSON in, UTF-8 JSON out. Any *mut u8 returned is
// caller-owned, release once with sw_free. Errors never trap; they return a JSON error object.

// Every exported function is a C ABI boundary that takes caller-owned raw pointers by contract.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::c_void;
use std::path::PathBuf;
use std::slice;
use std::sync::Arc;
use std::time::Duration;

use secondwind_ledger::events;
use secondwind_optimize::certificate::{self, Certificate};
use secondwind_optimize::offload::{CallbackStore, OffloadStore, Store};
use secondwind_optimize::proxy::{FreezeState, rewrite};
use secondwind_optimize::tokens::Tiktoken;
use secondwind_optimize::transform::CallbackProposer;
use secondwind_optimize::{Optimizer, Outcome};
use serde_json::{Value, json};

// Prices in real model tokens (cl100k via bpe-openai) so a caller's numbers match the proxy.
fn optimizer() -> Optimizer {
    Optimizer::default().with_counter(Arc::new(Tiktoken::cl100k()))
}

const OFFLOAD_TTL: Duration = Duration::from_secs(24 * 3600);
const FREEZE_CAP: usize = 100_000;
const SEEN_CAP: usize = 500_000;

/// The contract version crossing the boundary; bindings can gate on it.
pub const SW_ABI_VERSION: u32 = 1;

#[unsafe(no_mangle)]
pub extern "C" fn sw_abi_version() -> u32 {
    SW_ABI_VERSION
}

/// Compress one block. Input is UTF-8 JSON `{"block": "<text>", "model": "<optional model id>"}`.
/// Returns a newly allocated UTF-8 JSON buffer (release with sw_free) describing the outcome:
/// `{"kind":"compressed"|"offloaded"|"verbatim"|"error", ...}`. Never traps.
#[unsafe(no_mangle)]
pub extern "C" fn sw_compress(
    input_ptr: *const u8,
    input_len: usize,
    out_len: *mut usize,
) -> *mut u8 {
    let json = std::panic::catch_unwind(|| compress_json(read_input(input_ptr, input_len)))
        .unwrap_or_else(|_| error("panic in core"));
    into_owned(json.into_bytes(), out_len)
}

/// Independently verify a compressed wire against its fidelity certificate. Input is UTF-8 JSON
/// `{"wire": "<compressed>", "hash": "<certificate hash>"}`. Returns `{"ok": true|false}`, so any
/// caller can confirm losslessness itself rather than trust the compressor.
#[unsafe(no_mangle)]
pub extern "C" fn sw_verify(
    input_ptr: *const u8,
    input_len: usize,
    out_len: *mut usize,
) -> *mut u8 {
    let json = std::panic::catch_unwind(|| verify_json(read_input(input_ptr, input_len)))
        .unwrap_or_else(|_| error("panic in core"));
    into_owned(json.into_bytes(), out_len)
}

/// Opaque per-conversation session: cross-request freeze memory (resends re-emit byte-identical
/// bytes so the cache prefix holds), offload store, optional resolver. One per conversation.
pub struct Session {
    freeze: FreezeState,
    store: Arc<dyn OffloadStore>,
    resolver: Option<String>,
    home: Option<PathBuf>,
    proposers: bool,
    codec: Option<Codec>,
}

// A host-supplied codec: encode turns a block into a wire, decode turns it back. secondwind proves
// decode(encode(raw)) == raw for every block, so a wrong codec is dropped, never shipped.
struct Codec {
    ctx: usize,
    encode: CodecFn,
    decode: CodecFn,
}

// ctx is a host-owned pointer; encode/decode take input bytes, return an output pointer (length via
// out_len) or null. The pointer need only stay valid until the next call; secondwind copies at once.
type CodecFn = extern "C" fn(
    ctx: *mut c_void,
    input: *const u8,
    input_len: usize,
    out_len: *mut usize,
) -> *const u8;

fn call_codec(f: CodecFn, ctx: usize, input: &str) -> Option<String> {
    let mut out_len: usize = 0;
    let ptr = f(
        ctx as *mut c_void,
        input.as_ptr(),
        input.len(),
        &mut out_len,
    );
    if ptr.is_null() {
        return None;
    }
    String::from_utf8(unsafe { slice::from_raw_parts(ptr, out_len) }.to_vec()).ok()
}

/// Open a session. Config is UTF-8 JSON, all optional:
/// `{"resolver": "<tool name>", "offload_dir": "<path>", "home": "<dir>"}`. With `home` set, each
/// rewrite books ledger events under it (so `secondwind proof` shows these runs). Release with sw_session_free.
#[unsafe(no_mangle)]
pub extern "C" fn sw_session_new(config_ptr: *const u8, config_len: usize) -> *mut Session {
    let config = parse_config(config_ptr, config_len);
    let store: Arc<dyn OffloadStore> = match config.get("offload_dir").and_then(Value::as_str) {
        Some(dir) => Arc::new(Store::persistent(dir, OFFLOAD_TTL)),
        None => Arc::new(Store::default()),
    };
    build_session(&config, store, None)
}

// Host offload backend: bytes live wherever the host puts them; secondwind keeps the marker/stub/
// coverage logic. `put` stores `val` under `id` (nonzero = ok); `get` returns the bytes (len via
// out_len) or null, pointer valid only until the next call (copied at once). offload_dir is ignored.
type PutFn = extern "C" fn(
    ctx: *mut c_void,
    id: *const u8,
    id_len: usize,
    val: *const u8,
    val_len: usize,
) -> i32;
type GetFn =
    extern "C" fn(ctx: *mut c_void, id: *const u8, id_len: usize, out_len: *mut usize) -> *const u8;

#[unsafe(no_mangle)]
pub extern "C" fn sw_session_new_with_store(
    config_ptr: *const u8,
    config_len: usize,
    ctx: *mut c_void,
    put: PutFn,
    get: GetFn,
) -> *mut Session {
    let config = parse_config(config_ptr, config_len);
    // ctx crosses as usize so the closures are Send + Sync; the host owns its thread-safety.
    let ctx = ctx as usize;
    let store = Arc::new(CallbackStore::new(
        move |id: &str, val: &str| {
            put(
                ctx as *mut c_void,
                id.as_ptr(),
                id.len(),
                val.as_ptr(),
                val.len(),
            ) != 0
        },
        move |id: &str| {
            let mut out_len: usize = 0;
            let ptr = get(ctx as *mut c_void, id.as_ptr(), id.len(), &mut out_len);
            if ptr.is_null() {
                return None;
            }
            String::from_utf8(unsafe { slice::from_raw_parts(ptr, out_len) }.to_vec()).ok()
        },
    ));
    build_session(&config, store, None)
}

/// Register a host-supplied codec that competes in the best-of-N search. secondwind proves
/// decode(encode(raw)) == raw per block and drops any that fails, so a reckless codec never corrupts
/// output. Config is sw_session_new's JSON.
#[unsafe(no_mangle)]
pub extern "C" fn sw_session_new_with_codec(
    config_ptr: *const u8,
    config_len: usize,
    ctx: *mut c_void,
    encode: CodecFn,
    decode: CodecFn,
) -> *mut Session {
    let config = parse_config(config_ptr, config_len);
    let store: Arc<dyn OffloadStore> = match config.get("offload_dir").and_then(Value::as_str) {
        Some(dir) => Arc::new(Store::persistent(dir, OFFLOAD_TTL)),
        None => Arc::new(Store::default()),
    };
    build_session(
        &config,
        store,
        Some(Codec {
            ctx: ctx as usize,
            encode,
            decode,
        }),
    )
}

fn parse_config(ptr: *const u8, len: usize) -> Value {
    read_input(ptr, len)
        .and_then(|b| serde_json::from_slice(b).ok())
        .unwrap_or_else(|| json!({}))
}

fn build_session(
    config: &Value,
    store: Arc<dyn OffloadStore>,
    codec: Option<Codec>,
) -> *mut Session {
    Box::into_raw(Box::new(Session {
        freeze: FreezeState::default(),
        store,
        resolver: config
            .get("resolver")
            .and_then(Value::as_str)
            .map(str::to_string),
        home: config
            .get("home")
            .and_then(Value::as_str)
            .map(PathBuf::from),
        // Best-of-N proposer search on unless the host disables it; output is proven lossless, so
        // this is a cost/latency preference, not a safety switch.
        proposers: config
            .get("proposers")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        codec,
    }))
}

/// Close a session opened by sw_session_new.
#[unsafe(no_mangle)]
pub extern "C" fn sw_session_free(session: *mut Session) {
    if !session.is_null() {
        unsafe {
            drop(Box::from_raw(session));
        }
    }
}

/// Rewrite a whole request in place: compress every tool-output block (the shaper locates them in
/// whatever wire shape the request uses). Input is the UTF-8 JSON request body; returns
/// `{"request": <rewritten body>, "stats": {...}}`. Needs a session handle.
#[unsafe(no_mangle)]
pub extern "C" fn sw_rewrite(
    session: *mut Session,
    input_ptr: *const u8,
    input_len: usize,
    out_len: *mut usize,
) -> *mut u8 {
    let input = read_input(input_ptr, input_len);
    let json = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rewrite_request(session, input)
    }))
    .unwrap_or_else(|_| error("panic in core"));
    into_owned(json.into_bytes(), out_len)
}

fn rewrite_request(session: *mut Session, input: Option<&[u8]>) -> String {
    if session.is_null() {
        return error("null session handle");
    }
    let session = unsafe { &*session };
    let Some(mut body) = input.and_then(|b| serde_json::from_slice::<Value>(b).ok()) else {
        return error("input is not valid JSON");
    };
    let mut optimizer = optimizer().with_shared_store(session.store.clone());
    optimizer.set_proposers_enabled(session.proposers);
    if let Some(codec) = &session.codec {
        let (ctx, encode, decode) = (codec.ctx, codec.encode, codec.decode);
        optimizer = optimizer.with_text_proposer(Box::new(CallbackProposer::new(
            move |raw: &str| call_codec(encode, ctx, raw),
            move |wire: &str| call_codec(decode, ctx, wire),
        )));
    }
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if !model.is_empty() {
        optimizer.set_model(&model);
    }
    session.freeze.bound(FREEZE_CAP, SEEN_CAP);
    let stats = rewrite(
        &mut body,
        &mut optimizer,
        session.resolver.as_deref(),
        &session.freeze,
    );

    // Only a block's first sight books a row, matching the proxy, so a resend is never re-counted.
    if let Some(home) = &session.home {
        for block in stats.iter().filter(|s| s.first_seen) {
            events::record(
                home,
                &events::Event {
                    at_ms: events::now_ms(),
                    surface: "library".into(),
                    transform: block.transform.clone(),
                    input_tokens: block.input_units as u64,
                    output_tokens: block.output_units as u64,
                    saved_usd: block.saved_usd,
                    verified: true,
                    inline: block.inline,
                    atoms: block.atoms,
                    cert: block.cert.clone(),
                    model: model.clone(),
                    platform: String::new(),
                    tenant: String::new(),
                    kept_reason: block.kept_reason.clone(),
                    req_id: String::new(),
                },
            );
        }
    }

    // Count once per block (a resend re-emits the frozen wire, never re-booked). A kept/refused block
    // rewrote nothing, so it is excluded from the rewrite counts and token totals and reported apart.
    let compressed = stats.iter().filter(|s| s.kept_reason.is_empty());
    let first_seen = compressed.clone().filter(|s| s.first_seen);
    let input_tokens: u64 = first_seen.clone().map(|s| s.input_units as u64).sum();
    let output_tokens: u64 = first_seen.clone().map(|s| s.output_units as u64).sum();
    json!({
        "request": body,
        "stats": {
            "blocks_rewritten": compressed.clone().count(),
            "blocks_first_seen": first_seen.clone().count(),
            "blocks_kept": stats.iter().filter(|s| !s.kept_reason.is_empty()).count(),
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "tokens_saved": input_tokens.saturating_sub(output_tokens),
            "transforms": compressed.map(|s| s.transform.as_str()).collect::<Vec<_>>(),
        },
    })
    .to_string()
}

/// Allocate `len` zeroed library-owned bytes (null if `len` is 0), released with sw_free. Exists for
/// wasm hosts, which can't write the module's linear memory except through it; native FFI callers own
/// their own buffers and never need this.
#[unsafe(no_mangle)]
pub extern "C" fn sw_alloc(len: usize) -> *mut u8 {
    if len == 0 {
        return std::ptr::null_mut();
    }
    Box::into_raw(vec![0u8; len].into_boxed_slice()) as *mut u8
}

/// Release a buffer returned by sw_compress or sw_verify, or one obtained from sw_alloc. Call
/// exactly once per pointer, with the same `len` it was created with.
#[unsafe(no_mangle)]
pub extern "C" fn sw_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)));
    }
}

fn compress_json(input: Option<&[u8]>) -> String {
    let Some(request) = input.and_then(|b| serde_json::from_slice::<Value>(b).ok()) else {
        return error("input is not valid JSON");
    };
    let Some(block) = request.get("block").and_then(Value::as_str) else {
        return error("missing string field: block");
    };
    let mut optimizer = optimizer();
    if let Some(model) = request.get("model").and_then(Value::as_str) {
        optimizer.set_model(model);
    }
    let outcome = optimizer.compress_block(block);
    outcome_json(block, &outcome, &optimizer).to_string()
}

// Reports tokens, the billed unit. saved_usd stays internal to the net-cost gate's decision.
fn outcome_json(raw: &str, outcome: &Outcome, optimizer: &Optimizer) -> Value {
    let input_tokens = optimizer.count(raw);
    match outcome {
        Outcome::Compressed {
            wire, transform, ..
        } => {
            let output_tokens = optimizer.count(wire);
            json!({
                "kind": "compressed",
                "transform": transform,
                "wire": wire,
                "input_bytes": raw.len(),
                "wire_bytes": wire.len(),
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "tokens_saved": input_tokens.saturating_sub(output_tokens),
                // Portable proof (hash of the canonical original) that sw_verify checks; the Outcome's
                // own certificate is the internal CLMH admission cert.
                "certificate": { "hash": certificate::certify(raw).hash },
            })
        }
        Outcome::Offloaded { stub, marker, .. } => {
            let output_tokens = optimizer.count(stub);
            json!({
                "kind": "offloaded",
                "stub": stub,
                "marker": marker,
                "input_bytes": raw.len(),
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "tokens_saved": input_tokens.saturating_sub(output_tokens),
            })
        }
        Outcome::KeptVerbatim { .. } => json!({
            "kind": "verbatim",
            "input_bytes": raw.len(),
            "input_tokens": input_tokens,
            "tokens_saved": 0,
        }),
    }
}

fn verify_json(input: Option<&[u8]>) -> String {
    let Some(request) = input.and_then(|b| serde_json::from_slice::<Value>(b).ok()) else {
        return error("input is not valid JSON");
    };
    let (Some(wire), Some(hash)) = (
        request.get("wire").and_then(Value::as_str),
        request.get("hash").and_then(Value::as_str),
    ) else {
        return error("missing string fields: wire, hash");
    };
    let ok = certificate::verify(
        wire,
        &Certificate {
            hash: hash.to_string(),
        },
    );
    json!({ "ok": ok }).to_string()
}

fn read_input<'a>(ptr: *const u8, len: usize) -> Option<&'a [u8]> {
    (!ptr.is_null()).then(|| unsafe { slice::from_raw_parts(ptr, len) })
}

fn into_owned(bytes: Vec<u8>, out_len: *mut usize) -> *mut u8 {
    let boxed = bytes.into_boxed_slice();
    if !out_len.is_null() {
        unsafe {
            *out_len = boxed.len();
        }
    }
    Box::into_raw(boxed) as *mut u8
}

fn error(message: &str) -> String {
    json!({ "kind": "error", "error": message }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_then_verify_roundtrips_over_the_contract() {
        let raw: String = format!(
            "[{}]",
            (0..40)
                .map(|i| format!(r#"{{"id":{i},"path":"file-{i}.txt","state":"open"}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        let request = json!({ "block": raw }).to_string();
        let out: Value = serde_json::from_str(&compress_json(Some(request.as_bytes()))).unwrap();
        // The contract under test is compress-then-verify; whichever inline codec wins must produce a
        // wire that independently verifies against its portable certificate.
        assert_eq!(out["kind"], "compressed");

        let verify_req =
            json!({ "wire": out["wire"], "hash": out["certificate"]["hash"] }).to_string();
        let verdict: Value =
            serde_json::from_str(&verify_json(Some(verify_req.as_bytes()))).unwrap();
        assert_eq!(
            verdict["ok"], true,
            "the wire must independently verify lossless"
        );
    }

    #[test]
    fn rewrite_compresses_a_tool_output_inside_a_whole_request() {
        let session = Box::into_raw(Box::new(Session {
            freeze: FreezeState::default(),
            store: Arc::new(Store::default()),
            resolver: None,
            home: None,
            proposers: true,
            codec: None,
        }));
        let ls: String = format!(
            "[{}]",
            (0..40)
                .map(|i| format!(r#"{{"id":{i},"path":"file-{i}.txt","state":"open"}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        let request = json!({
            "model": "gpt-4o",
            "messages": [
                { "role": "user", "content": "ls" },
                { "role": "tool", "tool_call_id": "c1", "content": ls },
            ],
        })
        .to_string();

        let out: Value =
            serde_json::from_str(&rewrite_request(session, Some(request.as_bytes()))).unwrap();
        assert_eq!(out["stats"]["blocks_rewritten"], 1);
        assert_eq!(out["stats"]["transforms"][0], "columnar");
        let tool = out["request"]["messages"][1]["content"].as_str().unwrap();
        assert!(tool.len() < ls.len(), "the tool output must shrink");
        assert_eq!(
            out["request"]["messages"][0]["content"], "ls",
            "other messages untouched"
        );

        unsafe { drop(Box::from_raw(session)) };
    }

    #[test]
    fn a_resend_through_a_session_is_byte_identical_and_counted_once() {
        // Prompt-cache safety: the same request rewritten twice through one session must re-emit
        // byte-identical bytes (so the provider's cached prefix holds) and never re-book the block.
        let session = Box::into_raw(Box::new(Session {
            freeze: FreezeState::default(),
            store: Arc::new(Store::default()),
            resolver: None,
            home: None,
            proposers: true,
            codec: None,
        }));
        let ls: String = (0..40)
            .map(|i| {
                format!(
                    "{:>8} -rw-r--r-- 1 root wheel {:>6} file-{i}.txt",
                    13000 + i * 137,
                    100 + i * 37
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let request = json!({
            "model": "gpt-4o",
            "messages": [
                { "role": "user", "content": "ls" },
                { "role": "tool", "tool_call_id": "c1", "content": ls },
            ],
        })
        .to_string();

        let first: Value =
            serde_json::from_str(&rewrite_request(session, Some(request.as_bytes()))).unwrap();
        let second: Value =
            serde_json::from_str(&rewrite_request(session, Some(request.as_bytes()))).unwrap();
        assert_eq!(
            first["request"], second["request"],
            "a resend must be byte-identical"
        );
        assert_eq!(
            second["stats"]["blocks_first_seen"], 0,
            "a resend is not re-counted"
        );

        unsafe { drop(Box::from_raw(session)) };
    }

    #[test]
    fn sw_alloc_hands_out_a_writable_buffer_and_sw_free_reclaims_it() {
        assert!(
            sw_alloc(0).is_null(),
            "a zero-length request has no buffer to own"
        );
        let ptr = sw_alloc(64);
        assert!(!ptr.is_null());
        unsafe { slice::from_raw_parts_mut(ptr, 64).fill(0xab) };
        sw_free(ptr, 64);
    }

    #[test]
    fn bad_input_returns_an_error_object_not_a_trap() {
        let out: Value = serde_json::from_str(&compress_json(Some(b"not json"))).unwrap();
        assert_eq!(out["kind"], "error");
        let out: Value = serde_json::from_str(&compress_json(None)).unwrap();
        assert_eq!(out["kind"], "error");
    }
}
