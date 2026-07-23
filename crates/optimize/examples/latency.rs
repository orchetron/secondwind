// Per-block compression latency: the codec cost (Off mode = inline compress + self-verify decode) and
// the Auto coverage-gate cost (covers_content + preview) it adds on top. Wall-clock, warmed, sorted.
// cargo run -p secondwind-optimize --example latency --release -- <files...>
use secondwind_optimize::offload::covering_preview;
use secondwind_optimize::{OffloadMode, Optimizer, Outcome};
use std::time::Instant;

fn stats(mut us: Vec<f64>) -> (f64, f64, f64) {
    us.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = us.len();
    let mean = us.iter().sum::<f64>() / n as f64;
    (mean, us[n / 2], us[(n * 99 / 100).min(n - 1)])
}

// Time f over an iteration count scaled so every workload does a comparable amount of total work.
fn time_each(raw: &str, mut f: impl FnMut()) -> (f64, f64, f64) {
    let iters = (20_000_000 / raw.len().max(1)).clamp(20, 3000);
    for _ in 0..3 {
        f();
    }
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        f();
        samples.push(t.elapsed().as_secs_f64() * 1e6);
    }
    stats(samples)
}

fn main() {
    println!(
        "{:24} {:>9} {:>7} {:>18} {:>10} {:>16}  codec",
        "workload", "bytes", "MB/s", "codec p50/p99 us", "gate p50", "gate %"
    );
    for path in std::env::args().skip(1) {
        let raw = std::fs::read_to_string(&path).unwrap_or_default();
        if raw.trim().is_empty() {
            continue;
        }

        let codec = match Optimizer::default().compress_block(&raw) {
            Outcome::Compressed { transform, .. } => transform.to_string(),
            Outcome::Offloaded { .. } => "offload".into(),
            Outcome::KeptVerbatim { .. } => "verbatim".into(),
        };

        // Off isolates the inline codec: compress plus its own decode==raw self-verify, no store I/O.
        let (c_mean, c_p50, c_p99) = time_each(&raw, || {
            let mut opt = Optimizer::default();
            opt.set_offload_mode(OffloadMode::Off);
            let _ = opt.compress_block(&raw);
        });
        // The Auto gate added to every inline-shipping block: the fused content-coverage check.
        let (g_mean, g_p50, _g_p99) = time_each(&raw, || {
            let _ = covering_preview(&raw).is_some();
        });

        let mbps = (raw.len() as f64 / 1e6) / (c_mean / 1e6);
        let gate_pct = 100.0 * g_mean / c_mean;
        let name = path.rsplit('/').next().unwrap_or(&path);
        println!(
            "{name:24} {:>9} {mbps:>7.1} {:>8.1}/{:>7.1} {g_p50:>10.2} {gate_pct:>15.1}%  {codec}",
            raw.len(),
            c_p50,
            c_p99
        );
    }
}
