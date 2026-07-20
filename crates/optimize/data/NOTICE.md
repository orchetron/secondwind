# Third-party attribution: bundled relevance embeddings

`static_vectors.bin` and `static_vocab.txt` are a static embedding table distilled
from a pretrained sentence embedding model. They are a derivative work, bundled and
redistributed under the terms below.

- Source model: `sentence-transformers/all-MiniLM-L6-v2`
  - License: Apache License 2.0
  - Copyright the sentence-transformers authors (UKP Lab)
  - Base architecture `microsoft/MiniLM`, MIT License
- Distillation tool: `model2vec` (MinishLab), MIT License
- Modification: the model's per-token output vectors were captured into a static
  lookup table, PCA-reduced to 256 dimensions, and int8-quantized by this project.
  No original model weights are redistributed; only the derived static table.

Apache License 2.0 permits this commercial use and redistribution of a derivative
with attribution. The full Apache 2.0 text applies to this derived table:
https://www.apache.org/licenses/LICENSE-2.0

The optional `--embed` endpoint backend bundles no model weights; it calls an
embeddings endpoint the operator supplies and vets.
