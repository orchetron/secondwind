# secondwind

Lossless, provable, composable LLM tool-output compression, in-process.

Native (lowest-overhead path). One import, both runtimes: it binds the shared library through Bun's built-in FFI
under Bun and through koffi under Node. The right per-platform binary installs automatically:

```js
import { Session, compress, verify } from "secondwind";

const session = new Session();
const out = session.rewrite(request);       // compress a whole request's tool outputs
console.log(out.stats);                      // tokens saved

// one-line Vercel AI SDK integration:
import { wrapLanguageModel } from "ai";
import { secondwindMiddleware } from "secondwind/vercel";
const model = wrapLanguageModel({ model: openai("gpt-4o"), middleware: secondwindMiddleware() });

// LangGraph.js: compress the tool outputs an agent loop re-sends before each model call
import { compressPreModelHook } from "secondwind/langgraph";
const agent = createReactAgent({ llm: model, tools, preModelHook: compressPreModelHook() });
```

The `Session` takes an options object, the same surface as the Python binding:

```js
new Session({ codec: myCodec });               // your codec competes in the best-of-N, dropped if it ever fails
new Session({ proposers: false });             // turn the aggressive search off
new Session({ store, resolver, offloadDir });  // offload + recover, or a Redis / S3 / db-backed store
```

Sandboxed WebAssembly (plain Node, Deno, Bun, or the browser; no native build, no Bun required):

```js
import { load } from "secondwind/wasm";

const sw = await load();
const out = sw.compress(block);
// Inline result: { wire, certificate }. A large block returns a recoverable { stub, marker } instead.
if (out.wire) sw.verify(out.wire, out.certificate.hash);  // confirm losslessness in the sandbox
const session = sw.session();
session.rewrite(request);
```

Every result is lossless: an inline wire is independently verifiable with `verify(wire, hash)`, and a
large block is offloaded to a recoverable marker. The native library and the wasm module are both
bundled, so there is no build step and no model download.

The wasm module imports nothing from the host (check `WebAssembly.Module.imports` yourself: the list
is empty), so it has no capability to read a file, open a socket, or read a clock. It takes bytes in
and gives bytes back.
