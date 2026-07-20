# examples

Realistic tool-output blocks. Point `optimize` at any of them and it routes each to
whichever transform wins its proof-gated best-of-N search:

```sh
secondwind optimize services.json --tokens
```

`--tokens` prices the result in real model tokens, the billed unit. Drop it to
price in bytes. The numbers below reproduce from the files in this folder.

| file | shape | transform | tokens | saving |
|---|---|---|---|---|
| `services.json` | uniform JSON array | columnar | 1377 → 387 | 72% |
| `search.txt` | grep / ripgrep output | offload | 525 → 120 | 77% |
| `deploy.log` | CLI log lines | columns | 552 → 300 | 46% |
| `manifest.json` | large flat object | offload | 1435 → 98 | 93% |
| `notes.txt` | long prose | offload | 209 → 134 | 36% |

The prose one uses the opt-in summary, so run it with `--prose`. The summary rides
the `offload` mechanism, so its machine `transform` field reads `offload`:

```sh
secondwind optimize notes.txt --prose --tokens
```

## Every one is lossless

The saving is only half of it. Each compressed block decodes back to its exact
original bytes:

- Inline transforms (`columnar`, `columns`, `search`, `log`) rewrite the block into a compact,
  self-contained form that still holds all the data, so the model reads it directly
  with no round-trip.
- `offload` replaces bulk with a short marker and keeps the exact original one
  `resolve` call away, in a durable on-disk store.
- The opt-in `--prose` summary is the exception to byte-exact inline: what the model
  reads inline is a lossy summary, not the self-contained form the transforms above
  produce. The exact original text is recovered via `resolve` (the summary rides the
  `offload` store).

Every applied block carries a `blake3` fidelity certificate (`optimize` prints it),
and `secondwind verify <wire> <certificate>` re-derives it from the compressed
output. The transforms are also round-trip property-tested:
`crates/optimize/tests/fuzz.rs` fuzzes the `columnar`, `log`, `search`, and `offload`
paths, and the text-columnar (`columns`) codec is property-tested in its own module,
`crates/optimize/src/text_columnar.rs`.
