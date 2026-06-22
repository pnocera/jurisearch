# Evidence — bge-m3 vs French-specialist embeddings (local validation)

Date: 2026-06-22
Purpose: decide whether to **lock `bge-m3`** as the v1 embedding model, or run the full French-specialist bake-off at plan task 1.7.
Outcome: lock `bge-m3` — recorded as **`DECISIONS.md` D21**; plan updated (W5 / 0.4 / 1.7 / §8 / risk register).
Harness: `local-embed-tests/` (`corpus.jsonl`, `queries.jsonl`, `eval.py`, `serve_st.py`, `endpoints.json`).

## Method
- **Curated French-legal mini-benchmark** — 37 passages (faithful paraphrases of well-known provisions, *with hard negatives*: produits défectueux vs vices cachés vs fait des choses; erreur vs dol; REP vs référé-suspension; etc.) and 22 natural-language queries with gold labels. Full corpus deliberately not needed: the comparison is *relative* (all models see the same gold).
- **Fair serving — each model with ITS correct pooling/normalization:**
  - `bge-m3` — llama.cpp `:8097`, `pooling=cls`, 1024-d, normalized.
  - `Lajavaness/sentence-camembert-large` — sentence-transformers `:8098`, model-default (mean) pooling.
  - `OrdalieTech/Solon-embeddings-large-0.1` — sentence-transformers `:8099`, with its required **`"query : "`** query prefix applied (without it Solon is handicapped).
- **Metric:** cosine retrieval over the shared corpus → MRR@10, Recall@1/5/10, nDCG@10, plus per-query head-to-head with an exact sign test.

## Results
| model | MRR@10 | R@1 | R@5 | R@10 | nDCG@10 | vs bge-m3 (per-query) |
|---|---|---|---|---|---|---|
| **bge-m3** | 0.932 | 0.864 | 1.000 | 1.000 | 0.950 | — |
| **CamemBERT** | 0.932 | 0.909 | 0.955 | 1.000 | 0.948 | 2–3, 17 ties → **p=1.000** |
| **Solon** (+`query :`) | 0.932 | 0.864 | 1.000 | 1.000 | 0.950 | 2–2, 18 ties → **p=1.000** |

(CamemBERT and Solon measured in separate runs; bge-m3 identical in both.)

## Findings
- **Identical MRR@10 (0.932)** across all three; nDCG within 0.002. bge-m3 is **statistically indistinguishable** from both French specialists (sign test p=1.000 in both head-to-heads).
- All disagreements are single-rank wobbles (gold at 1 vs 2), **except** bge-m3's clean win on employer liability (`commettant/préposé`): rank 1 vs CamemBERT 6 / Solon 2.
- Solon was served fairly (prefix applied); it still only tied.

## Caveats
- N=22, synthetic but source-faithful → a **directional** "no detectable difference," not a release-gating proof. "No detectable difference" *is* the lock question, and all three agree.
- CamemBERT is STS/similarity-tuned; **Solon is retrieval-tuned** (the tougher comparator) — both tie bge-m3.

## Conclusion
Locking `bge-m3` loses nothing versus the two strongest French specialists on French-legal retrieval, and bge-m3 additionally keeps multilingual reach + the dense/sparse/ColBERT upgrade path. → **Lock bge-m3 (D21); skip the 1.7 French-specialist bake-off; retain the re-embed/migration capability for any future model change.**

## Reproduce
Serve the three endpoints (`local-embed-tests/README.md`), then:
```bash
cd local-embed-tests && uv run eval.py     # writes results.json
```
