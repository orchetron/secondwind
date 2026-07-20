# Relevance benchmark

Measures how well a ranker orders tool-output rows by relevance to a request.
Two labeled corpora, identical metric definitions across the Rust test and the
Python scorer so numbers compare directly.

- `crates/optimize/tests/relevance_corpus.jsonl` (18 queries): relevant rows use a
  synonym of the query. Tests basic semantics over term overlap.
- `hard_corpus.jsonl` (40 queries): lexical traps that share query words but are
  irrelevant, polysemy (python the language vs the snake), negation and intent,
  long paragraph rows, and fine-grained near-duplicates. Separates strong models.

Run: `cargo test -p secondwind-optimize --test relevance_bench -- --nocapture`
and `python3 bench/relevance/score.py [corpus.jsonl]`.

## Measured

Only the BM25, MiniLM, and distilled MRR/P@1/P@3/R@3 figures are directly
reproducible from the two runs (the Rust bench emits distilled, score.py emits BM25
and MiniLM). Rows and columns marked `*` are one-off offline measurements, not
produced by score.py or the Rust bench.

Easy corpus (MRR):

| BM25 | distilled default | transformer (MiniLM) |
|---|---|---|
| 0.824 | 0.917 | 1.000 |

Hard corpus (MRR / P@1 / NDCG@5 / MAP):

| ranker | MRR | P@1 | NDCG@5* | MAP* |
|---|---|---|---|---|
| BM25 | 0.662 | 0.450 | 0.748 | 0.634 |
| distilled default (ours) | 0.746 | 0.575 | - | - |
| MiniLM dense | 0.850 | 0.725 | 0.881 | 0.823 |
| bge-small dense* | 0.892 | 0.800 | 0.909 | 0.860 |
| hybrid bm25+dense* | 0.779 | 0.600 | 0.820 | 0.727 |
| cross-encoder rerank* | 0.721 | 0.550 | 0.791 | 0.695 |

## Distilled static embeddings (shipped default)

The default ranker distills a transformer into a static sentence-embedding table
(the model2vec / potion approach). It keeps the model-free, deterministic, offline
properties but far outranks raw co-occurrence vectors on the hard corpus (MRR):

| GloVe static (prior default) | potion-8M* | potion-base-32M* | MiniLM (runtime) |
|---|---|---|---|
| 0.683 | 0.762 | 0.804 | 0.850 |

Pure dense beats fusing BM25 under it (0.737 vs 0.717 for potion), consistent with
the endpoint finding.

Provenance drives what we bundle. potion-base is distilled from bge (BAAI, China
origin), which regulated enterprises flag regardless of its MIT license. So the
bundled default is distilled by us from all-MiniLM-L6-v2 (Apache 2.0, western
origin), measuring MRR 0.746 with clean provenance we can publish and checksum, near
the potion-from-bge 0.762. Only the embedding table ships (safetensors, not a
pickle), so loading it cannot execute code. The endpoint mode ships no weights: the
enterprise points it at their own vetted model.

The shipped default is a self-distilled static table from all-MiniLM (~7.6MB int8,
29525 x 256) encoded pure, baked in as static_vocab.txt + static_vectors.bin and
wired as DistilledEmbedder (crates/optimize/src/lib.rs), read through a pure-Rust
wordpiece tokenizer.

## What it establishes

- The distilled default beats BM25 on both corpora, including hard relevance, where
  it now measures 0.746 against BM25's 0.662, though still below a dense
  transformer. It stays model-free, deterministic, and offline while now beating the
  lexical baseline.
- A dense transformer is far better on hard relevance, and a stronger one beats a
  weaker one: bge-small (0.892) over MiniLM (0.850). The switchable endpoint mode
  (`serve/run --embed`) reaches these numbers and beats the MiniLM baseline by
  pointing at a stronger model.
- Fusing the lexical signal under a strong dense model HURTS it (0.779 vs 0.850);
  the lexical traps that wreck BM25 leak in. So the endpoint embedder ranks pure
  dense (its `dominant()` is true) and discards the lexical base. This corrected an
  earlier "hybrid is better" assumption the measurement refuted.
