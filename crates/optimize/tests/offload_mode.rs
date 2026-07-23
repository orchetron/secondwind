use secondwind_optimize::{OffloadMode, Optimizer, Outcome};
use serde_json::{Value, json};

// A compact record array: columnar compresses it well and the reopen cost outweighs the small win,
// so Auto keeps it inline. This is where Always visibly diverges from Auto.
fn inline_array() -> String {
    let records: Vec<Value> = (0..25)
        .map(|i| json!({"id": i, "name": format!("item_{i}"), "ok": i % 2 == 0}))
        .collect();
    Value::Array(records).to_string()
}

// A record array with a content-covering preview: Auto evicts it because the preview answers most
// lookups and the reopen cost undercuts the inline wire.
fn record_array() -> String {
    let records: Vec<Value> = (0..60)
        .map(|i| {
            json!({
                "id": i,
                "state": if i % 2 == 0 { "open" } else { "closed" },
                "title": format!("change number {i} to the pipeline"),
                "body": "x".repeat(300),
            })
        })
        .collect();
    Value::Array(records).to_string()
}

fn mode(block: &str, mode: OffloadMode) -> Outcome {
    let mut optimizer = Optimizer::default();
    optimizer.set_offload_mode(mode);
    optimizer.compress_block(block)
}

#[test]
fn auto_is_the_default_mode() {
    assert_eq!(Optimizer::default().offload_mode(), OffloadMode::Auto);
}

#[test]
fn off_never_evicts_even_when_auto_would() {
    // The array offloads under Auto; Off must keep it inline instead of falling to verbatim.
    assert!(matches!(
        mode(&record_array(), OffloadMode::Auto),
        Outcome::Offloaded { .. }
    ));
    assert!(matches!(
        mode(&record_array(), OffloadMode::Off),
        Outcome::Compressed { .. }
    ));
}

#[test]
fn always_evicts_a_block_auto_ships_inline() {
    // Auto keeps the compact array inline (offload is not cheaper); Always forces the eviction.
    assert!(matches!(
        mode(&inline_array(), OffloadMode::Auto),
        Outcome::Compressed { .. }
    ));
    assert!(matches!(
        mode(&inline_array(), OffloadMode::Always),
        Outcome::Offloaded { .. }
    ));
}

#[test]
fn off_suppresses_the_relevance_split_offload() {
    // The query-aware path evicts non-matching rows under Auto; Off must not offload on any path.
    let block = record_array();
    let mut auto = Optimizer::default();
    assert!(matches!(
        auto.compress_block_with_query(&block, "change 3"),
        Outcome::Offloaded { .. }
    ));
    let mut off = Optimizer::default();
    off.set_offload_mode(OffloadMode::Off);
    assert!(!matches!(
        off.compress_block_with_query(&block, "change 3"),
        Outcome::Offloaded { .. }
    ));
}

#[test]
fn always_keeps_inline_below_the_offload_floor() {
    // Too small to offload: Always falls back to the inline wire, never verbatim.
    let tiny = json!([{"a": 1, "b": 2}, {"a": 3, "b": 4}]).to_string();
    assert!(!matches!(
        mode(&tiny, OffloadMode::Always),
        Outcome::Offloaded { .. }
    ));
}

#[test]
fn compat_shim_maps_bool_to_off_and_auto() {
    let mut opt = Optimizer::default();
    opt.set_offload_allowed(false);
    assert_eq!(opt.offload_mode(), OffloadMode::Off);
    opt.set_offload_allowed(true);
    assert_eq!(opt.offload_mode(), OffloadMode::Auto);
}

// A forced eviction still resolves back to its exact bytes: mode changes routing, never fidelity.
#[test]
fn always_eviction_round_trips() {
    let block = inline_array();
    let mut optimizer = Optimizer::default();
    optimizer.set_offload_mode(OffloadMode::Always);
    let Outcome::Offloaded { marker, .. } = optimizer.compress_block(&block) else {
        panic!("Always should evict the array block");
    };
    assert_eq!(optimizer.resolve(&marker).as_deref(), Some(block.as_str()));
}

#[test]
fn resolve_selected_fetches_one_field_and_keeps_the_whole_block_byte_exact() {
    // Per-section resolve: one marker, but the agent can pull just one record's field instead of the
    // whole block, and resolving the marker alone still returns the original bytes exactly.
    let block = format!(
        "[{}]",
        (0..40)
            .map(|i| format!(r#"{{"n":{i},"body":"detail for row {i} long enough to matter"}}"#))
            .collect::<Vec<_>>()
            .join(",")
    );
    let mut optimizer = Optimizer::default();
    optimizer.set_offload_mode(OffloadMode::Always);
    let Outcome::Offloaded { marker, .. } = optimizer.compress_block(&block) else {
        panic!("expected offload");
    };
    assert_eq!(
        optimizer.resolve_selected(&marker, "n=7/body").as_deref(),
        Some("detail for row 7 long enough to matter")
    );
    assert_eq!(
        optimizer.resolve_selected(&marker, "").as_deref(),
        Some(block.as_str()),
        "empty selector returns the whole block byte-exact"
    );
    assert_eq!(optimizer.resolve_selected(&marker, "n=999/body"), None);
}
