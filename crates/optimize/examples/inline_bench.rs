// Per-workload token reduction, two honest columns that are never blended: inline (Off mode =
// lossless in-place compression, every value present, no round-trip) and offload (Always mode =
// full recoverable eviction to a marker the agent resolves on demand -- removed, not compressed).
// The default Auto mode picks between them per block; this shows both endpoints.
// cargo run -p secondwind-optimize --example inline_bench --release --features tiktoken -- <files...>
use secondwind_optimize::tokens::Tiktoken;
use secondwind_optimize::{OffloadMode, Optimizer, Outcome};
use std::sync::Arc;

fn main() {
    let counter = Arc::new(Tiktoken::cl100k());
    println!(
        "{:26} {:>8} {:>9} {:>10}  codec",
        "workload", "in_tok", "inline%", "offload%"
    );
    for path in std::env::args().skip(1) {
        let raw = std::fs::read_to_string(&path).unwrap_or_default();
        if raw.trim().is_empty() {
            continue;
        }
        let it = Optimizer::default()
            .with_counter(counter.clone())
            .count(&raw);

        let mut inline = Optimizer::default().with_counter(counter.clone());
        inline.set_offload_mode(OffloadMode::Off);
        let (inline_out, codec) = match inline.compress_block(&raw) {
            Outcome::Compressed {
                wire, transform, ..
            } => (inline.count(&wire), transform.to_string()),
            _ => (it, "verbatim".into()),
        };

        let mut offloaded = Optimizer::default().with_counter(counter.clone());
        offloaded.set_offload_mode(OffloadMode::Always);
        let offload_out = match offloaded.compress_block(&raw) {
            Outcome::Compressed { wire, .. } => offloaded.count(&wire),
            Outcome::Offloaded { stub, .. } => offloaded.count(&stub),
            Outcome::KeptVerbatim { .. } => it,
        };

        let pct = |o: usize| {
            if it > 0 {
                100.0 * (it as f64 - o as f64) / it as f64
            } else {
                0.0
            }
        };
        let name = path.rsplit('/').next().unwrap_or(&path);
        println!(
            "{name:26} {it:>8} {:>8.1}% {:>9.1}%  {codec}",
            pct(inline_out),
            pct(offload_out)
        );
    }
}
