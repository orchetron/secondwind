# secondwind

Lossless, provable, composable LLM tool-output compression, in-process.

```python
import secondwind

session = secondwind.Session()
out = session.rewrite(request)          # compress a whole request's tool outputs
print(out["stats"])                     # tokens saved
secondwind.compress(block)              # or compress a single tool-output block

# one-line LiteLLM integration:
import litellm
from secondwind.litellm import SecondwindCallback
litellm.callbacks = [SecondwindCallback()]

# ASGI gateway (Starlette / FastAPI): rewrite request bodies before you forward them upstream
from secondwind.asgi import SecondwindMiddleware
app.add_middleware(SecondwindMiddleware)

# LangChain (LCEL): compress tool outputs before the model call
from secondwind.langchain import compress_tool_outputs
chain = compress_tool_outputs() | model

# LangGraph: compress the tool outputs an agent loop re-sends before each model call
from secondwind.langchain import compress_pre_model_hook
agent = create_react_agent(model, tools, pre_model_hook=compress_pre_model_hook())

# Agno: a lossless drop-in for its LLM-based tool-result compression
from secondwind.agno import SecondwindCompressionManager
agent = Agent(model=..., compress_tool_results=True,
              compression_manager=SecondwindCompressionManager(compress_tool_results=True))

# Strands Agents: compress tool results as they are produced
from secondwind.strands import SecondwindHooks
agent = Agent(model=..., tools=[...], hooks=[SecondwindHooks()])

# Cursor: a postToolUse hook that rewrites MCP tool output the model sees
#   ~/.cursor/hooks.json -> { "version": 1, "hooks": { "postToolUse": [ { "command": "python -m secondwind.cursor" } ] } }
```

```python
# Bring your own codec: it competes in the best-of-N search, and secondwind proves
# decode(encode(x)) == x for every block, so a wrong codec is dropped, never shipped.
class MyCodec:
    def encode(self, text): ...   # -> str | None
    def decode(self, wire): ...   # -> str | None
session = secondwind.Session(codec=MyCodec())     # reckless is safe: proven per-instance
session = secondwind.Session(proposers=False)     # turn the aggressive search off

# Offload big blocks and recover them on demand, or back the store with Redis / S3 / a database.
session = secondwind.Session(resolver="resolve", offload_dir="~/.secondwind/offload")
session = secondwind.Session(store=MyStore())     # any object with put(id, value) and get(id)
```

Every result is lossless and independently verifiable (`secondwind.verify(wire, hash)`); the native
library is bundled, so there is no build step and no model download.
