# 30-day readout (template)

The launch gate: after 30 days of real traffic, report what secondwind actually did,
measured against the pre-registered method in [METHOD.md](METHOD.md). Fill every `<...>`
from the shipped CLI and the event log. Leave a row blank rather than estimate it.

Period: `<start>` to `<end>`. Method version: `<vX.Y.Z>`. Corpus: real traffic only.

## Coverage


| field                   | value                          |
| ----------------------- | ------------------------------ |
| sessions                | `<N>`                          |
| tool-output blocks seen | `<N>`                          |
| blocks compressed       | `<N>`                          |
| platforms               | `<claude code / openai / ...>` |
| models                  | `<names, recorded as sent>`    |


## Token reduction

Tokens are the only value figure reported. No dollar estimate is produced (see METHOD.md).


| field                  | value                                   |
| ---------------------- | --------------------------------------- |
| tokens in (original)   | `<N>`                                   |
| tokens out (effective) | `<N>`                                   |
| tokens removed         | `<N>`                                   |
| reduction              | `<%>`                                   |
| by transform           | `<columnar, search, log, offload, ...>` |
| by platform            | `<...>`                                 |


Counted once per block: a block re-read from the provider cache on later turns is not
counted again, so this is the conservative figure.

## Lossless proof

The load-bearing claim. A single lost value fails the gate.


| field                                                | value          |
| ---------------------------------------------------- | -------------- |
| blocks verified lossless                             | `<N> / <N>`    |
| lossless failures                                    | `<0 expected>` |
| self-proof rejects (`self_proof_reject`)             | `<N>`          |
| offload recoveries, byte-for-byte                    | `<N> / <N>`    |
| certificate spot-checks re-run (`secondwind verify`) | `<N>`          |


If this is ever not `<N>/<N>`, that is the story. Report it first. Self-proof rejects are
blocks the fail-closed whole-request check declined to forward compressed; the count comes
from the `kept_reason = self_proof_reject` rows in `~/.secondwind/events/events.jsonl`.

## Do no harm


| field                                             | value                                                                      |
| ------------------------------------------------- | -------------------------------------------------------------------------- |
| blocks the gate refused (kept verbatim)           | `<N>`                                                                      |
| kept verbatim, by reason                          | `<refused_clmh: N / no_saving: N / no_resolver: N / self_proof_reject: N>` |
| offload reopen rate (model called resolve)        | `<%>`                                                                      |
| agent-behavior changes (replay judge) [1]         | `<worse / equivalent / better>`                                            |
| cache stability: resends forwarded byte-identical | `<yes / rate>`                                                             |


Refusals are expected and healthy. secondwind rewrites a block only when it is a proven
win and leaves the rest untouched. The kept-verbatim-by-reason breakdown comes from the
decision trail in `~/.secondwind/events/events.jsonl` (`kept_reason`, joined per request by
`req_id`), which records blocks seen versus compressed versus kept verbatim.

[1] Outside the deterministic pre-registered method. The replay-judge row asks a live model
to rate whether a compressed choice was acceptable, so it is non-deterministic (METHOD.md
excludes non-deterministic judgment). Report it as a separate replay-judge readout, not as a
method-conformant figure.

## Not measured, by design

Instruction loss and end-to-end task-success deltas need non-deterministic judgment and
are out of this readout (see METHOD.md). Any figure not producible from the CLI or the
event log is left blank, never estimated.

## Reproduction

- Token reduction on your own traffic: `secondwind cache-savings`
- Offload reopen rate: `secondwind reopen-rate`
- Replay-judge figure: `secondwind replay` (needs a `--model`, and a `--max-spend` cap; falls back to `SECONDWIND_MODEL`)
- Lossless and compression suite: `cargo test`
- Token benchmark corpus: `python3 bench/token_bench.py target/debug/secondwind`
- Any published receipt: `secondwind repro <trace.json>`

