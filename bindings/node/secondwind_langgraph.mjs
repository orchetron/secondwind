// LangGraph.js integration: compress accumulated tool outputs before each model call.
//
//   import { compressPreModelHook } from "secondwind/langgraph";
//   const agent = createReactAgent({ llm: model, tools, preModelHook: compressPreModelHook() });
//
// Returns compressed messages as `llmInputMessages` so LangGraph feeds the model the compressed form
// without mutating the durable `messages` history. Any failure returns no update (never breaks the agent).

import { Session } from "./secondwind.mjs";

const typeOf = (message) => message.getType?.() ?? message._getType?.();

// Map LangChain message objects to the OpenAI request shape the core reads. Non-tool roles are
// carried through so the maturation gate sees the real conversation sequence.
function toOpenAI(messages) {
  return messages.map((message) => {
    const content = message.content;
    switch (typeOf(message)) {
      case "tool":
        return { role: "tool", tool_call_id: message.tool_call_id, content };
      case "ai": {
        const out = { role: "assistant", content: typeof content === "string" ? content : "" };
        if (Array.isArray(message.tool_calls) && message.tool_calls.length) {
          out.tool_calls = message.tool_calls.map((call) => ({
            id: call.id,
            type: "function",
            function: { name: call.name, arguments: JSON.stringify(call.args ?? {}) },
          }));
        }
        return out;
      }
      case "system":
        return { role: "system", content };
      default:
        return { role: "user", content };
    }
  });
}

// New message of the same class with replaced content; original stays untouched (durable state keeps
// it byte-for-byte).
const withContent = (message, content) => new message.constructor({ ...message.lc_kwargs, content });

export function compressPreModelHook({ session, model = "gpt-4o", resolver } = {}) {
  const sess = session ?? new Session({ resolver });

  return (state) => {
    try {
      const messages = Array.isArray(state) ? state : state.messages ?? [];
      const out = sess.rewrite({ model, messages: toOpenAI(messages) });
      const rewritten = out?.request?.messages ?? [];

      const byId = new Map();
      for (const m of rewritten) if (m && m.role === "tool") byId.set(m.tool_call_id, m.content);

      let changed = false;
      const result = messages.map((message) => {
        const next = byId.get(message.tool_call_id);
        if (typeOf(message) === "tool" && typeof message.content === "string" && typeof next === "string" && next !== message.content) {
          changed = true;
          return withContent(message, next);
        }
        return message;
      });

      return changed ? { llmInputMessages: result } : {};
    } catch {
      return {}; // never break the agent
    }
  };
}
