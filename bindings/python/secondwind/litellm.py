"""LiteLLM callback: add secondwind lossless compression with one line.

    import litellm
    from secondwind.litellm import SecondwindCallback
    litellm.callbacks = [SecondwindCallback()]
"""

import logging

try:
    from litellm.integrations.custom_logger import CustomLogger as _CustomLogger
except ImportError:  # litellm optional
    _CustomLogger = object

from . import Session

logger = logging.getLogger("secondwind")


class SecondwindCallback(_CustomLogger):
    """One-line LiteLLM integration; compresses every request's tool outputs in-process, losslessly.
    Any failure falls back to the original request."""

    def __init__(self, resolver: str | None = None, home: str | None = None):
        super().__init__()
        self._session = Session(resolver=resolver, home=home)

    async def async_pre_call_hook(self, user_api_key_dict, cache, data, call_type):
        try:
            out = self._session.rewrite(data)
            if isinstance(out, dict) and "request" in out:
                data.update(out["request"])
        except Exception as exc:  # never break the call
            logger.warning("secondwind compression skipped: %s", exc)
        return data

    def close(self):
        self._session.close()
