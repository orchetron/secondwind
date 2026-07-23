#!/usr/bin/env python3
"""Relevance check: a model answers exact value-lookups about each workload shown raw vs secondwind's
inline wire; the wire passes when it scores within one question of the raw control. Best of N passes
absorbs model variance. Needs ANTHROPIC_API_KEY in ../../.env.
Usage: python3 bench/accuracy/run.py [file.json ...] [--passes N] [--questions M]"""
import json, os, re, subprocess, sys, urllib.error, urllib.request, random

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
WL = os.path.join(REPO, "bench", "compression", "workloads")
EMIT = os.path.join(REPO, "target", "release", "examples", "emit_wire")
MODEL = "claude-sonnet-5"
DEFAULT = ["github_prs_flat.json", "github_prs.json", "github_issues.json", "lock_package.json"]
random.seed(0)


def load_env():
    p = os.path.join(REPO, ".env")
    if os.path.exists(p):
        for line in open(p):
            line = line.strip()
            if line and not line.startswith("#") and "=" in line:
                k, v = line.split("=", 1)
                os.environ.setdefault(k.strip(), v.strip())


def ask(prompt):
    body = json.dumps({"model": MODEL, "max_tokens": 2000,
                       "messages": [{"role": "user", "content": prompt}]}).encode()
    req = urllib.request.Request("https://api.anthropic.com/v1/messages", data=body,
        headers={"x-api-key": KEY, "anthropic-version": "2023-06-01", "content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=180) as r:
            d = json.load(r)
    except urllib.error.HTTPError as e:
        sys.exit(f"API error {e.code}: {e.read().decode()[:300]}")
    return "".join(b.get("text", "") for b in d.get("content", []) if b.get("type") == "text")


def emit(text):
    r = subprocess.run([EMIT], input=text, capture_output=True, text=True)
    return r.stdout if r.returncode == 0 and r.stdout.strip() else text


def flatten(rec, prefix=""):
    out = {}
    for k, v in rec.items():
        key = f"{prefix}{k}"
        if isinstance(v, dict):
            out.update(flatten(v, key + "."))
        elif isinstance(v, (str, int, float, bool)):
            out[key] = v
    return out


def dominant_map(data):
    best = None
    for k, v in ([("", data)] + list(data.items())):
        if isinstance(v, dict):
            objs = [x for x in v.values() if isinstance(x, dict)]
            if len(v) >= 4 and len(objs) >= 0.6 * len(v) and (best is None or len(v) > len(best[1])):
                best = (k, v)
    return best


def cap(items, budget, sizef):
    out, total = [], 0
    for it in items:
        out.append(it)
        total += len(sizef(it))
        if total >= budget and len(out) >= 2:
            break
    return out


# Returns (block_json, flattened_records, idkey, noun) capped to a byte budget so a huge workload
# still costs little to test; the wire is emitted from exactly the block the model is shown.
def build_block(data, budget):
    if isinstance(data, list):
        items = [x for x in data if isinstance(x, dict)]
        if len(items) < 2:
            return None
        capped = cap(items, budget, json.dumps)
        return json.dumps(capped), [flatten(x) for x in capped], None, "item"
    if isinstance(data, dict):
        found = dominant_map(data)
        if not found:
            return None
        k, mp = found
        pairs = [(kk, vv) for kk, vv in mp.items() if isinstance(vv, dict)]
        if len(pairs) < 2:
            return None
        capped = cap(pairs, budget, lambda kv: json.dumps(kv[1]))
        if k == "":
            obj = dict(capped)
        else:
            obj = {kk: vv for kk, vv in data.items() if not isinstance(vv, (dict, list)) or kk == k}
            obj[k] = dict(capped)
        recs = [dict(_key=kk, **flatten(vv)) for kk, vv in capped]
        return json.dumps(obj), recs, "_key", "entry"
    return None


def pick_id(recs):
    present = [k for k in recs[0] if all(k in r for r in recs)]
    scored = [(len({str(r[k]) for r in recs}), k) for k in present]
    return max(scored, default=(0, list(recs[0])[0]))[1]


def gen_questions(recs, idkey, noun, n):
    # Cycle records asking a different field each time, so few large records still fill the sample.
    qs, order, step = [], random.sample(range(len(recs)), len(recs)), 0
    while len(qs) < n and step < n * 4:
        rec = recs[order[step % len(order)]]
        step += 1
        idval = rec.get(idkey)
        fields = [f for f in rec if f != idkey and str(rec[f]).strip() and len(str(rec[f])) < 90]
        if idval is None or not fields:
            continue
        f = random.choice(fields)
        q = (f'For the {noun} with {idkey}={idval!r}, what is "{f}"?', str(rec[f]))
        if q not in qs:
            qs.append(q)
    return qs


def parse_answers(resp, n):
    m = re.search(r"\[.*\]", resp, re.S)
    if m:
        try:
            a = json.loads(m.group(0))
            if isinstance(a, list):
                return [str(x) for x in a]
        except Exception:
            pass
    return [""] * n


def norm(s):
    return re.sub(r"\s+", " ", str(s).strip().strip("\"'").lower())


def score(block, qs):
    ql = "\n".join(f"{i+1}. {q}" for i, (q, _) in enumerate(qs))
    prompt = (f"Here is a tool result:\n\n{block}\n\nAnswer each question using ONLY the exact value "
              f"from the tool result above. Return a JSON array of strings, one answer per question in "
              f"order, and nothing else.\n\n{ql}")
    ans = parse_answers(ask(prompt), len(qs))
    return sum(1 for i, (_, a) in enumerate(qs) if i < len(ans) and norm(ans[i]) == norm(a))


def codec_of(wire, raw):
    if wire == raw:
        return "verbatim"
    head = wire.split("\n", 1)[0]
    return next((c for c in ("SWCOL", "SWNEST", "SWDOC") if head.startswith(c)), head[:10])


def run_workload(fn, passes, nq, budget=45000):
    data = json.load(open(os.path.join(WL, fn)))
    built = build_block(data, budget)
    if not built:
        print(f"{fn:24} no record shape to test"); return None
    block, recs, idkey, noun = built
    if idkey is None:
        idkey = pick_id(recs)
    qs = gen_questions(recs, idkey, noun, nq)
    if not qs:
        print(f"{fn:24} no answerable questions"); return None
    wire = emit(block)
    codec = codec_of(wire, block)
    raw_best = max(score(block, qs) for _ in range(passes))
    wire_best = max(score(wire, qs) for _ in range(passes))
    ok = wire_best >= raw_best - 1
    print(f"{fn:24} codec={codec:8} raw {raw_best}/{len(qs)}  wire {wire_best}/{len(qs)}  {'PASS' if ok else 'FAIL'}")
    return ok


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    passes = int(next((a.split("=")[1] for a in sys.argv if a.startswith("--passes=")), 2))
    nq = int(next((a.split("=")[1] for a in sys.argv if a.startswith("--questions=")), 12))
    files = args or DEFAULT
    print(f"model={MODEL}  passes={passes}  questions={nq}\n")
    results = [run_workload(f, passes, nq) for f in files]
    done = [r for r in results if r is not None]
    print(f"\n{sum(done)}/{len(done)} codecs pass relevance (wire within 1 of raw control)")
    sys.exit(0 if all(done) else 1)


if __name__ == "__main__":
    load_env()
    KEY = os.environ.get("ANTHROPIC_API_KEY") or sys.exit("no ANTHROPIC_API_KEY in .env")
    main()
