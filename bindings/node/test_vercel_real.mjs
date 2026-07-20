// Verifies the middleware against the REAL Vercel AI SDK (v7): a mock model captures the prompt it
// is handed, so we can confirm the SDK invoked our transformParams and the model received the
// COMPRESSED tool output. Run: bun bindings/node/test_vercel_real.mjs
import { generateText, wrapLanguageModel } from "ai";
import { MockLanguageModelV3 } from "ai/test";
import { secondwindMiddleware } from "./secondwind_vercel.mjs";

const ls = Array.from(
  { length: 300 },
  (_, i) => `-rw-r--r--  1 root  wheel  ${i} Jan  1 file-${i}`,
).join("\n");

let capturedPrompt;
const mock = new MockLanguageModelV3({
  doGenerate: async (options) => {
    capturedPrompt = options.prompt;
    return {
      content: [{ type: "text", text: "done" }],
      finishReason: "stop",
      usage: { inputTokens: 1, outputTokens: 1, totalTokens: 2 },
      warnings: [],
    };
  },
});

const model = wrapLanguageModel({ model: mock, middleware: secondwindMiddleware() });

await generateText({
  model,
  messages: [
    { role: "user", content: "list the artifacts" },
    { role: "assistant", content: [{ type: "tool-call", toolCallId: "c1", toolName: "ls", input: {} }] },
    { role: "tool", content: [{ type: "tool-result", toolCallId: "c1", toolName: "ls", output: { type: "text", value: ls } }] },
  ],
});

const toolMessage = capturedPrompt.find((m) => m.role === "tool");
const part = toolMessage.content.find((p) => p.type === "tool-result");
const received = part.output?.value ?? part.result;

console.log(`tool output the model received: ${ls.length} -> ${received.length} bytes`);
if (!(received.length < ls.length)) throw new Error("the model received an uncompressed tool output");
if (capturedPrompt[0].content[0].text !== "list the artifacts") throw new Error("user message was altered");
console.log("PASS: real AI SDK v7 invoked the middleware; the model got the compressed tool output");
