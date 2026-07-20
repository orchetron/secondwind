"""Strands Agents integration: compress tool results losslessly as they are produced.

    from strands import Agent
    from secondwind.strands import SecondwindHooks

    agent = Agent(model=..., tools=[...], hooks=[SecondwindHooks()])

Registers on AfterToolCallEvent and rewrites each tool result's text before it reaches the model,
in-process. Any failure leaves the result untouched.
"""

try:
    from strands.hooks import AfterToolCallEvent, HookProvider

    _STRANDS = True
except ImportError:  # strands optional
    HookProvider = object
    _STRANDS = False

from . import Session


class SecondwindHooks(HookProvider):
    def __init__(self, resolver=None):
        self._session = Session(resolver=resolver)

    def register_hooks(self, registry, **kwargs):
        registry.add_callback(AfterToolCallEvent, self._on_after_tool_call)

    def _on_after_tool_call(self, event):
        try:
            result = getattr(event, "result", None)
            content = result.get("content") if isinstance(result, dict) else None
            if not content:
                return
            tool_use_id = result.get("toolUseId", "t")
            for block in content:
                text = block.get("text") if isinstance(block, dict) else None
                if not isinstance(text, str) or not text:
                    continue
                out = self._session.rewrite(
                    {"model": "", "messages": [{"role": "tool", "tool_call_id": tool_use_id, "content": text}]}
                )
                new = out["request"]["messages"][0]["content"]
                if isinstance(new, str) and new != text:
                    block["text"] = new  # mutate the result dict in place, before it reaches the model
        except Exception:  # never break the run
            pass
