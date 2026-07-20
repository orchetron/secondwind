#!/usr/bin/env python3
"""Relevance head-to-head on the same labeled corpus the Rust test uses.

    python3 score.py [corpus.jsonl]

Reports MRR, P@1, P@3, Recall@3 for a BM25 baseline and a MiniLM
(all-MiniLM-L6-v2) embedding ranker. The MiniLM head-to-head is already wired in
`embedding_rank` and runs automatically when sentence-transformers is installed;
when that library is missing the run prints BM25 only. The metric definitions match
crates/optimize/tests/relevance_bench.rs exactly, so a number here is comparable to
a number there.
"""
import json
import math
import re
import sys
from pathlib import Path

DEFAULT = Path(__file__).resolve().parents[2] / "crates/optimize/tests/relevance_corpus.jsonl"
TOKEN = re.compile(r"[a-z0-9]+")


def tokens(text):
    return [t for t in TOKEN.findall(text.lower()) if len(t) >= 3]


def bm25_rank(query, rows, k1=1.5, b=0.75):
    q = set(tokens(query))
    docs = [tokens(r) for r in rows]
    avg = (sum(len(d) for d in docs) / len(docs)) if docs else 1.0
    n = len(docs)
    scores = []
    for d in docs:
        s = 0.0
        for term in q:
            tf = d.count(term)
            if not tf:
                continue
            df = sum(1 for x in docs if term in x)
            idf = math.log((n - df + 0.5) / (df + 0.5) + 1.0)
            s += idf * tf * (k1 + 1) / (tf + k1 * (1 - b + b * len(d) / avg))
        scores.append(s)
    return scores


_MODEL = None


def embedding_rank(query, rows):
    """Contextual bi-encoder relevance, the approach a runtime embedding ranker
    uses. Returns None if sentence-transformers is not installed."""
    global _MODEL
    try:
        from sentence_transformers import SentenceTransformer, util
    except ImportError:
        return None
    if _MODEL is None:
        _MODEL = SentenceTransformer("all-MiniLM-L6-v2")
    q = _MODEL.encode(query, normalize_embeddings=True)
    r = _MODEL.encode(rows, normalize_embeddings=True)
    return list(util.cos_sim(q, r)[0].tolist())


def ranking(scores):
    return sorted(range(len(scores)), key=lambda i: (-scores[i], i))


def metrics(rank_fn, corpus):
    mrr = p1 = p3 = r3 = 0.0
    for t in corpus:
        order = ranking(rank_fn(t["query"], t["rows"]))
        rel = set(t["relevant"])
        rr = next((1.0 / (i + 1) for i, x in enumerate(order) if x in rel), 0.0)
        hit3 = sum(1 for x in order[:3] if x in rel)
        mrr += rr
        p1 += 1.0 if order[0] in rel else 0.0
        p3 += hit3 / 3.0
        r3 += hit3 / len(rel)
    n = len(corpus)
    return mrr / n, p1 / n, p3 / n, r3 / n


def main():
    path = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT
    corpus = [json.loads(l) for l in path.read_text().splitlines() if l.strip()]
    print(f"relevance benchmark over {len(corpus)} labeled queries ({path.name})")
    rankers = [("bm25", bm25_rank)]
    if embedding_rank(corpus[0]["query"], corpus[0]["rows"]) is not None:
        rankers.append(("embedding", embedding_rank))
    else:
        print("  (sentence-transformers not installed: BM25 only. Install it to run the MiniLM head-to-head.)")
    for name, fn in rankers:
        mrr, p1, p3, r3 = metrics(fn, corpus)
        print(f"  {name:10}  MRR {mrr:.3f}   P@1 {p1:.3f}   P@3 {p3:.3f}   R@3 {r3:.3f}")


if __name__ == "__main__":
    main()
