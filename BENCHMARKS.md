# Benchmarks

Every number below is reproducible from this repo. Rig: Apple M3 (8 cores), release build,
warm process. Latency figures count tokens through the shipped tokenizer (the net-cost gate).

## Token reduction on real agent workloads

Two columns, never blended. **Inline** is lossless compression: the block stays in the window, every
value present and blake3-verified, readable at face value, no round-trip. **Offload** is recoverable
eviction: the block moves to a marker the agent resolves on demand, so it is removed-and-recoverable,
not compressed. The columns are the two endpoints (offload disabled vs forced); the default `Auto`
mode picks between them per block. Counted with the cl100k tokenizer.

| Shape | Workload | in tokens | Inline (lossless) | Offload (recoverable) |
|---|---|---:|---:|---:|
| Record arrays (JSON) | GitHub pull requests | 312,440 | **38.8%** | 97.9% |
| | GitHub issues | 127,353 | **15.7%** | 95.2% |
| | cargo metadata | 333,681 | **3.0%** | 100.0% |
| | GitHub PRs, flattened | 3,095 | **56.1%** | 84.8% |
| Object-map JSON | npm registry response | 2,758,223 | **8.4%** | 100.0% |
| | PyPI registry response | 81,071 | **19.9%** | 99.9% |
| | package-lock.json | 4,493 | **25.4%** | 98.9% |
| Dependency & lock graphs | cargo tree | 8,755 | **27.0%** | 99.4% |
| | Cargo.lock | 21,829 | **17.1%** | 99.8% |
| Directory & path listings | find | 682 | **37.0%** | 92.8% |
| | git ls-files | 1,393 | **21.7%** | 97.0% |
| | ls -R | 5,647 | – | **98.9%** |
| Code search | grep (path:line:content) | 5,758 | **15.7%** | 91.9% |
| | signatures | 4,429 | **15.8%** | 92.1% |
| Diffs & version control | git log | 4,698 | – | **98.6%** |
| | git diff | 2,879 | – | **94.0%** |
| Free text | prose (public domain) | 34,639 | – | **99.9%** |

Inline reduction is what the model can read at face value: structure removed, every value kept, no
decoder in the loop. That readability bound is why the inline figures are moderate (3-56%) rather than
the higher ratios an unreadable re-encoding would print. Offload is the recoverable ceiling when the
agent carries a resolve tool, and it applies to any shape (85-100%), including the ones with no inline
lever (object maps, recursive listings, free text, shown as `–`).

The default `Auto` mode does not take the offload endpoint blindly: it offloads only when the eviction
preview covers the block's content (record arrays) or the shape has no inline lever, and ships inline
everywhere else, so nothing readable is evicted that could be kept in place.

```sh
bash bench/compression/gen_workloads.sh   # rebuild the corpus from public/safe sources
cargo run -p secondwind-optimize --example inline_bench --release --features tiktoken -- \
  bench/compression/workloads/*
```

The corpus is regenerated from this repository itself plus (optionally) public GitHub, npm, PyPI, and
Project Gutenberg; it is gitignored, so no personal environment or process data is ever committed. The
repo-local rows are near-deterministic; the live public-data rows are representative and vary a few
points run to run.

## Prompt-cache preservation

Rewriting tool output must not disturb a provider's prompt cache: agents resend a growing history every
turn, so a byte change inside the already-cached prefix forces the whole suffix to be re-billed. This
measures effective input cost (cache-weighted) over a growing conversation (12 turns, 11 scored),
secondwind versus sending history verbatim, under a modeled prefix cache (in this model the cache breaks
at the first differing byte; 1024-token minimum; read and create rates taken from the shipped rate table).

The cache guard is on by default: a block's wire form is decided on first sight and frozen, never
rewritten on a later turn, so the cached prefix stays byte-stable.

| configuration | vs baseline effective input cost | cache-bust turns |
|---|---:|---:|
| inline-only (no resolver) | **+22.9% cheaper** | none |
| offload, cache guard on (default) | +64.5% cheaper (best case) | none |
| offload, cache guard off (opt-out) | −128% (dearer) | 8 of 11 |

Both default paths preserve the cache (zero busts) and cost less than sending verbatim. The inline-only
**+22.9%** is the caveat-free number: every value is kept and readable, no resolve. The offload row's
+64.5% is a best case that measures cost only and assumes the evicted bodies are never resolved back; a
workload that re-reads a dropped body pays an uncounted round trip. The guard-off row is the maturation
the guard prevents: deferring an offload past a few turns rewrites an already-cached block, and the
re-created suffix dwarfs the compression saving. Positive is cheaper.

```sh
cargo run -p secondwind-optimize --example cache_bench --release --features tiktoken
```

The prefix-cache model is a documented approximation; the mechanism is confirmed against a live provider
with real cache tokens (`bench/cache/live_probe.py`): a byte-identical prefix resend reads the entire
cached prefix, while changing one block inside it drops the read sharply.

## Compression latency

Per tool-output block, warm process, tokens counted through the shipped tokenizer. This is first-sight
cost: a block resent within a conversation hits the freeze cache and skips compression entirely.

| block (JSON array) | ~tokens | p50 | p99 | p99.9 |
|---|---|---|---|---|
| 2 KB (15 rows) | 782 | 0.72 ms | 1.15 ms | 2.24 ms |
| 27 KB (200 rows) | 10,778 | 6.94 ms | 10.87 ms | 55.7 ms |
| 282 KB (2000 rows) | 109,680 | 68 ms | 175 ms | 343 ms |

Latency scales roughly linearly with block size (about 0.25 ms/KB). Compression runs before the request
is forwarded, so it overlaps a model call that already takes hundreds of ms to seconds.

```sh
SW_BENCH2=1 cargo test -p secondwind-optimize --release --features tiktoken \
  bench_stage_latency -- --nocapture --test-threads=1
```

### Where the time goes (27 KB block, mean µs/call)

| stage | µs/block |
|---|---|
| admit (CLMH + inverse witness + blake3) | 1,702 |
| priced (tokenize, net-cost gate) | 1,252 |
| codec encode (columnar) | 522 |
| parse (serde_json) | 239 |
| dup-key scan | 158 |
| detector suite | 107 |
| **sum of measured stages** | **3,980** |
| **full compress_block** | **7,128** |

The full call exceeds the summed stages because it also runs the inline-vs-offload gate (a content-table
coverage check) and, for a record array the cost model elects to evict, the offload store write. The
admit proof and the tokenizer, not the codec, dominate: losslessness and honest pricing are the cost.

## Proxy throughput

The `serve` proxy under load (`oha`, 50 concurrent connections, against a local mock upstream so the
numbers reflect the proxy, not the model API).

| request | body | req/sec/node | p50 | p99 |
|---|---|---|---|---|
| passthrough (no compressible output) | 78 B | 53,974 | 0.88 ms | 2.01 ms |
| compressible tool output (compressed each request) | 31 KB | 5,818 | 6.76 ms | 40.6 ms |
| direct to upstream (no proxy, baseline) | 78 B | 174,530 | n/a | n/a |

All runs: 100% success. Passthrough and baseline are bounded by request body size (each request is parsed
and re-serialized), not compression. The compressible row is the compress-every-request floor: `oha`
sends independent requests, so none share a conversation and none hit the freeze cache; within a real
conversation a resent block hits that cache and skips compression, tracking the passthrough row instead.

```sh
cargo run -p secondwind --example mock_upstream --release &          # mock model API on :9099
secondwind serve --listen 127.0.0.1:8787 --upstream http://127.0.0.1:9099 &
oha -c 50 -z 10s -m POST -H 'content-type: application/json' \
  -D body.json http://127.0.0.1:8787/v1/messages                     # any provider request body
```

## Compression ratio (bytes)

Byte reduction per shape, with every value verified present against the recovered body (the test fails
if any value is lost). The `via` column keeps inline compression and recoverable offload distinct.

| shape | reduction | via | values kept |
|---|---|---|---|
| high-cardinality array | 55.5% | inline (columnar) | 991/991 |
| low-cardinality array | 70.4% | inline (columnar) | 11/11 |
| flat object | 94.8% | offload (recoverable) | 1200/1200 |

```sh
cargo test -p secondwind-optimize --test compression_bench -- --nocapture
```

## Notes

- Measured on one machine (Apple M3, 8 cores), release build, warm process. Your numbers
  will vary with hardware and workload.
- The proxy load test uses an instant mock upstream; a real model API adds hundreds of ms
  to seconds per request, which dwarfs the proxy's own overhead.
- The compression latency table is the fresh-compression cost, measured single-thread; the
  compressible-throughput row is the same cost under concurrent load.
