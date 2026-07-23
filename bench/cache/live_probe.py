# Live provider check of the mechanism the cache guard rests on: a byte-identical prefix resend reads
# from cache, while changing a block inside the cached prefix busts it (cache_read drops, creation jumps).
# Needs ANTHROPIC_API_KEY in ../../.env. Run: python3 bench/cache/live_probe.py
import json
import os
import sys
import urllib.request

for line in open(os.path.join(os.path.dirname(__file__), "../../.env"), encoding="utf-8"):
    if "=" in line and not line.strip().startswith("#"):
        k, v = line.strip().split("=", 1)
        os.environ.setdefault(k, v)

KEY = os.environ.get("ANTHROPIC_API_KEY") or sys.exit("no ANTHROPIC_API_KEY in .env")
MODEL = "claude-sonnet-4-5"

# A stable system prefix well over the 1024-token minimum, so it caches on its own breakpoint and stays
# read even when a later block changes (a realistic agent's system+tools prefix, not a rigged tiny one).
PREAMBLE = "You are a precise coding assistant. Follow the repository conventions exactly. " * 110
BLOCK_VERBATIM = "".join(f"row {i}: service-{i} on port {7000 + i} status ok detail line\n" for i in range(120))
BLOCK_CHANGED = "[120 rows offloaded, call resolve for the full table]"  # what a rewrite would substitute


def call(block, question):
    # cache_control on the first user block caches preamble+block; the question is the varying suffix.
    body = {
        "model": MODEL,
        "max_tokens": 16,
        "system": [{"type": "text", "text": PREAMBLE, "cache_control": {"type": "ephemeral"}}],
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": block, "cache_control": {"type": "ephemeral"}}]},
            {"role": "assistant", "content": "Noted."},
            {"role": "user", "content": question},
        ],
    }
    req = urllib.request.Request(
        "https://api.anthropic.com/v1/messages",
        data=json.dumps(body).encode(),
        headers={"x-api-key": KEY, "anthropic-version": "2023-06-01", "content-type": "application/json"},
    )
    u = json.load(urllib.request.urlopen(req))["usage"]
    return u.get("cache_creation_input_tokens", 0), u.get("cache_read_input_tokens", 0)


def show(label, cw, cr):
    print(f"{label:38} cache_creation={cw:>6}  cache_read={cr:>6}")


print(f"model {MODEL}\n")
# 1) prime the cache with the verbatim prefix.
show("turn 1 (prime, verbatim block)", *call(BLOCK_VERBATIM, "Say ok."))
# 2) byte-identical prefix, new question => should READ the cached prefix (the guard's steady state).
show("turn 2 (identical block, new q)", *call(BLOCK_VERBATIM, "Say ok now."))
# 3) block inside the prefix changed => cache diverges at the block, read collapses (the maturation bug).
show("turn 3 (block CHANGED, new q)", *call(BLOCK_CHANGED, "Say ok please."))
print("\nExpected: turn 2 reads the full cached prefix; turn 3 keeps the stable system prefix but the")
print("read drops sharply from the changed block onward (a mid-prefix change busts cache from there).")
