use std::sync::Arc;

use secondwind_ledger::events;
use secondwind_optimize::Optimizer;
use secondwind_optimize::proxy::{losslessly_equivalent, rewrite};
use serde_json::Value;

use super::{AppState, FREEZE_CAP, SEEN_CAP};

// The one shape-and-book path both transports share, run on the blocking pool. Returns the
// bytes to forward: rewritten normally, original under observe or when nothing changed.
pub(crate) async fn shape(
    state: Arc<AppState>,
    platform: String,
    tenant: String,
    body: Vec<u8>,
) -> Vec<u8> {
    tokio::task::spawn_blocking(move || {
        // Fail-open: a transform panic forwards the original bytes. Freeze locks are
        // poison-tolerant, so the next request still recovers the shared map.
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            shape_blocking(&state, &platform, &tenant, &body)
        })) {
            Ok(Some(rewritten)) => rewritten,
            Ok(None) | Err(_) => body,
        }
    })
    .await
    .unwrap_or_default()
}

// Some(bytes) when the body was rewritten; None to forward the caller's original bytes (a
// non-JSON body, observe mode, or nothing compressed).
fn shape_blocking(state: &AppState, platform: &str, tenant: &str, body: &[u8]) -> Option<Vec<u8>> {
    let mut json = serde_json::from_slice::<Value>(body).ok()?;
    let original = json.clone();

    let mut optimizer = state.shaping.apply(
        Optimizer::default()
            .with_counter(state.counter.clone())
            .with_shared_store(state.store.clone()),
    );

    // Record the model the request names even when it's not in the rate table (pricing falls back).
    let requested_model = json
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(model) = &requested_model {
        optimizer.set_model(model);
    }
    let model = requested_model.unwrap_or_else(|| optimizer.model().to_string());

    // Bound the shared memory before rewrite takes its own fine-grained locks, so concurrent
    // sessions never serialize.
    state.freeze.bound(FREEZE_CAP, SEEN_CAP);

    let blocks = rewrite(
        &mut json,
        &mut optimizer,
        state.shaping.resolver.as_deref(),
        &state.freeze,
    );
    let surface = if state.shaping.observe {
        "observe"
    } else {
        "serve"
    };
    // A kept/refused block leaves the body unchanged, so `changed` tracks only real rewrites: an
    // unchanged body must forward the caller's original bytes, never a re-serialized copy.
    let changed = blocks.iter().any(|b| b.kept_reason.is_empty());
    // Fail closed: prove the rewrite differs from the original only in leaves that reconstruct or
    // recover. On failure forward the exact original, and book the rejection so the gate catching a
    // codec or plumbing bug is visible, not swallowed.
    if changed && !losslessly_equivalent(&original, &json, state.store.as_ref()) {
        state
            .counters
            .self_proof_rejected
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        events::record(
            &state.home,
            &events::Event {
                at_ms: events::now_ms(),
                surface: surface.into(),
                kept_reason: "self_proof_reject".into(),
                model: model.clone(),
                platform: platform.to_string(),
                tenant: tenant.to_string(),
                ..Default::default()
            },
        );
        return None;
    }
    let req_id = format!(
        "r{:x}",
        state
            .counters
            .req_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let (mut seen_in, mut seen_out) = (0u64, 0u64);
    // Only a block's first sight books a feed row; a resend is never re-counted.
    for block in blocks {
        if !block.first_seen {
            continue;
        }
        // A kept block changed nothing, so it stays out of the compression totals but is still booked
        // with its reason (verified: an untouched block is trivially lossless).
        if block.kept_reason.is_empty() {
            seen_in += block.input_units as u64;
            seen_out += block.output_units as u64;
        }
        events::record(
            &state.home,
            &events::Event {
                at_ms: events::now_ms(),
                surface: surface.into(),
                transform: block.transform,
                input_tokens: block.input_units as u64,
                output_tokens: block.output_units as u64,
                saved_usd: block.saved_usd,
                verified: true,
                inline: block.inline,
                atoms: block.atoms,
                cert: block.cert,
                model: model.clone(),
                platform: platform.to_string(),
                tenant: tenant.to_string(),
                kept_reason: block.kept_reason,
                req_id: req_id.clone(),
            },
        );
    }

    if state.shaping.observe {
        if seen_in > 0 {
            let pct = 100.0 * (seen_in - seen_out) as f64 / seen_in.max(1) as f64;
            eprintln!(
                "secondwind observe: would compress {seen_in} -> {seen_out} tokens ({pct:.1}%), forwarding original untouched"
            );
        }
        None
    } else if changed {
        serde_json::to_vec(&json).ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use secondwind_optimize::offload::{OffloadError, OffloadStore, Offloaded, Store};
    use secondwind_optimize::proxy::FreezeState;
    use secondwind_optimize::tokens::Tiktoken;
    use serde_json::json;

    use super::super::{Counters, Shaping};
    use super::*;

    // Persists normally but resolve is broken (bytes stored, unretrievable): the availability
    // failure the whole-request self-proof must catch, forcing the proxy to forward the original.
    struct BrokenResolve(Store);
    impl OffloadStore for BrokenResolve {
        fn offload(&self, raw: &str) -> Result<Offloaded, OffloadError> {
            self.0.offload(raw)
        }
        fn resolve(&self, _marker: &str) -> Option<String> {
            None
        }
        fn covers(&self, marker: &str, original: &str) -> bool {
            self.0.covers(marker, original)
        }
        fn prospective_stub_len(&self, raw: &str) -> Option<(usize, usize)> {
            self.0.prospective_stub_len(raw)
        }
    }

    fn temp_home() -> std::path::PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let home = std::env::temp_dir().join(format!(
            "sw-selfproof-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    #[test]
    fn a_self_proof_rejection_forwards_the_original_and_books_it() {
        let resolver = "mcp__secondwind__resolve";
        let home = temp_home();
        let state = AppState {
            counter: Arc::new(Tiktoken::cl100k()),
            freeze: Arc::new(FreezeState::default()),
            counters: Arc::new(Counters::default()),
            store: Arc::new(BrokenResolve(Store::default())),
            dir: home.clone(),
            home: home.clone(),
            upstream: "http://upstream.invalid".into(),
            rules: Vec::new(),
            shaping: Shaping {
                resolver: Some(resolver.into()),
                ..Default::default()
            },
            http: reqwest::Client::new(),
        };

        // A covering records block offloads on first sight, but the broken store can't resolve the
        // marker, so the whole-request lossless self-proof rejects the rewrite.
        let content = format!(
            "[{}]",
            (0..60)
                .map(|i| format!(
                    r#"{{"id":{i},"state":"open","title":"r{i}","body":"{}"}}"#,
                    "long boilerplate detail text repeated for bulk ".repeat(8)
                ))
                .collect::<Vec<_>>()
                .join(",")
        );
        let body = json!({
            "tools": [{"name": resolver, "description": "swload fetch"}],
            "messages": [{"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": content}
            ]}]
        });
        let original = serde_json::to_vec(&body).unwrap();

        let out = shape_blocking(&state, "test", "", &original);
        assert!(
            out.is_none(),
            "a rejected rewrite forwards the caller's original bytes"
        );
        assert_eq!(
            state.counters.self_proof_rejected.load(Ordering::Relaxed),
            1,
            "the rejection is counted"
        );
        let events = secondwind_ledger::events::load(&home);
        assert!(
            events.iter().any(|e| e.kept_reason == "self_proof_reject"),
            "the rejection is booked as an event, not swallowed"
        );

        let _ = std::fs::remove_dir_all(&home);
    }
}
