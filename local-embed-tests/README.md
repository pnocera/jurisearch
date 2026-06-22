# local-embed-tests — bge-m3 vs French specialists

A small, self-contained French-legal retrieval benchmark to check whether **bge-m3**
leaves ranking quality on the table versus a French specialist (CamemBERT, Solon, …)
**before locking it as the v1 embedding model** (see `work/03-implementation/.../1.7`).

No full corpus needed: the comparison is *relative* (every model sees the same gold),
so a curated set of ~36 passages + ~22 queries with hard negatives is enough to detect
a real difference.

## Files
- `corpus.jsonl`   — French legal passages (faithful paraphrases of well-known provisions), with built-in hard negatives (e.g. produits défectueux vs vices cachés vs fait des choses).
- `queries.jsonl`  — natural-language legal questions, each with `relevant_ids` (gold).
- `endpoints.json` — the models to compare (OpenAI-compatible `/v1/embeddings`).
- `eval.py`        — pure-stdlib harness: embeds corpus+queries through each endpoint, ranks by cosine, reports MRR@10 / Recall@1,5,10 / nDCG + a head-to-head sign test.
- `serve_st.py`    — minimal OpenAI-compatible server for any sentence-transformers model.

## How to run

**1. bge-m3** — your existing llama.cpp server already works (`:8097`, `--pooling cls`).

**2. French specialist** — served via sentence-transformers (no reliable GGUF exists, and a
mis-converted GGUF would use the wrong pooling → unfair). `serve_st.py` declares its deps
inline (PEP 723), so **uv** builds the env automatically:
```bash
uv run serve_st.py --model Lajavaness/sentence-camembert-large --port 8098
# optional extra comparator:
# uv run serve_st.py --model OrdalieTech/Solon-embeddings-large-0.1 --port 8099
```
Or with a persistent venv:
```bash
uv venv && uv pip install sentence-transformers
uv run python serve_st.py --model Lajavaness/sentence-camembert-large --port 8098
```
(CPU is fine — only ~59 short texts are encoded. For GPU torch, install the CUDA build into the venv.)

**3. run the eval** (pure stdlib, no deps):
```bash
uv run eval.py        # or: python3 eval.py
```
Unreachable endpoints are skipped, so you can run it with only bge-m3 up to get a baseline,
then start CamemBERT and re-run for the head-to-head.

## Reading the result
- Same MRR/nDCG within noise + sign-test `p > 0.10` ⇒ **bge-m3 is as good as the specialist on French legal retrieval** → locking bge-m3 is well-supported.
- Specialist clearly higher ⇒ reconsider before locking (or keep the specialist as a candidate).

## Fairness notes
- Each model is served with **its own correct pooling** (bge-m3 = CLS via llama.cpp; sentence-transformers models apply their configured pooling). Don't force CamemBERT through llama.cpp with `--pooling cls` — that would be a rigged test.
- For a serving-stack-controlled run you can also serve bge-m3 via `serve_st.py --model BAAI/bge-m3 --port 8197` and compare ST-vs-ST.
- This is a **directional** check (small N), not a release-gating eval. It answers "is bge-m3 obviously worse for French?" — which is exactly the lock-decision question.
