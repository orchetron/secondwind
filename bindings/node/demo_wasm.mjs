// Demo: secondwind core as a sandboxed WASM module in plain Node (no native lib, no build step).
// Run: node bindings/node/demo_wasm.mjs
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { load } from "./secondwind_wasm.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const wasmFile =
  process.env.SECONDWIND_WASM ??
  join(here, "..", "..", "target", "wasm32-unknown-unknown", "release", "secondwind.wasm");
const bytes = await readFile(wasmFile);

// Sandbox claim, checked mechanically: the module imports nothing, so it has no host capability.
const imports = WebAssembly.Module.imports(new WebAssembly.Module(bytes));
console.log(`imports the module asks the host for: ${imports.length}`);
if (imports.length !== 0) throw new Error(`expected a zero-capability sandbox, got ${imports.length} imports`);

const sw = await load(bytes);
console.log(`abi version: ${sw.abiVersion()}\n`);

// Realistic ls -l block: uniform columns the codec factors losslessly.
const ls = Array.from({ length: 40 }, (_, i) => `-rw-r--r-- 1 root wheel ${String(100 + i * 37).padStart(6)} file-${i}.txt`).join("\n");

const c = sw.compress(ls);
console.log(`compress: ${c.kind} via ${c.transform}`);
console.log(`  tokens ${c.input_tokens} -> ${c.output_tokens} (${c.tokens_saved} saved, ${((c.tokens_saved / c.input_tokens) * 100).toFixed(1)}%)`);

const ok = sw.verify(c.wire, c.certificate.hash);
console.log(`  independently verified lossless in the sandbox: ${ok}`);
if (!ok) throw new Error("wire failed to verify lossless");

const mid = Math.floor(c.wire.length / 2);
const altered = c.wire.slice(0, mid) + (c.wire[mid] === "0" ? "1" : "0") + c.wire.slice(mid + 1);
const tampered = sw.verify(altered, c.certificate.hash);
console.log(`  a tampered wire is rejected: ${tampered === false}`);
if (tampered !== false) throw new Error("tamper detection failed");

// A whole request: only the tool output is rewritten; every other byte is untouched.
const request = {
  model: "gpt-4o",
  messages: [
    { role: "user", content: "ls -l" },
    { role: "tool", tool_call_id: "c1", content: ls },
  ],
};
const session = sw.session();
const first = session.rewrite(request);
const shrunk = first.request.messages[1].content;
console.log(`\nrewrite: tool output ${ls.length} -> ${shrunk.length} bytes; user message untouched: ${first.request.messages[0].content === "ls -l"}`);

// Cache safety: a resend re-emits byte-identical bytes and is not re-counted.
const second = session.rewrite(request);
const identical = JSON.stringify(first.request) === JSON.stringify(second.request);
console.log(`resend is byte-identical: ${identical}; counted once: ${session.stats().blocks_first_seen === first.stats.blocks_first_seen}`);
if (!identical) throw new Error("resend was not byte-identical");
session.close();

console.log("\nall wasm-sandbox checks passed.");
