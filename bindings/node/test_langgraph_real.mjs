// Verifies the LangGraph.js pre-model hook against REAL @langchain/langgraph: inside a running agent
// loop the model RECEIVES the compressed tool output, while the durable state keeps the original
// byte-for-byte. A scripted tool-calling model records exactly what the agent hands it.
// Run: SECONDWIND_LIB=../../target/release/libsecondwind.dylib node bindings/node/test_langgraph_real.mjs

import assert from "node:assert";
import { BaseChatModel } from "@langchain/core/language_models/chat_models";
import { AIMessage, HumanMessage, ToolMessage } from "@langchain/core/messages";
import { tool } from "@langchain/core/tools";
import { createReactAgent } from "@langchain/langgraph/prebuilt";
import { z } from "zod";
import { compressPreModelHook } from "./secondwind_langgraph.mjs";

const LS = Array.from({ length: 60 }, (_, i) => `-rw-r--r-- 1 root wheel ${String(100 + i * 37).padStart(6)} file-${i}.txt`).join("\n");

// A minimal real chat model that replays scripted responses and records every message list it is
// asked to generate from, so the test can inspect exactly what the agent sent the model.
class ScriptedModel extends BaseChatModel {
  constructor(scripted) {
    super({});
    this.scripted = scripted;
    this.seen = [];
  }
  _llmType() {
    return "scripted";
  }
  bindTools() {
    return this; // tool calls are scripted, so binding is a no-op
  }
  async _generate(messages) {
    this.seen.push(messages);
    const message = this.scripted[this.seen.length - 1];
    return { generations: [{ text: typeof message.content === "string" ? message.content : "", message }] };
  }
}

const listFiles = tool(async () => LS, {
  name: "list_files",
  description: "List the files.",
  schema: z.object({}),
});

const model = new ScriptedModel([
  new AIMessage({ content: "", tool_calls: [{ name: "list_files", args: {}, id: "c1" }] }),
  new AIMessage({ content: "done" }),
]);

const agent = createReactAgent({ llm: model, tools: [listFiles], preModelHook: compressPreModelHook() });
const result = await agent.invoke({ messages: [new HumanMessage("list the files")] });

// The agent called the model twice: once to request the tool, once after the tool ran.
assert.equal(model.seen.length, 2, `expected two model calls, got ${model.seen.length}`);

// On the second call the model must have READ the compressed tool output.
const sent = model.seen[1].find((m) => m instanceof ToolMessage);
assert.ok(sent, "a tool message should reach the model on the second call");
assert.notEqual(sent.content, LS, "the model should not receive the raw tool output");
assert.ok(sent.content.length < LS.length, "the tool output the model reads should be smaller");
console.log(`PASS: model reads compressed tool output (${LS.length} -> ${sent.content.length} chars)`);

// The durable state keeps the original byte-for-byte (llmInputMessages does not mutate it).
const kept = result.messages.find((m) => m instanceof ToolMessage);
assert.equal(kept.content, LS, "the persisted state must keep the original tool output byte-for-byte");
console.log("PASS: state history preserves the original tool output byte-for-byte");

// The loop ran end to end and produced the final answer.
assert.equal(result.messages.at(-1).content, "done", "the agent should finish the loop");
console.log("PASS: createReactAgent({ preModelHook }) runs the loop end to end");
