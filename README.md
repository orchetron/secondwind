<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/secondwind-dark.png">
  <img alt="secondwind" src="docs/secondwind.png" width="340">
</picture>

**Lossless. Provable. Composable.**

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](#license) [![Tests](https://img.shields.io/badge/tests-passing-brightgreen.svg)](#build-and-test) [![Fidelity](https://img.shields.io/badge/fidelity-lossless%20by%20proof-8A2BE2.svg)](#how-it-works)

</div>

<p align="center">
  <img src="examples/demo.gif" width="760" alt="secondwind demo: a coding agent on the left, live token savings on the right">
</p>

<p align="center">
  <em>A coding agent on the left. secondwind cutting its tokens on the right, proven lossless, live.</em>
</p>

Cut the tokens your AI agent sends the model, without losing a value, and prove it.

secondwind sits between your coding agent and the model API. It losslessly compresses the
tool output your agent feeds back to the model, proves every value survived, and reports
exactly how many tokens it removed. Your agent behaves the same. The model just reads
fewer tokens.

- **Lossless by proof, not by hope.** A block is compressed only when it passes a
  canonical-hash admission check and a coverage invariant. Anything that would drop a
  value is refused and sent through untouched.
- **Zero change to your agent.** A thin proxy rewrites requests and pipes replies
  straight back, so streaming never breaks and secondwind never joins the conversation.
- **Tokens you can prove, not dollars you would guess.** Every block reports the exact
  tokens it removed, counted once and re-checkable against the original. secondwind never
  estimates your bill; the token reduction is the ground truth.

## Three ways to run it

- **Proxy**: `secondwind serve` / `run` sits between your agent and the model API. Zero code
  change, works with any agent (below).
- **Library (SDK)**: call `Session` directly in Python, Node/Bun, or sandboxed WebAssembly. No
  proxy, no model download ([In-process](#use-it-in-process-library-and-adapters)).
- **Middleware**: drop-in adapters for LangChain, LangGraph, Agno, Strands, Cursor, LiteLLM, the
  Vercel AI SDK, and ASGI ([Adapters](#framework-adapters)).

Same lossless-and-proven core in all three.

## Install

Pick the surface you want: the proxy is a single binary, each library is one package.

```sh
# CLI / proxy: a prebuilt, checksum-verified binary (falls back to a source build)
curl -fsSL https://raw.githubusercontent.com/orchetron/secondwind/main/install.sh | sh

pip install secondwind      # Python library and adapters
npm install secondwind      # Node / Bun library and adapters
```

Rather build the CLI from source? `cargo install --git https://github.com/orchetron/secondwind
secondwind` (needs [Rust](https://rustup.rs)), or clone this repo and `cargo install --path crates/cli`.

## Quickstart

Once the CLI is installed, two commands wire it up; then use your agent exactly as you always do:

```sh
secondwind setup            # wire Claude Code once (registers the resolve tool)
secondwind run -- claude    # run your agent through it, get a token receipt
```

Use Claude Code exactly as you normally would. When the session ends, you get a receipt:

```
secondwind receipt: 6 blocks, 5120 tokens saved (64.3%), 6/6 verified lossless
```

Every rewrite is gated and verified lossless, and nothing about the agent's behavior
changes.

Then watch it work, and confirm the install:

```sh
secondwind proof     # a local web dashboard of every block compressed, with lossless proof
secondwind watch     # or a live terminal view, for a second pane next to your agent
secondwind check     # verify the install: tokenizer, state, agent integration
```

### Other agents and APIs

`run` is the fast path for any terminal agent. It launches the agent with its base URL
pointed at the proxy for that session, sets both `ANTHROPIC_BASE_URL` and `OPENAI_BASE_URL`
so Anthropic and OpenAI CLIs both work, and changes nothing on disk, so there is nothing to
undo and the proxy exits with the agent. With no argument it routes whatever known agent is
on your PATH:

```sh
secondwind run              # detect an installed agent and route it
secondwind run -- codex     # or name one; both base URLs are set for you
```

For a GUI or a persistent setup, run the shared endpoint and point the agent's own base URL
override at it:

```sh
secondwind serve &                               # hosts the optimizing endpoint on :8787
export ANTHROPIC_BASE_URL=http://127.0.0.1:8787  # an Anthropic-API agent
# or, for an OpenAI-API agent, set its base URL (OPENAI_BASE_URL / SDK base_url) to the same
```

The proxy detects the request shape per request and compresses the tool outputs in it,
whether the agent speaks the Anthropic Messages, OpenAI Chat Completions, OpenAI
Responses, or AWS Bedrock Converse format. Anything it cannot prove a win on passes
through with every value intact.

### See one block compress

Point `optimize` at any tool-output block to watch it shrink and prove it. Straight from a
clone, with nothing installed, `cargo run -p secondwind -- optimize <file>` does the same:

```sh
secondwind optimize bench/compression/corpus/01-highcard-array.json
cargo run -p secondwind -- optimize examples/services.json   # no install needed
```

```json
{
  "transform": "columnar",
  "input_tokens": 19502,
  "output_tokens": 10042,
  "values_kept": "991/991",
  "certificate": "7e4784728467de31e0b5a3238a06c311662af8b3831c707748141c6ff4cafee6"
}
```

That block went from 19,502 to 10,042 tokens, all 991 of its values are still present, and
the certificate lets `secondwind verify` re-check the compressed output against the
original. (The output above is trimmed to the salient fields; the command also prints the byte
counts and the full compressed wire.) More samples, one per tool-output shape, are in
[examples/](examples/).

## Use it in-process (library and adapters)

No proxy required: call secondwind directly in your own code, same lossless-and-proven core, in your
process. The native library is bundled, so there is no build step and no model download.

**Python** (`pip install secondwind`):

```python
import secondwind

session = secondwind.Session()
out = session.rewrite(request)     # compress a request's tool outputs, in place
print(out["stats"])                # tokens saved, counted once
secondwind.compress(block)         # or compress a single block
```

**Node and Bun** (`npm install secondwind`; native FFI, or `secondwind/wasm` for a sandboxed build
that runs anywhere WebAssembly does, including the browser):

```js
import { Session } from "secondwind";
const out = new Session().rewrite(request);
```

Any language can call the same shared library through its C ABI. Full API in the binding READMEs:
[Python](bindings/python/README.md), [Node](bindings/node/README.md).

### Framework adapters

Drop-in, in-process, lossless. Each is a couple of lines; the binding READMEs carry the snippets.

| framework | import |
|---|---|
| LangChain (LCEL) | `from secondwind.langchain import compress_tool_outputs` |
| LangGraph | `from secondwind.langchain import compress_pre_model_hook` |
| Agno | `from secondwind.agno import SecondwindCompressionManager` |
| Strands | `from secondwind.strands import SecondwindHooks` |
| Cursor | `python -m secondwind.cursor` (postToolUse hook) |
| LiteLLM | `from secondwind.litellm import SecondwindCallback` |
| ASGI (Starlette / FastAPI) | `from secondwind.asgi import SecondwindMiddleware` |
| Vercel AI SDK | `import { secondwindMiddleware } from "secondwind/vercel"` |
| LangGraph.js | `import { compressPreModelHook } from "secondwind/langgraph"` |

One import and one wrap. Your framework's tool outputs compress losslessly on the way to the
model, and pass through untouched when they will not shrink.

### Bring your own codec

The library runs a proof-gated best-of-N codec search, and you can add your own codec to it. It
competes with the built-ins and is proven `decode(encode(x)) == x` per block, so a wrong or reckless
codec is dropped, never shipped.

```python
session = secondwind.Session(codec=MyCodec())   # or Session(proposers=False) to turn the search off
```

## How it works

```text
  Your agent  ·  Claude Code, Cursor, Codex, or any Anthropic, OpenAI, or Bedrock app
       │  tool outputs · search results · logs · diffs · files · prose
       ▼
  ┌─ secondwind · runs locally, your data never leaves your machine ───────────
  │
  │  1 · route each block by shape, run the matching compressor
  │       uniform JSON arrays   ->  columnar codec (dict, affix, adaptive per-column)
  │       search / grep / find  ->  shared path + snippet dictionary, or match-map
  │       logs / CLI output     ->  line template + parameter extraction
  │       diffs / patches       ->  per-file change-surface summary
  │       long prose            ->  extractive coverage summary (or the --prose-classifier endpoint)
  │       a repeated block      ->  byte-exact cross-turn dedup
  │
  │  2 · rank which rows stay inline (relevance)
  │       BM25 + relevance feedback + character trigram + distilled static
  │       embeddings, or your own embedding model via --embed
  │
  │  3 · apply only if it clears every gate
  │       admission   canonical leaf-multiset hash + coverage      (lossless)
  │       net-cost    a real token reduction after overhead        (a real win)
  │       detectors   fabrication · numeric drift · artifact loss  (no harm)
  │
  └─ emit ─────────────────────────────────────────────────────────────────────
       inline     every value present, no round-trip
       offload    a short marker; the model calls resolve() for the exact
                  original, kept in a durable on-disk store
       verbatim   nothing was a proven win, so the block passes untouched

  Responses stream from the model back to your agent byte for byte.
  secondwind rewrites requests; it never rewrites a response.
```

Your agent calls a tool (reads a file, runs a search, lists an API), then feeds the result
back to the model on the next turn. That tool output is where the tokens pile up.
secondwind rewrites it three ways, in order of preference:

1. **Reshape it inline.** Uniform arrays, search results, logs, and diffs are re-encoded to
   a compact form that still contains every value, so the model reads it directly with no
   round-trip.
2. **Offload it, recoverably.** Bulk that will not shrink inline is replaced with a short
   marker. The model fetches the exact original through the `resolve` tool (registered by
   `secondwind setup`) whenever it needs it. Nothing is dropped, it is moved one hop away.
3. **Leave it alone.** If neither is a proven win, the block passes through byte for byte.

Offload activates only when the agent actually carries the `resolve` tool, so a marker can
never be stranded. The response path is a pure byte pipe: secondwind reads requests and
never touches responses.

### Relevance and prose (optional)

- Query-aware relevance keeps the rows a request is about inline and offloads the rest. It
  runs on a thin, baked-in embedding table by default. Add `--embed <url> --embed-model
  <name>` to rank with your own embeddings model instead.
- `--prose` summarizes long prose into a shorter, recoverable working summary.
  `--prose-classifier <url>` points at your own extractive keep/drop endpoint for
  token-level shrink. Both keep the full original one `resolve` call away.

## Architecture

secondwind runs entirely on your machine. Nothing leaves it except the compressed request,
which goes to the same model API your agent already uses.

```text
  On your machine                                          remote
  ─────────────────────────────────────────────           ──────────

     your agent     ──── request ────►   secondwind    ──── compressed ───►  Model
     (any API)      ◄─── response ────   proxy           ◄─── response ─────  API
          │                              (serve / run)
          │  resolve(marker)                  │  writes offloaded originals
          ▼                                   ▼
     secondwind mcp   ◄──── exact ─────   offload store (on disk)
     (resolve tool)         original
```

- **The proxy** (`serve` / `run`) detects the request shape, rewrites the tool outputs in
  it through the optimizer, and pipes responses back untouched.
- **The optimizer** applies each transform only behind an admission gate (a canonical-hash
  and coverage check) and a net-cost gate, so a block is rewritten only when it is proven
  lossless and a real token reduction.
- **The resolve server** (`mcp`, registered by `setup`) shares one disk store with the
  proxy, so a block the proxy offloads is fetched back by the agent's `resolve` tool with
  the exact original bytes.

**Extensibility.** Bring your own upstream or model, relevance embedder, prose classifier,
compression codec or transform, offload store, or agent, and set your own platform labels, all
without forking. Each of these is a short section in the guide:
[docs/COMPOSABILITY.md](docs/COMPOSABILITY.md).

**Observability.** Every decision is recorded to `~/.secondwind/events/events.jsonl`: blocks seen
versus compressed versus kept verbatim (with the reason), self-proof rejections, and a per-request
id. `proof` and `watch` read it. Method and pre-registered gates are in
[docs/METHOD.md](docs/METHOD.md) and [docs/READOUT.md](docs/READOUT.md).

## Under the hood

Lossless *and* cheap is easy to get wrong. A few things that make it hold:

- **Prompt-cache safe.** The provider caches the request prefix, so a block rewritten differently on
  a resend would shift the bytes after it and bust that cache, re-billing at the write premium what
  was reading at `0.1x`. secondwind freezes each block's chosen wire and re-emits it byte-for-byte on
  every resend, so the cached prefix never moves; a net-cost gate refuses any rewrite that would not
  save more than it costs.
- **Best-of-N, proven per block.** Each block runs through every codec that fits (columnar arrays,
  text columns, token and line dictionaries, log templates, offload) and the smallest wire that
  passes the lossless proof wins. A codec you bring competes in the same search under the same
  per-instance `decode(encode(x)) == x` proof, so a reckless one is dropped, never shipped.
- **Proof, not trust.** A rewrite is admitted only when a canonical leaf-multiset hash and an inverse
  witness show every value survived in the right place; each applied block carries a blake3
  certificate you can re-check with `secondwind verify`, and the whole request is re-proved lossless
  before it is forwarded, falling back to the exact original on any doubt.
- **Fresh stays whole, aged compresses.** Output the model is still acting on this turn is left
  verbatim; only output that has aged past a few turns is offloaded, so the model never has to
  resolve something it still needs.
- **Query-aware relevance.** When it must pick which rows stay inline, it ranks them on a baked-in
  static embedding table by default, no model call and nothing leaving your machine, or on your own
  embeddings model if you point it at one.

## Commands

| command | what it does |
|---|---|
| `secondwind run [-- <agent>]` | route an agent through the optimizer (auto-detects one if unnamed), print a verified receipt |
| `secondwind serve` | host the optimizing endpoint for any agent (Anthropic, OpenAI, or Bedrock shapes) |
| `secondwind setup` | register the `resolve` tool with Claude Code (`--hook` also compresses Bash output) |
| `secondwind proof` | live dashboard of tokens removed, with per-block lossless proof |
| `secondwind watch` | live terminal view of savings, refreshed each second (companion to `proof`) |
| `secondwind check` | verify the install and agent integration |
| `secondwind optimize <file>` | compress one tool-output block and print the result |
| `secondwind verify <wire> <cert>` | re-check a compressed wire against its fidelity certificate |
| `secondwind exec -- <cmd>` | compress one command's output, shell-filter style |
| `secondwind` (no args) | audit what optimization did to your existing sessions |

Run `secondwind help <command>` for the full flags of any command.

## Benchmarks

Reproducible from this repo, no external services:

```sh
cargo test -p secondwind-optimize --test compression_bench -- --nocapture
cargo test -p secondwind-optimize --test relevance_bench   -- --nocapture
```

Compression on a corpus of tool-output shapes. Byte reduction next to fidelity, where
fidelity is every significant value kept inline or recovered through the marker. The test
fails if any value is lost, so a reduction is only ever reported next to verified-lossless
output:

| shape | byte reduction | values kept |
|---|---|---|
| high-cardinality array | 56.6% | 991/991 |
| low-cardinality array | 88.4% | 11/11 |
| flat object | 94.8% | 1200/1200 |

Those are byte reductions. For token reduction (the billed unit), `bench/token_bench.py`
runs a corpus of tool-output shapes with the same per-block fidelity check; method and
numbers in [bench/](bench/).

## FAQ

**Does it change how my agent behaves?**
No. secondwind only rewrites tool output on the way to the model, and only when it can
prove the rewrite is lossless. Responses stream back byte for byte, and your agent's own
logic never sees secondwind at all.

**What if the model needs a value that was offloaded?**
It calls the `resolve` tool (registered by `secondwind setup`) and gets the exact original
bytes from the on-disk store. Offload happens only when that tool is present, so a marker
can never be left with nothing to fetch.

**Which agents and models work?**
Any agent on the Anthropic, OpenAI, or AWS Bedrock API. Claude Code is wired automatically
by `setup`; any other agent points its API base URL (`ANTHROPIC_BASE_URL` or
`OPENAI_BASE_URL`) at `serve`, which detects the Anthropic Messages, OpenAI Chat
Completions, OpenAI Responses, and Bedrock Converse shapes. Compression itself is
model-independent.

**How do I know a rewrite is real, not just a smaller number?**
Every applied block reports the exact tokens it removed and carries a fidelity certificate
that `secondwind verify` re-checks against the original. A block is rewritten only when it
clears the lossless admission check and a net-cost gate that refuses any rewrite that is
not a real reduction after overhead.

**Does my data leave my machine?**
Only the compressed request, and only to the same model API your agent already calls. The
proxy, the store, and the resolve server all run locally.

## Requirements

- An agent on the Anthropic, OpenAI, or AWS Bedrock API. Claude Code is wired automatically by
  `secondwind setup`. Any other agent points its API base URL at `serve`.
- [Rust](https://rustup.rs) (stable) only to build the CLI from source; the install script and the
  language packages ship prebuilt binaries, so most users need nothing else.

The CLI installs from a prebuilt binary (the [install script](#install) or the
[GitHub releases](https://github.com/orchetron/secondwind/releases)); the libraries are on
[PyPI](https://pypi.org/project/secondwind/) and [npm](https://www.npmjs.com/package/secondwind). A
crates.io publish (`cargo install secondwind`) will follow.

## Build and test

```sh
cargo build --release
cargo test
```

## License

Apache-2.0. See [LICENSE](LICENSE).

---

<div align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/secondwind-dark.png">
    <img alt="secondwind" src="docs/secondwind.png" width="110">
  </picture>
  <br><br>
  <sub>Built by <b>Orchetron</b> · <a href="./CONTRIBUTING.md">Contributing</a> · Apache-2.0</sub>
</div>
