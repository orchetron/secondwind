"""Cursor integration: a postToolUse hook that losslessly compresses MCP tool output.

Cursor's one supported output-rewrite seam is `postToolUse` -> `updated_mcp_tool_output`. This module
IS that hook: reads Cursor's JSON on stdin, compresses the tool output, writes the replacement on
stdout. Any error or non-compressible output returns {} so the model sees the original.

Configure ~/.cursor/hooks.json (or a project .cursor/hooks.json):

    {
      "version": 1,
      "hooks": { "postToolUse": [ { "command": "python -m secondwind.cursor" } ] }
    }

For chat / plan mode (a different seam), point Cursor's "Override OpenAI Base URL" at a running
`secondwind serve` proxy. Cursor's hook field names are beta and version-dependent; if yours differ,
set FIELD_IN / FIELD_OUT below.
"""

import json
import sys

from . import compress

FIELD_IN = "tool_output"
FIELD_OUT = "updated_mcp_tool_output"


def _compress_text(text):
    if not isinstance(text, str) or not text:
        return None
    try:
        out = compress(text)
        if out.get("kind") == "compressed":
            return out["wire"]
    except Exception:
        pass
    return None


def rewrite_output(payload):
    """The hook's stdout for a postToolUse payload, or {} to leave the tool output unchanged."""
    output = payload.get(FIELD_IN)
    if isinstance(output, str):
        wire = _compress_text(output)
        return {FIELD_OUT: wire} if wire else {}
    # MCP content-block shape: {"content": [{"text": ...}, ...]}
    if isinstance(output, dict) and isinstance(output.get("content"), list):
        content = [dict(block) if isinstance(block, dict) else block for block in output["content"]]
        changed = False
        for block in content:
            if isinstance(block, dict) and isinstance(block.get("text"), str):
                wire = _compress_text(block["text"])
                if wire:
                    block["text"] = wire
                    changed = True
        return {FIELD_OUT: {**output, "content": content}} if changed else {}
    return {}


def main():
    try:
        payload = json.load(sys.stdin)
    except Exception:
        sys.stdout.write("{}")
        return
    json.dump(rewrite_output(payload), sys.stdout)


if __name__ == "__main__":
    main()
