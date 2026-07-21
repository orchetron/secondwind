# Benchmarks

Every number below is reproducible from this repo. Rig: Apple M3 (8 cores), release build,
warm process. Latency figures count tokens through the shipped tokenizer (the net-cost gate).

## Compression latency

Per tool-output block, over thousands of samples. This is first-sight cost: a resent block
hits the freeze cache and skips compression entirely.

| block (JSON array) | ~tokens | p50 | p99 | p99.9 |
|---|---|---|---|---|
| 2 KB (15 rows) | 782 | 0.56 ms | 0.70 ms | 0.79 ms |
| 27 KB (200 rows) | 10,778 | 5.5 ms | 5.9 ms | 6.1 ms |
| 282 KB (2000 rows) | 109,680 | 55 ms | 66 ms | 71 ms |

Latency scales roughly linearly with block size (about 0.2 ms/KB). Compression runs before
the request is forwarded, so it overlaps a model call that already takes hundreds of ms to
seconds.

```sh
SW_BENCH2=1 cargo test -p secondwind-optimize --release --features tiktoken \
  bench_stage_latency -- --nocapture --test-threads=1
```

### Where the time goes (27 KB block)

| stage | µs/block |
|---|---|
| codec encode (columnar) | 2,013 |
| admission proof (CLMH + inverse witness + blake3) | 1,676 |
| tokenize (net-cost gate) | 1,248 |
| parse + dup-key scan + detectors | 486 |
| **total** | **5,575** |

## Proxy throughput

The `serve` proxy under load (`oha`, 50 concurrent connections, 10 s, against a local mock
upstream so the numbers reflect the proxy, not the model API).

| request | body | req/sec/node | req/min/node | p50 | p99 |
|---|---|---|---|---|---|
| passthrough (no compressible output) | 115 B | 55,194 | ~3.3 M | 0.85 ms | 1.95 ms |
| compressible tool output (cache hit) | 38.6 KB | 3,375 | ~202 k | 10.6 ms | 82 ms |
| direct to upstream (no proxy, baseline) | 115 B | 167,355 | ~10 M | n/a | n/a |

All runs: 100% success. Throughput is bounded by request body size (each request is parsed
and re-serialized), not by compression: a resent or non-compressible request never
recompresses.

```sh
cargo run -p secondwind --example mock_upstream --release &          # mock model API on :9099
secondwind serve --listen 127.0.0.1:8787 --upstream http://127.0.0.1:9099 &
oha -c 50 -z 10s -m POST -H 'content-type: application/json' \
  -D body.json http://127.0.0.1:8787/v1/messages                     # any Anthropic/OpenAI body
```

## Compression ratio

Byte reduction per tool-output shape, with every value verified present (the test fails if
any value is lost). Token-level numbers and method are in [bench/](bench/).

| shape | byte reduction | values kept |
|---|---|---|
| high-cardinality array | 56.6% | 991/991 |
| low-cardinality array | 88.4% | 11/11 |
| flat object | 94.8% | 1200/1200 |

```sh
cargo test -p secondwind-optimize --test compression_bench -- --nocapture
```

## Notes

- Measured on one machine (Apple M3, 8 cores), release build, warm process. Your numbers
  will vary with hardware and workload.
- The proxy load test uses an instant mock upstream; a real model API adds hundreds of ms
  to seconds per request, which dwarfs the proxy's own overhead.
- The compressible-throughput row is cache-hit traffic (a fixed body). The per-block
  compression latency table is the fresh-compression cost, measured single-thread.
