"""LangChain and LangGraph integration: compress tool-message outputs losslessly, two ways.

LCEL chain:

    from secondwind.langchain import compress_tool_outputs
    chain = compress_tool_outputs() | model      # model = any LangChain chat model

LangGraph agent loop:

    from langgraph.prebuilt import create_react_agent
    from secondwind.langchain import compress_pre_model_hook
    agent = create_react_agent(model, tools, pre_model_hook=compress_pre_model_hook())

Both convert to the OpenAI shape, rewrite tool outputs through a session (resends stay byte-identical,
only aged blocks offload), and map compressed content back by tool_call_id. Any failure returns the
messages unchanged, so neither seam can break a chain or agent.
"""

from . import Session


def _rewrite_tool_messages(sess, model, messages):
    """New message list with only ToolMessage string content compressed; others are the original
    object. On failure returns the input list itself, so list identity signals whether anything changed."""
    from langchain_core.messages import ToolMessage, convert_to_openai_messages

    try:
        openai_messages = convert_to_openai_messages(messages)
        out = sess.rewrite({"model": model, "messages": openai_messages})
        rewritten = out.get("request", {}).get("messages", [])
    except Exception:  # never break the caller
        return messages

    by_id = {
        m.get("tool_call_id"): m.get("content")
        for m in rewritten
        if isinstance(m, dict) and m.get("role") == "tool"
    }
    result = []
    for message in messages:
        new_content = by_id.get(getattr(message, "tool_call_id", None))
        if (
            isinstance(message, ToolMessage)
            and isinstance(message.content, str)
            and isinstance(new_content, str)
            and new_content != message.content
        ):
            result.append(message.model_copy(update={"content": new_content}))
        else:
            result.append(message)
    return result


def compress_tool_outputs(session=None, model="gpt-4o", resolver=None):
    """LCEL runnable that compresses the tool messages passing through it, so the downstream model
    in `compress_tool_outputs() | model` reads the compressed form."""
    from langchain_core.runnables import RunnableLambda

    sess = session or Session(resolver=resolver)

    def _transform(value):
        messages = value.to_messages() if hasattr(value, "to_messages") else list(value)
        return _rewrite_tool_messages(sess, model, messages)

    return RunnableLambda(_transform)


def compress_pre_model_hook(session=None, model="gpt-4o", resolver=None):
    """LangGraph pre_model_hook: compress accumulated tool outputs before each model call.

    Tool results pile up in state and are re-sent every step. Returns them as `llm_input_messages` so
    the model reads the compressed form while durable `messages` history keeps the originals
    byte-for-byte. Returns no update when nothing compresses (or the rewrite fails)."""
    sess = session or Session(resolver=resolver)

    def _hook(state):
        messages = state["messages"] if isinstance(state, dict) else getattr(state, "messages", [])
        rewritten = _rewrite_tool_messages(sess, model, messages)
        if rewritten is messages or all(a is b for a, b in zip(rewritten, messages)):
            return {}
        return {"llm_input_messages": rewritten}

    return _hook
