"""ASGI middleware: compress LLM tool outputs for a self-hosted gateway with one line.

    from secondwind.asgi import SecondwindMiddleware
    app.add_middleware(SecondwindMiddleware)          # Starlette / FastAPI
    # or wrap any ASGI app directly:  app = SecondwindMiddleware(app)

Rewrites tool outputs in a JSON chat-completion body in-process before your app forwards upstream.
Only touches POST requests whose JSON body has `messages`; any failure falls back to the original body.
"""

import json

from . import Session


async def _read_body(receive):
    chunks = []
    while True:
        message = await receive()
        if message["type"] == "http.request":
            chunks.append(message.get("body", b""))
            if not message.get("more_body", False):
                break
        elif message["type"] == "http.disconnect":
            break
    return b"".join(chunks)


def _replay(body):
    # Replay the (possibly rewritten) body as one request event, then disconnect.
    done = False

    async def receive():
        nonlocal done
        if not done:
            done = True
            return {"type": "http.request", "body": body, "more_body": False}
        return {"type": "http.disconnect"}

    return receive


class SecondwindMiddleware:
    def __init__(self, app, resolver=None, home=None, paths=None):
        """`paths`: a prefix (or tuple) to limit rewriting to your LLM route; default touches any
        POST whose JSON body has `messages`."""
        self.app = app
        self._session = Session(resolver=resolver, home=home)
        self._paths = (paths,) if isinstance(paths, str) else tuple(paths) if paths else None

    async def __call__(self, scope, receive, send):
        if scope["type"] != "http" or scope.get("method") != "POST":
            return await self.app(scope, receive, send)
        if self._paths and not scope["path"].startswith(self._paths):
            return await self.app(scope, receive, send)

        body = await _read_body(receive)
        new_body = self._maybe_rewrite(body)
        if new_body is body:
            return await self.app(scope, _replay(body), send)

        # Body length changed: correct Content-Length before the app reads it.
        headers = [(k, v) for (k, v) in scope["headers"] if k.lower() != b"content-length"]
        headers.append((b"content-length", str(len(new_body)).encode()))
        return await self.app({**scope, "headers": headers}, _replay(new_body), send)

    def _maybe_rewrite(self, body):
        try:
            payload = json.loads(body)
        except Exception:
            return body
        if not isinstance(payload, dict) or "messages" not in payload:
            return body
        try:
            out = self._session.rewrite(payload)
            # Nothing compressed: forward the caller's exact bytes. Re-serializing an unchanged
            # body would shift whitespace and key order and needlessly bust the upstream cache.
            if not out["stats"].get("blocks_rewritten"):
                return body
            return json.dumps(out["request"]).encode("utf-8")
        except Exception:  # never break the request
            return body

    def close(self):
        self._session.close()
