"""Verify the ASGI middleware against real Starlette: the app receives the compressed request body,
and non-LLM paths pass through untouched. Run: python test_asgi.py"""

from starlette.applications import Starlette
from starlette.responses import JSONResponse
from starlette.routing import Route
from starlette.testclient import TestClient

from secondwind.asgi import SecondwindMiddleware

captured = {}


async def completions(request):
    captured["payload"] = await request.json()  # what the app sees AFTER the middleware
    return JSONResponse({"ok": True})


async def other(request):
    captured["other"] = await request.json()
    return JSONResponse({"ok": True})


app = Starlette(routes=[Route("/v1/chat/completions", completions, methods=["POST"]), Route("/other", other, methods=["POST"])])
app.add_middleware(SecondwindMiddleware)
client = TestClient(app)

ls = "\n".join(f"-rw-r--r-- 1 root wheel {100 + i * 37:>6} file-{i}.txt" for i in range(40))
request = {"model": "gpt-4o", "messages": [{"role": "user", "content": "ls -l"}, {"role": "tool", "tool_call_id": "c1", "content": ls}]}

client.post("/v1/chat/completions", json=request)
tool_out = captured["payload"]["messages"][1]["content"]
assert tool_out != ls and len(tool_out) < len(ls), "the app should receive the compressed tool output"
assert captured["payload"]["messages"][0]["content"] == "ls -l", "non-tool messages are untouched"
print(f"PASS: app received compressed tool output ({len(ls)} -> {len(tool_out)} bytes), other messages intact")

# A body with no `messages` passes through byte-for-byte.
passthrough = {"hello": "world", "n": 1}
client.post("/other", json=passthrough)
assert captured["other"] == passthrough, "a non-LLM body must pass through untouched"
print("PASS: non-LLM request body passed through untouched")
