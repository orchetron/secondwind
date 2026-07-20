// Demo: compress + independently verify + whole-request rewrite in-process over the C ABI.
// Run: bun bindings/node/demo.mjs
import { abiVersion, compress, verify, Session } from "./secondwind.mjs";

console.log("ABI version:", abiVersion());

const ls = Array.from(
  { length: 300 },
  (_, i) => `-rw-r--r--  1 root  wheel  ${String(100 + i * 37).padStart(7)} Jan  1 12:00 file-${i}.txt`,
).join("\n");

const c = compress(ls);
console.log(`compress: ${c.kind}/${c.transform} ${c.input_bytes} -> ${c.wire_bytes} bytes`);
console.log("independently verified lossless:", verify(c.wire, c.certificate.hash));
console.log("tampered verifies:", verify(c.wire.replace("file-0", "file-X"), c.certificate.hash), "(must be false)");

const request = {
  model: "gpt-4o",
  messages: [
    { role: "user", content: "list the artifacts" },
    { role: "tool", tool_call_id: "c1", content: ls },
  ],
};
const before = JSON.stringify(request).length;
const session = new Session();
const out = session.rewrite(request);
const after = JSON.stringify(out.request).length;
console.log(`whole request: ${before} -> ${after} bytes (${(100 * (before - after) / before).toFixed(1)}% smaller)`);
console.log("stats:", session.stats());
session.close();
