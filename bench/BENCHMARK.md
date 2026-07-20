# Token benchmark: reduction with verified fidelity

Every compression number here is paired with a **fidelity check**, because a token
reduction is only honest if nothing was lost. Fidelity is verified the reliable way,
not with an approximate score:

- **Offloaded blocks**: the recovered bytes must equal the original **byte-for-byte**.
- **Inline transforms**: the wire must pass `secondwind verify` against the block's
  certificate (canonical leaf-multiset equality).

A block is only ever counted after that check passes.

## Reproduce

```
cargo build -p secondwind
python3 bench/token_bench.py target/debug/secondwind
```

The corpus is ~30 tool outputs: real command captures (`cargo metadata`, `git log`,
`ps aux`, `env`, `find`) plus representative shapes across the categories agents hit,
uniform JSON arrays, nested objects, structured logs, search results, CLI text. Content
is synthetic; structure is real; generation is seeded so runs compare.

## Result (N=29, tokenizer: cl100k)

| metric | value |
|---|---|
| total tokens | ~477,000 → ~25,000 (one representative run) |
| **removed overall** | **~95%** |
| per-file reduction | median 92.7%, range 29 to 100% |
| **lossless** | **29 / 29 verified** |
| blocks refused (not a real saving) | 0 in this corpus |

Five of the inputs are live host captures (`ps aux`, `env`, `git log`, `find`,
`cargo metadata`), so the exact totals shift a little run to run; the median per-file
reduction and the 29/29 lossless rate are the stable figures. The numbers above are from
one representative run, regenerate them with the command above.

## What the number does and does not mean

Reduction splits by mechanism, and the honest reading keeps them separate:

- **Inline** (values stay in context, no round-trip): uniform arrays 29 to 86%
  (columnar reformat), structured text and logs 51 to 67% (columnar and template
  extraction). The model reads the same values in fewer tokens.
- **Offload** (87 to 100%): a large block becomes a preview + marker, and the exact
  original is one `resolve` call away in a local store, returned **byte-for-byte**. The
  reduction is real (fewer tokens on the wire) but the data is recoverable, not inline;
  we do not claim offloaded data is "in context."

The headline (~95%) is dominated by a few large offloadable blocks; the median (92.7%)
and the per-category ranges are the more representative figures.

## Why "lossless" is the load-bearing word

Sampling or dropping tokens can post a higher raw ratio by throwing information
away. Here the fidelity column is not decoration: a run that lost a single value would
print `LOSS!` and the block would not be counted. 29/29 verified is the claim, and the
harness fails loudly if it ever stops being true.
