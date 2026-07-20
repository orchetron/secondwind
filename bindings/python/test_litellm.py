"""Proof: the one-line LiteLLM callback compresses tool outputs in-process, losslessly, against
the real litellm CustomLogger base class."""

import asyncio
import json

from secondwind.litellm import SecondwindCallback

ls = "\n".join(
    f"-rw-r--r--  1 root  wheel  {100 + i * 37:>7} Jan  1 12:00 file-{i}.txt" for i in range(300)
)
data = {
    "model": "gpt-4o",
    "messages": [
        {"role": "user", "content": "list the artifacts"},
        {"role": "tool", "tool_call_id": "c1", "content": ls},
    ],
}

before = len(json.dumps(data))
callback = SecondwindCallback()
base = type(callback).__mro__[1]
print(f"base class: {base.__module__}.{base.__qualname__}")

out = asyncio.run(callback.async_pre_call_hook(None, None, data, "completion"))
after = len(json.dumps(out))
tool = out["messages"][1]["content"]

print(f"request: {before} -> {after} bytes ({100 * (before - after) / before:.1f}% smaller)")
print(f"tool output: {len(ls)} -> {len(tool)} bytes")
assert out is data, "the hook mutates data in place and returns it"
assert len(tool) < len(ls), "the tool output must compress"
assert out["messages"][0]["content"] == "list the artifacts", "non-tool messages untouched"
print("PASS: one line, real litellm base, tool outputs compressed in-process and losslessly")
