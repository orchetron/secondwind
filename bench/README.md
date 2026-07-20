# secondwind benchmarks

Reproducible from this repository, no external services.

- **relevance/**: ranking quality of which tool-output rows to keep inline, over two
  labeled corpora (easy, hard). Metrics match the Rust test and the Python scorer so
  numbers compare directly.
  `cargo test -p secondwind-optimize --test relevance_bench -- --nocapture`
- **compression/**: byte reduction and verified-lossless fidelity across tool-output
  shapes.
  `cargo test -p secondwind-optimize --test compression_bench -- --nocapture`
- **token_bench.py**: token reduction (the billed unit on the wire) with per-block
  fidelity across a tool-output corpus.
  `cargo build -p secondwind && python3 bench/token_bench.py target/debug/secondwind`
  (see [BENCHMARK.md](BENCHMARK.md)).

Lossless and admission guarantees are also enforced by the round-trip property suite
(`crates/optimize/tests/fuzz.rs`) and the admission tests
(`crates/optimize/tests/admission.rs`).

Real-traffic measurements (cache-adjusted net dollars, offload reopen rate, and
task-success replay) are produced by the shipped CLI over your own sessions
(`secondwind cache-savings`, `reopen-rate`, `replay`), not from a committed corpus.
