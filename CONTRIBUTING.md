# Contributing

Thanks for looking at secondwind. It is a Rust workspace: if `cargo test` passes and
`cargo clippy` is clean, you are most of the way to a mergeable change.

## Get set up

From a checkout of the repository:

```sh
cargo build
cargo test
```

Try it without installing anything, on a bundled example:

```sh
cargo run -p secondwind -- optimize examples/services.json
```

That runs one tool-output block through the optimizer and prints the transform, the token
counts, and the values-kept proof. No agent wiring required.

## The one invariant

secondwind's promise is that it never drops a value. Every compression path is gated:

- **Admission** proves the rewrite is lossless (a canonical leaf-multiset hash plus a
  coverage check) before it is allowed on the wire.
- **Net-cost** refuses any rewrite that is not a real token reduction after overhead.
- **Detectors** back-stop against fabrication, numeric drift, and artifact loss.

A change that weakens any of these is not a compression win, it is a correctness bug. New
or changed transforms must pass the round-trip property suite
(`crates/optimize/tests/fuzz.rs`) and the admission tests
(`crates/optimize/tests/admission.rs`).

## Where things live

| crate | what it does |
|---|---|
| `core` | the trace intermediate representation |
| `sources` | adapters that read agent session transcripts |
| `analyzers` | the deterministic violation detectors |
| `optimize` | the compressors, the admission and net-cost gates, the proxy |
| `ledger` | the token and rate accounting behind the net-cost gate |
| `report` | the scoreboard and receipt formatting |
| `redact` | secret and path redaction, the publish gate the scoreboard runs before it writes |
| `tap` | the pass-through recording proxy behind the `tap` subcommand |
| `ffi` | the single C ABI cdylib every non-Rust binding calls (Python ctypes, Node koffi/`bun:ffi`, WASM) |
| `cli` | the `secondwind` binary |

A new wire format is one `RequestShaper` implementation in `crates/optimize/src/shape.rs`
plus a branch in `pick_shaper`. A new compressor is a `Transform` plus its round-trip test.

## Bindings and adapters

The workspace is Rust, but the reach is not. `crates/ffi` exposes the one C ABI, and the
non-Rust bindings under `bindings/` call it to ship drop-in framework adapters, each with
its own suite:

- `bindings/python` runs its adapters as standalone scripts (`python test_<name>.py`):
  `test_langchain`, `test_langgraph`, `test_agno`, `test_strands`, `test_cursor`,
  `test_litellm`, `test_asgi`.
- `bindings/node` runs `test_*.mjs`: `test_vercel_real`, `test_langgraph_real`,
  `test_native_parity`.

A change that crosses the FFI boundary or touches an adapter must also pass that
language's suite, where losslessness is re-verified in-process. `cargo test` alone does
not cover it.

## Before you open a PR

- `cargo test` and `cargo clippy --workspace --all-targets` both pass.
- New behavior has a test. A new transform has a round-trip test.
- Comments are load-bearing only. Match the style of the code around you.

## License

By contributing you agree that your contributions are licensed under Apache-2.0, the same
license as the project.
