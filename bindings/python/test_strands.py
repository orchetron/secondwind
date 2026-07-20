"""Verify the Strands adapter against real strands: registering SecondwindHooks and firing a real
AfterToolCallEvent through the real HookRegistry compresses the tool result losslessly, in place.
Run: python test_strands.py"""

import secondwind
from strands.hooks import AfterToolCallEvent, HookRegistry

from secondwind.strands import SecondwindHooks

ls = "\n".join(f"-rw-r--r-- 1 root wheel {100 + i * 37:>6} file-{i}.txt" for i in range(40))

# The engine is lossless.
c = secondwind.compress(ls)
assert c["kind"] == "compressed" and secondwind.verify(c["wire"], c["certificate"]["hash"]), "engine must be lossless"

# Register through the real registry and fire a real AfterToolCallEvent.
registry = HookRegistry()
SecondwindHooks().register_hooks(registry)

result = {"toolUseId": "t1", "status": "success", "content": [{"text": ls}]}
event = AfterToolCallEvent(
    agent=None,
    selected_tool=None,
    tool_use={"toolUseId": "t1", "name": "list_files", "input": {}},
    invocation_state={},
    result=result,
)
registry.invoke_callbacks(event)

compressed = result["content"][0]["text"]
assert compressed != ls and len(compressed) < len(ls), "the tool result text should be compressed in place"
print(f"PASS: Strands tool result compressed losslessly via AfterToolCallEvent ({len(ls)} -> {len(compressed)} chars)")
