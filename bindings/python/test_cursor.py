"""Verify the Cursor postToolUse hook: it rewrites tool output losslessly, handles the MCP
content-block shape, leaves incompressible output alone, and runs as a real stdin/stdout process.
Cursor itself is not runnable here, so this exercises the hook against synthetic Cursor payloads.
Run: python test_cursor.py"""

import json
import subprocess
import sys

import secondwind
from secondwind.cursor import FIELD_IN, FIELD_OUT, rewrite_output

ls = "\n".join(f"-rw-r--r-- 1 root wheel {100 + i * 37:>6} file-{i}.txt" for i in range(40))

# String tool output -> updated_mcp_tool_output with a shorter, verifiably-lossless wire.
out = rewrite_output({"hook_event_name": "postToolUse", "tool_name": "run", FIELD_IN: ls})
wire = out[FIELD_OUT]
assert isinstance(wire, str) and len(wire) < len(ls), "string tool output should be replaced by a shorter wire"
c = secondwind.compress(ls)
assert secondwind.verify(c["wire"], c["certificate"]["hash"]), "engine is lossless"
print(f"PASS: string tool_output rewritten losslessly ({len(ls)} -> {len(wire)} chars)")

# MCP content-block shape -> text blocks compressed in place.
mcp = rewrite_output({FIELD_IN: {"content": [{"type": "text", "text": ls}], "isError": False}})
assert len(mcp[FIELD_OUT]["content"][0]["text"]) < len(ls), "content-block text should be compressed"
assert mcp[FIELD_OUT]["isError"] is False, "other fields preserved"
print("PASS: MCP content-block text compressed, other fields preserved")

# Incompressible / tiny output -> no change ({}), so the model sees the original.
assert rewrite_output({FIELD_IN: "ok"}) == {}, "tiny output must be left unchanged"
assert rewrite_output({"tool_name": "x"}) == {}, "missing output must be left unchanged"
print("PASS: incompressible or absent output left unchanged")

# Runs as a real hook process: JSON on stdin -> JSON on stdout.
proc = subprocess.run(
    [sys.executable, "-m", "secondwind.cursor"],
    input=json.dumps({FIELD_IN: ls}),
    capture_output=True,
    text=True,
)
piped = json.loads(proc.stdout)
assert FIELD_OUT in piped and len(piped[FIELD_OUT]) < len(ls), "the hook process should emit the replacement on stdout"
print("PASS: runs as a stdin/stdout hook process")
