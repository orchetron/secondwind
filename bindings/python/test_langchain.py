"""Verify the LangChain adapter against real langchain-core: the tool message the model receives is
compressed, and the transform composes in an LCEL chain. Run: python test_langchain.py"""

from langchain_core.language_models.fake_chat_models import GenericFakeChatModel
from langchain_core.messages import AIMessage, HumanMessage, ToolMessage

from secondwind.langchain import compress_tool_outputs

ls = "\n".join(f"-rw-r--r-- 1 root wheel {100 + i * 37:>6} file-{i}.txt" for i in range(40))
messages = [
    HumanMessage("list the files"),
    ToolMessage(content=ls, tool_call_id="c1"),
]

transform = compress_tool_outputs()

# The transform output is exactly what the model receives downstream in `transform | model`.
out = transform.invoke(messages)
tool_msg = next(m for m in out if isinstance(m, ToolMessage))
assert tool_msg.content != ls and len(tool_msg.content) < len(ls), "the tool message should be compressed"
assert out[0].content == "list the files", "the human message is untouched"
print(f"PASS: tool message compressed for the model ({len(ls)} -> {len(tool_msg.content)} chars)")

# It composes as an LCEL chain with a real chat model interface.
model = GenericFakeChatModel(messages=iter([AIMessage("done")]))
chain = transform | model
result = chain.invoke(messages)
assert result.content == "done", "the chain should run end to end"
print("PASS: compress_tool_outputs() | model runs as an LCEL chain")
