#!/usr/bin/env python3
"""Multi-model relevance validation: for every codec, a set of models answers exact value-lookups
about a workload shown raw vs secondwind's inline wire; the wire passes when it scores within one of
the raw control. Covers all inline codecs and Anthropic model tiers. Needs ANTHROPIC_API_KEY in ../../.env.
Usage: python3 bench/accuracy/validate.py"""
import json, os, re, subprocess, sys, urllib.error, urllib.request, random
from collections import Counter

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
WL = os.path.join(REPO, "bench", "compression", "workloads")
EMIT = os.path.join(REPO, "target", "release", "examples", "emit_wire")
MODELS = ["claude-haiku-4-5-20251001", "claude-sonnet-5", "claude-opus-4-8"]
random.seed(0)


def load_env():
    for line in open(os.path.join(REPO, ".env")):
        line = line.strip()
        if line and not line.startswith("#") and "=" in line:
            k, v = line.split("=", 1)
            os.environ.setdefault(k.strip(), v.strip())


def ask(model, prompt):
    body = json.dumps({"model": model, "max_tokens": 2000,
                       "messages": [{"role": "user", "content": prompt}]}).encode()
    req = urllib.request.Request("https://api.anthropic.com/v1/messages", data=body,
        headers={"x-api-key": KEY, "anthropic-version": "2023-06-01", "content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=180) as r:
            d = json.load(r)
    except urllib.error.HTTPError as e:
        return f"__ERR__ {e.code}"
    return "".join(b.get("text", "") for b in d.get("content", []) if b.get("type") == "text")


def emit(text):
    r = subprocess.run([EMIT], input=text, capture_output=True, text=True)
    return r.stdout if r.returncode == 0 and r.stdout.strip() else text


def parse(resp, n):
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


def flatten(rec, prefix=""):
    out = {}
    for k, v in rec.items():
        key = f"{prefix}{k}"
        if isinstance(v, dict):
            out.update(flatten(v, key + "."))
        elif isinstance(v, (str, int, float, bool)):
            out[key] = v
    return out


# --- per-shape (block, questions) builders; each returns (block_text, [(q, answer)]) ---
def cap_records(items, budget):
    out, total = [], 0
    for it in items:
        out.append(it)
        total += len(json.dumps(it))
        if total >= budget and len(out) >= 2:
            break
    return out


def build_records(fn):
    d = cap_records(json.load(open(os.path.join(WL, fn))), 45000)
    recs = [flatten(x) for x in d]
    idk = max(((len({str(r.get(k)) for r in recs}), k) for k in recs[0]), default=(0, "id"))[1]
    qs = qa_lookup(recs, idk, "item", 10)
    return json.dumps(d), qs


def build_map(fn):
    data = json.load(open(os.path.join(WL, fn)))
    mp = next((v for v in data.values() if isinstance(v, dict) and len(v) >= 4
               and sum(isinstance(x, dict) for x in v.values()) >= 0.6 * len(v)), None)
    pairs = list(mp.items())[:40]
    recs = [dict(_key=k, **flatten(v)) for k, v in pairs if isinstance(v, dict)]
    obj = {k: v for k, v in data.items() if not isinstance(v, (dict, list))}
    key = next(k for k, v in data.items() if v is mp)
    obj[key] = dict(pairs)
    return json.dumps(obj), qa_lookup(recs, "_key", "entry", 10)


def build_pypi():
    d = json.load(open(os.path.join(WL, "api_pypi_requests.json")))
    rel = {k: v for k, v in list(d["releases"].items())[:25] if v}
    block = {"info": {k: d["info"][k] for k in ("name", "version", "summary") if k in d["info"]}, "releases": rel}
    qs = []
    for ver in random.sample(list(rel), 8):
        f = next((x for x in ("filename", "size", "packagetype") if x in rel[ver][0]), None)
        if f:
            qs.append((f'For release version "{ver}", what is the "{f}" of its file?', str(rel[ver][0][f])))
    return json.dumps(block), qs


def qa_lookup(recs, idk, noun, n):
    qs, order, step = [], random.sample(range(len(recs)), len(recs)), 0
    while len(qs) < n and step < n * 4:
        r = recs[order[step % len(order)]]
        step += 1
        idv = r.get(idk)
        flds = [f for f in r if f != idk and str(r[f]).strip() and len(str(r[f])) < 90]
        if idv is None or not flds:
            continue
        f = random.choice(flds)
        q = (f'For the {noun} with {idk}={idv!r}, what is "{f}"?', str(r[f]))
        if q not in qs:
            qs.append(q)
    return qs


def build_grep(fn):
    raw = open(os.path.join(WL, fn)).read()
    lines = [l for l in raw.split("\n") if l.strip()]
    qs = []
    for i in random.sample(range(len(lines)), 8):
        p, ln, _ = lines[i].split(":", 2)
        qs.append((f'Reproduce the EXACT full line for file "{p}" at line {ln} (path:line:content).', lines[i]))
    return raw, qs


def build_listing(fn):
    raw = open(os.path.join(WL, fn)).read()
    lines = [l for l in raw.split("\n") if l.strip()]
    cnt = Counter(l.rsplit("/", 1)[-1] for l in lines)
    uniq = [l for l in lines if cnt[l.rsplit("/", 1)[-1]] == 1]
    qs = [(f'Reproduce the EXACT full path whose filename is "{l.rsplit("/",1)[-1]}".', l)
          for l in random.sample(uniq, min(8, len(uniq)))]
    return raw, qs


def build_tree(fn):
    raw = open(os.path.join(WL, fn)).read()
    lines = [l for l in raw.split("\n") if l.strip()]

    def depth(l):
        s, b = l, 0
        while s.startswith("│   ") or s.startswith("    "):
            s, b = s[4:], b + 1
        for c in ("├── ", "└── "):
            if s.startswith(c):
                return b + 1, s[4:].strip()
        return (0, s.strip()) if b == 0 else (b, s.strip())

    nodes = [depth(l) for l in lines]
    cnt = Counter(c for _, c in nodes)
    qs = []
    for i in random.sample(range(1, len(lines)), len(lines) - 1):
        d, c = nodes[i]
        if "[" in c or cnt[c] != 1:
            continue
        parent = next((nodes[j][1] for j in range(i - 1, -1, -1)
                       if nodes[j][0] == d - 1 and "[" not in nodes[j][1]), None)
        if parent and cnt[parent] == 1:
            qs.append((f'In this dependency tree, which package is "{c}" directly nested under (its parent)?', parent))
        if len(qs) >= 8:
            break
    return raw, qs


def build_lock(fn):
    raw = open(os.path.join(WL, fn)).read()
    pkgs = []
    for blk in raw.split("[[package]]\n")[1:]:
        d = {}
        for l in blk.split("\n"):
            if l.startswith(("name = ", "version = ", "checksum = ")):
                k, v = l.split(" = ", 1)
                d[k] = v.strip('"')
            elif not l.strip():
                break
        if "name" in d:
            pkgs.append(d)
    qs = []
    for p in random.sample([p for p in pkgs if "checksum" in p], 8):
        f = random.choice(["version", "checksum"])
        qs.append((f'For the package named "{p["name"]}", what is its "{f}"?', p[f]))
    return raw, qs


WORKLOADS = [
    ("github_prs_flat.json", "columnar", lambda: build_records("github_prs_flat.json")),
    ("github_prs.json", "nested", lambda: build_records("github_prs.json")),
    ("lock_package.json", "doc", lambda: build_map("lock_package.json")),
    ("api_pypi_requests.json", "norm", build_pypi),
    ("code_search_grep.txt", "grouped", lambda: build_grep("code_search_grep.txt")),
    ("find_rs.txt", "grouped", lambda: build_listing("find_rs.txt")),
    ("dep_cargo_tree.txt", "tree", lambda: build_tree("dep_cargo_tree.txt")),
    ("lock_cargo.toml", "lock", lambda: build_lock("lock_cargo.toml")),
]


def score(model, block, qs):
    ql = "\n".join(f"{i+1}. {q}" for i, (q, _) in enumerate(qs))
    prompt = (f"Here is a tool result:\n\n{block}\n\nAnswer each using ONLY the exact value from the "
              f"tool result. Return a JSON array of strings, one per question in order, nothing else.\n\n{ql}")
    ans = parse(ask(model, prompt), len(qs))
    return sum(1 for i, (_, a) in enumerate(qs) if i < len(ans) and norm(ans[i]) == norm(a))


SHORT = {"claude-haiku-4-5-20251001": "haiku-4.5", "claude-sonnet-5": "sonnet-5", "claude-opus-4-8": "opus-4.8"}


def main():
    print(f"columns show wire/raw per model (P = wire within 1 of raw control)\n")
    print(f"{'workload':22} {'codec':9} " + " ".join(f"{SHORT.get(m, m):>13}" for m in MODELS))
    passes = total = 0
    for fn, codec, builder in WORKLOADS:
        block, qs = builder()
        wire = emit(block)
        row = f"{fn:22} {codec:9} "
        for model in MODELS:
            r, w = score(model, block, qs), score(model, wire, qs)
            ok = w >= r - 1
            passes += ok
            total += 1
            cell = f"{w}/{r} {'P' if ok else 'F'}"
            row += f"{cell:>13} "
        print(row)
    print(f"\n{passes}/{total} (model x workload) cells pass")


if __name__ == "__main__":
    load_env()
    KEY = os.environ.get("ANTHROPIC_API_KEY") or sys.exit("no key")
    main()
