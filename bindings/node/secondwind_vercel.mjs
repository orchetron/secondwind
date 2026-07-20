// Vercel AI SDK middleware: lossless compression with one wrap.
//
//   import { secondwindMiddleware } from "./secondwind_vercel.mjs";
//   const model = wrapLanguageModel({ model: openai("gpt-4o"), middleware: secondwindMiddleware() });
//
// Pre-call hook that compresses tool outputs in-process, mapping the AI SDK's tool-result parts to
// the core (its prompt shape isn't raw OpenAI messages).

import { compress } from "./secondwind.mjs";

// get/set handle for a tool-result part's text payload across AI SDK versions; null for shapes we
// leave alone (structured/non-text outputs).
function textHandle(part) {
  if (part.output && typeof part.output.value === "string") {
    return { get: () => part.output.value, set: (v) => (part.output = { type: "text", value: v }) };
  }
  if (typeof part.result === "string") {
    return { get: () => part.result, set: (v) => (part.result = v) };
  }
  return null;
}

export function secondwindMiddleware() {
  return {
    transformParams: async ({ params }) => {
      try {
        for (const message of params.prompt ?? []) {
          if (message.role !== "tool" || !Array.isArray(message.content)) continue;
          for (const part of message.content) {
            if (part.type !== "tool-result") continue;
            const handle = textHandle(part);
            if (!handle) continue;
            const result = compress(handle.get());
            if (result.kind === "compressed") handle.set(result.wire);
          }
        }
      } catch {
        // Never break the call: on error, return params untouched.
      }
      return params;
    },
  };
}
