"""Whole-request rewrite in-process: an OpenAI-style request with a big tool output, compressed
through the Rust core over the C ABI. This is the shape a LiteLLM/ASGI adapter wraps in one line."""

import json

import secondwind

# A tool call whose result is a big `ls -la` output: the exact thing that bloats agent context.
ls_output = "\n".join(
    f"-rw-r--r--  1 root  wheel  {100 + i * 37:>7} Jan  1 12:00 file-{i}.txt" for i in range(300)
)
request = {
    "model": "gpt-4o",
    "messages": [
        {"role": "user", "content": "list the build artifacts"},
        {"role": "assistant", "tool_calls": [
            {"id": "call_1", "type": "function", "function": {"name": "ls", "arguments": "{}"}}
        ]},
        {"role": "tool", "tool_call_id": "call_1", "content": ls_output},
    ],
}

before = len(json.dumps(request))
with secondwind.Session() as session:
    out = session.rewrite(request)

after = len(json.dumps(out["request"]))
tool_after = out["request"]["messages"][2]["content"]

print("stats:", out["stats"])
print(f"whole request: {before} -> {after} bytes ({100 * (before - after) / before:.1f}% smaller)")
print(f"tool output: {len(ls_output)} -> {len(tool_after)} bytes")
print("the other messages are untouched:", out["request"]["messages"][0]["content"])
