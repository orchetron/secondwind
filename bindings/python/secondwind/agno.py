"""Agno integration: lossless replacement for Agno's tool-result compression.

    from agno.agent import Agent
    from secondwind.agno import SecondwindCompressionManager

    agent = Agent(
        model=...,
        compress_tool_results=True,
        compression_manager=SecondwindCompressionManager(compress_tool_results=True),
    )

Keeps Agno's gating (when/which results to compress) but swaps its lossy per-result LLM summarizer
for secondwind: lossless, no model call. Any failure returns None so Agno keeps the original.
"""

try:
    from agno.compression import CompressionManager as _CompressionManager

    _AGNO = True
except ImportError:  # agno optional
    _CompressionManager = object
    _AGNO = False

from . import Session


class SecondwindCompressionManager(_CompressionManager):
    _sw_session = None

    def _session(self):
        if self._sw_session is None:
            self._sw_session = Session()
        return self._sw_session

    def _compress_lossless(self, content):
        if not isinstance(content, str) or not content:
            return None
        try:
            out = self._session().rewrite(
                {"model": "", "messages": [{"role": "tool", "tool_call_id": "t", "content": content}]}
            )
            new = out["request"]["messages"][0]["content"]
            return new if isinstance(new, str) and new != content else None
        except Exception:  # fail open: Agno keeps the original
            return None

    # Override both engine hooks so Agno's compress()/acompress() call secondwind, not an LLM.
    def _compress_tool_result(self, tool_msg, *args, **kwargs):
        return self._compress_lossless(getattr(tool_msg, "content", None))

    async def _acompress_tool_result(self, tool_msg, *args, **kwargs):
        return self._compress_lossless(getattr(tool_msg, "content", None))
