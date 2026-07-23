# Compression benchmark

Runs the optimizer over a corpus of tool-output shapes and reports byte reduction
alongside fidelity. Fidelity is every significant value (length >= 6, deduped)
that survives inline or is recovered by resolving the offload marker; the test
fails if any value is lost, so a savings figure is only ever reported next to
verified-lossless output.

Corpus in `./corpus` (synthetic): a high-cardinality array, a low-cardinality
array, a flat object, and a small array.

Run: `cargo test -p secondwind-optimize --test compression_bench -- --nocapture`

## Measured

| shape | bytes in | bytes out | saving | transform | values kept |
|---|---|---|---|---|---|
| high-cardinality array | 46706 | 20252 | 56.6% | columnar (inline) | 991/991 |
| low-cardinality array | 44611 | 5193 | 88.4% | columnar (inline) | 11/11 |
| flat object | 47518 | 2465 | 94.8% | offload (recoverable) | 1200/1200 |
| small array | 36 | 35 | 2.8% | columnar (inline) | 0/0 |

Inline transforms keep every value present in the emitted block. Offload replaces
the block with a values-omitted preview plus a marker that resolves to the exact
bytes, so the values are recoverable rather than inline. Both paths are byte-exact
and covered by the round-trip property suite (`crates/optimize/tests/fuzz.rs`).

Byte reduction is the metric here. On the wire the billed unit is tokens;
`bench/token_bench.py` measures token reduction over a tool-output corpus with the same
per-block fidelity check (see [../BENCHMARK.md](../BENCHMARK.md)).

## Real agent workloads (token reduction)

`gen_workloads.sh` rebuilds a real corpus from public/safe sources only: this repository itself
(cargo metadata, dependency tree, lockfiles, code search, file listings, git log/diff) plus, if the
network is available, public GitHub PRs/issues, npm and PyPI registry responses, and a Project
Gutenberg text. Output goes to `./workloads`, which is gitignored, so no personal environment or
process data is ever committed.

```sh
bash bench/compression/gen_workloads.sh
cargo run -p secondwind-optimize --example inline_bench --release --features tiktoken -- \
  bench/compression/workloads/*
```

`inline_bench` reports two columns per workload, never blended: **inline** (offload disabled = lossless
in-place compression, every value present) and **offload** (recoverable eviction to a marker). Counted
with the cl100k tokenizer. The recorded table is in [../../BENCHMARKS.md](../../BENCHMARKS.md);
repo-local rows are near-deterministic and the live public-data rows vary a few points run to run.
