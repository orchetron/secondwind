use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use secondwind_optimize::offload::{OffloadError, OffloadStore, Offloaded, Store};
use secondwind_optimize::tokens::ByteCounter;
use secondwind_optimize::transform::{Encoded, Transform};
use secondwind_optimize::{Optimizer, Outcome};
use serde_json::{Value, json};

// Example adopter transform: pack an all-string JSON object into key=value lines. Keeps every key
// and value visible so it stays lossless and the detector backstop sees no dropped artifact.
struct KvPack;

impl Transform for KvPack {
    fn id(&self) -> &'static str {
        "kv-pack"
    }
    fn try_encode(&self, value: &Value) -> Option<Encoded> {
        let obj = value.as_object()?;
        if obj.len() < 3 || !obj.values().all(Value::is_string) {
            return None;
        }
        let wire = obj
            .iter()
            .map(|(k, v)| format!("{k}={}", v.as_str().unwrap()))
            .collect::<Vec<_>>()
            .join("\n");
        Some(Encoded {
            wire,
            decoded: value.clone(),
        })
    }
}

fn kv_block() -> String {
    let mut obj = serde_json::Map::new();
    for i in 0..30 {
        obj.insert(
            format!("path_{i:02}"),
            json!(format!("/srv/app/module/service_{i:02}/config.yaml")),
        );
    }
    Value::Object(obj).to_string()
}

#[test]
fn a_custom_transform_is_applied() {
    // Offload disabled to isolate the composability path: the weak KvPack fixture barely shrinks, so
    // with a resolver the cost model would (correctly) evict instead; here we test it ships inline.
    let mut optimizer = Optimizer::default().with_transform(Box::new(KvPack));
    optimizer.set_offload_allowed(false);
    match optimizer.compress_block(&kv_block()) {
        Outcome::Compressed {
            transform, wire, ..
        } => {
            assert_eq!(transform, "kv-pack");
            assert!(wire.contains("path_00=/srv/app/module/service_00/config.yaml"));
        }
        _ => panic!("expected the custom transform to compress the block"),
    }
}

#[test]
fn with_counter_does_not_drop_a_custom_transform() {
    // Builder order-independent: the custom transform survives with_counter's columnar rebuild.
    let before = Optimizer::default()
        .with_transform(Box::new(KvPack))
        .with_counter(Arc::new(ByteCounter));
    let after = Optimizer::default()
        .with_counter(Arc::new(ByteCounter))
        .with_transform(Box::new(KvPack));
    for mut optimizer in [before, after] {
        optimizer.set_offload_allowed(false);
        match optimizer.compress_block(&kv_block()) {
            Outcome::Compressed { transform, .. } => assert_eq!(transform, "kv-pack"),
            _ => panic!("custom transform lost after with_counter"),
        }
    }
}

// Example adopter store backend (here local disk; could be Redis/object storage):
// every offload and resolve flows through it.
struct CountingStore {
    inner: Store,
    offloads: Arc<AtomicUsize>,
}

impl OffloadStore for CountingStore {
    fn offload(&self, raw: &str) -> Result<Offloaded, OffloadError> {
        self.offloads.fetch_add(1, Ordering::SeqCst);
        self.inner.offload(raw)
    }
    fn resolve(&self, marker: &str) -> Option<String> {
        self.inner.resolve(marker)
    }
    fn covers(&self, marker: &str, original: &str) -> bool {
        self.inner.covers(marker, original)
    }
    fn prospective_stub_len(&self, raw: &str) -> Option<(usize, usize)> {
        self.inner.prospective_stub_len(raw)
    }
}

#[test]
fn a_custom_store_backs_offload_and_resolve() {
    let offloads = Arc::new(AtomicUsize::new(0));
    let store = CountingStore {
        inner: Store::default(),
        offloads: offloads.clone(),
    };
    let mut optimizer = Optimizer::default().with_store(store);

    let bulk = "x".repeat(20_000);
    let Some(Outcome::Offloaded { marker, .. }) = optimizer.offload_repeat(&bulk) else {
        panic!("expected the block to offload through the custom store");
    };
    assert!(
        offloads.load(Ordering::SeqCst) >= 1,
        "offload went through the custom store"
    );
    assert_eq!(
        optimizer.resolve(&marker).as_deref(),
        Some(bulk.as_str()),
        "the marker resolves through the custom store"
    );
}
