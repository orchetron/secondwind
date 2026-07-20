// Native-binding parity: the SAME assertions must hold whether this runs under Node (koffi backend)
// or Bun (bun:ffi backend). Run: node bindings/node/test_native_parity.mjs  AND  bun ...same...
import { abiVersion, compress, verify, Session } from "./secondwind.mjs";

const runtime = typeof globalThis.Bun !== "undefined" ? "bun" : "node";
let passed = 0;
function check(name, cond) {
  if (!cond) throw new Error(`[${runtime}] FAILED: ${name}`);
  passed += 1;
}

check("abi version is 1", abiVersion() === 1);

// compress + independent verify + tamper rejection
const ls = Array.from({ length: 40 }, (_, i) => `-rw-r--r-- 1 root wheel ${String(100 + i * 37).padStart(6)} file-${i}.txt`).join("\n");
const c = compress(ls);
check("compresses via columns", c.kind === "compressed" && c.transform === "columns");
check("saves tokens", c.tokens_saved > 0);
check("independently verifies lossless", verify(c.wire, c.certificate.hash) === true);
check("rejects a tampered wire", verify(c.wire.replace("file-0", "file-X"), c.certificate.hash) === false);

// whole-request rewrite: only the tool output changes; a resend is byte-identical and counted once
const request = {
  model: "gpt-4o",
  messages: [
    { role: "user", content: "ls -l" },
    { role: "tool", tool_call_id: "c1", content: ls },
  ],
};
const session = new Session();
const first = session.rewrite(request);
check("tool output shrank", first.request.messages[1].content.length < ls.length);
check("other messages untouched", first.request.messages[0].content === "ls -l");
const second = session.rewrite(request);
check("resend is byte-identical", JSON.stringify(first.request) === JSON.stringify(second.request));
check("resend counted once", session.stats().blocks_first_seen === first.stats.blocks_first_seen);
session.close();

// store-callback: an aged, offload-favoring block routes its bytes through a host dict backend.
const bigObject = "{" + Array.from({ length: 200 }, (_, i) => `"k${i}":"value number ${i} for record ${i}"`).join(",") + "}";
const backend = new Map();
const store = {
  put: (id, val) => (backend.set(id, val), true),
  get: (id) => backend.get(id) ?? null,
};
const aged = {
  model: "gpt-4o",
  messages: [
    { role: "user", content: "fetch the records" },
    { role: "tool", tool_call_id: "c1", content: bigObject },
    { role: "assistant", content: "step 1" },
    { role: "user", content: "go on" },
    { role: "assistant", content: "step 2" },
    { role: "user", content: "go on" },
    { role: "assistant", content: "step 3" },
    { role: "user", content: "go on" },
    { role: "assistant", content: "step 4" },
    { role: "user", content: "summarize" },
  ],
};
const withStore = new Session({ resolver: "resolve_context", store });
const outcome = withStore.rewrite(aged);
const toolContent = outcome.request.messages[1].content;
check("aged block offloaded to a marker", toolContent.includes("<<swload:"));
check("host store received the bytes (put)", backend.size === 1);
check("stored bytes reconstruct the original", [...backend.values()][0] === bigObject);
withStore.close();

console.log(`[${runtime}] all ${passed} native-parity checks passed (backend: ${runtime === "bun" ? "bun:ffi" : "koffi"})`);
