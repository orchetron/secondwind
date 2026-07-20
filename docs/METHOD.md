# Measurement method

Version 0.1.0, registered 2026-07-16. Scoreboard results reference the method version they were produced under. Changes to this document change the version.

## What is measured

Secondwind compares two sides of an agent session:

- **original**: content as produced by tools and users, before any context optimization.
- **effective**: content as it reached the model provider on the wire.

Pairs come from the secondwind capture tap, a local pass-through proxy that records on-wire tool results keyed by `tool_use_id` and joins them to session transcripts by the same key. The join is an exact key lookup. No fuzzy matching is used.

## Violation classes

All detection is deterministic. No language model participates in measurement. Every violation is a re-runnable diff over a stored pair.

- **V1 fabrication**: the effective content asserts something checkable that the original contradicts. Detected subset: `file:line:content` claims where the location is absent from the original, or the content at the same location differs.
- **V2 numeric drift**: a keyed numeric value (`key = number`, `"key": number`) whose key appears exactly once on both sides with different values is a violation unconditionally. A numeric value that was dropped is a violation only if the exact value is referenced in a later turn (tool call, tool result, or model output).
- **V3 artifact loss**: a file path, tool identifier, or hash-like token dropped from the effective side is a violation only if it is referenced in a later turn.

Drops without a later reference are never violations. They are reported separately as **retention rates** (numerics kept / total, artifacts kept / total). Keys appearing multiple times on either side are excluded from V2 change detection to avoid ambiguous mappings.

Not yet measured, by design: instruction loss (V4) and end-to-end task-success deltas, both of which require non-deterministic judgment. If added, they will appear under a new method version.

## Token ledger

- Token counts come from provider-reported usage in session records, deduplicated by provider request id (records sharing a request id are counted once).
- **billed tokens**: the total tokens the provider counted for the effective, on-wire traffic, summing input, output, and cache reads and writes. It needs no rate table and is exact for any model, priced or not.
- No dollar figure is reported on the surfaces this method governs: the token ledger, the scoreboard, and the run receipt. A token's price depends on the model, the provider tokenizer, and cache state, so the value of a saved token is not knowable from the record; the token count is. A rate table still exists in the source tree (`crates/ledger`) to drive the net-cost admission gate, but it is never surfaced as a savings claim. The no-arg audit (`crates/report`) does print a provider list-price cost block from that rate table; it is a factual usage readout of what the traffic cost, not an optimizer savings claim, and sits outside the method surfaces above.
- Latency is reported only as measured processing overhead where available. Counterfactual response latencies are not computed.
- Confidence intervals are attached only where a cell (optimizer version by workload type by metric) has at least 30 traces. Below that, figures are point observations of the corpus.

## Corpus and provenance

Every trace carries two labels: origin (`real-work` or `synthetic`) and party (`first-party` or `donated`). The scoreboard reports the corpus mix per optimizer (real-work / synthetic / donated); it does not gate on it. As a corpus-curation policy, published scoreboards use corpora that are at least 50 percent real-work and never synthetic-only. The `single-fleet` label, on the other hand, is emitted by the tool: rate and savings figures from a corpus without donated traces are marked `single-fleet` and describe that corpus only. Violation receipts are existence proofs and hold at any sample size.

## Publication gate

Before any trace or receipt is published: two independent scanners run over it (pattern-based for known credential formats, entropy-based for unknown high-entropy tokens), home directory paths are anonymized, masking is deterministic so in-trace cross-references survive, and a human reviews the output. Donor-submitted traces additionally require written consent, and the donor review window closes before any measured vendor receives materials. Measured vendors receive results and reproduction materials seven days before publication, and their responses are linked from the scoreboard.

## Reproduction

Each published receipt links a sanitized trace file. `secondwind repro <trace.json>` re-runs the detectors over it and prints the findings. Reproduction re-runs detection over stored pairs; it does not re-run the optimizer that produced them, whose behavior may be nondeterministic or version-dependent.
