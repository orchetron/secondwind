# Using secondwind with your own stack

secondwind is a thin proxy: it shrinks the tool-output blocks in a request and
streams the model's reply back untouched. Everything below is configurable without
touching the code.

## Point it at your own model / gateway

```
secondwind serve --listen 127.0.0.1:8080 --upstream https://llm.internal.acme.com
```

`--upstream` is any endpoint, your gateway, a self-hosted model, a vendor. Your agent
talks to secondwind; secondwind forwards to `--upstream`. (`secondwind run -- <agent>`
does the same and points the child agent at the proxy for you.)

## Wire formats

The proxy detects the request shape per request and compresses the tool outputs in it:

- **Anthropic Messages**: `messages[].content[]` blocks of `type: "tool_result"`.
- **OpenAI Chat Completions**: `messages[]` with `role: "tool"`.
- **OpenAI Responses**: `input[]` items of `type: "function_call_output"`.
- **AWS Bedrock Converse**: `messages[].content[]` blocks of `toolResult`.

All four are handled out of the box; a request in any of these shapes gets its tool
outputs compressed, everything else passes through byte-for-byte. A different in-house
format is one `RequestShaper` implementation plus a branch in `pick_shaper` (see
`crates/optimize/src/shape.rs`).

## Your own model

The model is read per request from the request's `model` field and recorded as-is, so
your internal model name shows up correctly in the dashboard's BY MODEL and BY PLATFORM
views. secondwind never renames it. (Compression itself is model-independent.)

## Your own platform labels

Register detection rules in `~/.secondwind/config.json`. Each rule matches a request
header by name and a case-insensitive substring, and assigns a label:

```json
{
  "platforms": [
    { "header": "user-agent", "contains": "acme-agent", "label": "acme agent" },
    { "header": "x-team",     "contains": "payments",    "label": "payments" }
  ]
}
```

Rules are checked first; requests that match none fall back to the built-in detection
(Claude Code, direct SDK, etc.). No restart-time code change.

## Your own relevance / prose models

The two optional model-backed stages call endpoints you own, so nothing leaves your
network unless you opt in:

- **Relevance embedder**: `--embed https://embed.internal.acme.com` with the bearer
  key in `SECONDWIND_EMBED_KEY`. Any OpenAI-compatible `/embeddings` endpoint.
- **Prose keep/drop classifier**: `--prose-classifier https://prose.internal.acme.com`
  with the key in `SECONDWIND_PROSE_KEY`. You own the model that decides what to keep.

Both are off by default; without them secondwind uses its dependency-free built-ins.

## Your own compression transform

Add a domain-specific transform by implementing the `Transform` trait and registering
it with `Optimizer::with_transform`:

```rust
optimizer = optimizer.with_transform(Box::new(MyTransform));
```

A transform returns the wire form plus the value it decodes to; it is tried after the
built-ins when they refuse a block, and it passes the same lossless (atom-multiset) and
net-cost admission gate every built-in does, so a lossy or unprofitable transform is
refused, never shipped. Registration order is independent of the other builder calls.

## Your own text codec (proposer)

For a raw-text codec, implement the `TextProposer` trait (`id`, `encode`, `decode`) and
register it with `Optimizer::with_text_proposer`:

```rust
optimizer = optimizer.with_text_proposer(Box::new(MyCodec));
```

A proposer is searched best-of-N alongside the built-ins. It clears a different bar than a
transform: rather than the atom-multiset gate, each block is admitted under a per-instance
`decode(encode(raw)) == raw` proof, so the codec may be reckless, a wrong round trip is
caught on that block and dropped, never shipped. `CallbackProposer::new` wraps host-supplied
encode/decode closures, so a codec written in any language and reached over the C ABI
competes in the same search under the same proof.

## Your own offload store backend

Offloaded originals go to local disk by default. For a multi-instance deployment, where
any proxy must resolve any marker, back the store with a shared backend (Redis, object
storage) by implementing `OffloadStore` and passing it to `Optimizer::with_store`:

```rust
optimizer = optimizer.with_store(MyRedisStore::connect(url));
```

The trait is four methods (`offload`, `resolve`, `covers`, `prospective_stub_len`); the
built-in local-disk `Store` is one implementation of it. Everything else, the marker
format, the coverage proof, the inline-vs-offload gate, is unchanged.

## Add an agent

Which agents `run` detects and `setup` guides comes from one registry,
`crates/cli/src/agents.rs`. Adding an agent is a single entry, and both commands pick it
up with no per-agent branch:

```rust
Agent { name: "myagent", bin: "myagent", route: Route::Launch },
```

`Route::Launch` is a terminal agent that honors a base-URL env var, so `run` launches it
through the proxy for one session with nothing written to disk. `Route::Guided` is a GUI
app whose only sanctioned routing is a base-URL override the user sets in its own settings;
it names the app path, the setup steps, and two documented files, the app's MCP config and
its project rules file:

```rust
Route::Guided {
    app: "/Applications/MyIde.app",
    steps: &["open Settings, set the OpenAI base URL to the endpoint below", ...],
    mcp_config: ".myide/mcp.json",
    rules_file: ".myiderules",
}
```

`setup <agent>` prints the base-URL steps; `setup <agent> --tool` registers the resolve
tool in the MCP config and adds one marked, reversible block to the rules file, and `--off`
undoes both. secondwind only ever touches those two documented files, never the app's
internal store, which for these apps holds auth tokens and would corrupt on a blind write.
