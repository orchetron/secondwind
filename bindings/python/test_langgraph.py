"""Verify the LangGraph pre_model_hook against real langgraph: inside a running agent loop the model
RECEIVES the compressed tool output, while the durable state history keeps the original byte-for-byte
(compress what is sent, keep the truth). Run: python test_langgraph.py"""

import warnings

from langchain_core.language_models.chat_models import BaseChatModel
from langchain_core.messages import AIMessage, HumanMessage, ToolMessage
from langchain_core.outputs import ChatGeneration, ChatResult
from langgraph.prebuilt import create_react_agent

from secondwind.langchain import compress_pre_model_hook

warnings.filterwarnings("ignore")  # create_react_agent V1 deprecation note is not under test

LS = "\n".join(f"-rw-r--r-- 1 root wheel {100 + i * 37:>6} file-{i}.txt" for i in range(60))


class ScriptedModel(BaseChatModel):
    """A minimal real chat model that replays scripted responses and records every message list it
    is asked to generate from, so the test can inspect exactly what the agent sent the model."""

    scripted: list
    seen: list = []

    @property
    def _llm_type(self) -> str:
        return "scripted"

    def bind_tools(self, tools, **kwargs):
        return self  # tool calls are scripted, so binding is a no-op

    def _generate(self, messages, stop=None, run_manager=None, **kwargs):
        self.seen.append(list(messages))
        message = self.scripted[len(self.seen) - 1]
        return ChatResult(generations=[ChatGeneration(message=message)])


def list_files() -> str:
    """List the files."""
    return LS


model = ScriptedModel(
    scripted=[
        AIMessage(content="", tool_calls=[{"name": "list_files", "args": {}, "id": "c1"}]),
        AIMessage(content="done"),
    ],
    seen=[],
)

agent = create_react_agent(model, tools=[list_files], pre_model_hook=compress_pre_model_hook())
result = agent.invoke({"messages": [HumanMessage("list the files")]})

# The agent called the model twice: once to request the tool, once after the tool ran.
assert len(model.seen) == 2, f"expected two model calls, got {len(model.seen)}"

# On the second call the tool output is in play: the model must have READ the compressed form.
sent_tool = next(m for m in model.seen[1] if isinstance(m, ToolMessage))
assert sent_tool.content != LS, "the model should not receive the raw tool output"
assert len(sent_tool.content) < len(LS), "the tool output the model reads should be smaller"
print(f"PASS: model reads compressed tool output ({len(LS)} -> {len(sent_tool.content)} chars)")

# The durable state history keeps the original byte-for-byte (llm_input_messages does not mutate it).
kept_tool = next(m for m in result["messages"] if isinstance(m, ToolMessage))
assert kept_tool.content == LS, "the persisted state must keep the original tool output byte-for-byte"
print("PASS: state history preserves the original tool output byte-for-byte")

# The loop ran end to end and produced the final answer.
assert result["messages"][-1].content == "done", "the agent should finish the loop"
print("PASS: create_react_agent(pre_model_hook=...) runs the loop end to end")
