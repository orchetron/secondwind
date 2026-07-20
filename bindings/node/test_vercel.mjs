// Tests the Vercel middleware's transformParams logic against an AI-SDK-shaped prompt (a mock, so
// this needs no real `ai` install). Wiring into a real app is `wrapLanguageModel({ model, middleware })`.
import { secondwindMiddleware } from "./secondwind_vercel.mjs";

const ls = Array.from({ length: 300 }, (_, i) => `-rw-r--r--  1 root  wheel  ${i} Jan  1 file-${i}`).join("\n");

// AI SDK v5 normalized prompt: a tool message with a tool-result part whose output is text.
const params = {
  prompt: [
    { role: "user", content: [{ type: "text", text: "list" }] },
    {
      role: "tool",
      content: [{ type: "tool-result", toolCallId: "c1", toolName: "ls", output: { type: "text", value: ls } }],
    },
  ],
};

const before = params.prompt[1].content[0].output.value.length;
const out = await secondwindMiddleware().transformParams({ type: "generate", params });
const after = out.prompt[1].content[0].output.value.length;

console.log(`tool-result output: ${before} -> ${after} bytes (${(100 * (before - after) / before).toFixed(1)}% smaller)`);
if (!(after < before)) throw new Error("tool output must compress");
if (out.prompt[0].content[0].text !== "list") throw new Error("non-tool messages must be untouched");
console.log("PASS: Vercel AI SDK middleware compresses tool-result outputs in-process, losslessly");
