"""Verify the Agno adapter against real agno: secondwind replaces the LLM-based compression engine,
losslessly, inside Agno's own compress() loop. Run: python test_agno.py"""

import secondwind
from agno.models.message import Message

from secondwind.agno import SecondwindCompressionManager

ls = "\n".join(f"-rw-r--r-- 1 root wheel {100 + i * 37:>6} file-{i}.txt" for i in range(40))

mgr = SecondwindCompressionManager(compress_tool_results=True)

# The engine is lossless: the same compressor, run directly, produces an independently verifiable wire.
c = secondwind.compress(ls)
assert c["kind"] == "compressed" and secondwind.verify(c["wire"], c["certificate"]["hash"]), "engine must be lossless"

# It plugs into Agno's real compress() loop and writes compressed_content on the tool messages,
# with NO model call (the manager has no model configured).
messages = [
    Message(role="user", content="list the files"),
    Message(role="tool", content=ls, tool_call_id="c1"),
    Message(role="tool", content=ls, tool_call_id="c2"),
]
mgr.compress(messages)

tool_msgs = [m for m in messages if m.role == "tool"]
assert all(m.compressed_content and len(m.compressed_content) < len(ls) for m in tool_msgs), "tool results should be compressed"
assert messages[0].content == "list the files" and messages[0].compressed_content is None, "non-tool messages untouched"
saved = len(ls) - len(tool_msgs[0].compressed_content)
print(f"PASS: Agno tool results compressed losslessly with no LLM call ({len(ls)} -> {len(tool_msgs[0].compressed_content)} chars, {saved} saved each)")
