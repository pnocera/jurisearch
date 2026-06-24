# Phase 2 ranking: RRF tuning result + authority-ranking decision (2026-06-24)

Index: `/mnt/models/jurisearch-index/phase2-full-juridic` (unified statutes + jurisprudence).

## RRF dense-weight tuning (administrative retrieval — the weak category)

`eval france-juris` (50 administrative known-item qrels per point) swept `JURISEARCH_RRF_DENSE_WEIGHT`:

| dense_weight | admin recall@10 |
|---|---|
| 0.3 (default) | 0.74 |
| 0.6 | **0.76** (peak) |
| 1.0 | 0.68 |
| 1.5 | 0.68 |

(Judicial recall@10 is already 1.0; citation accuracy is RRF-independent.)

**Decision: keep the default `DEFAULT_RRF_DENSE_WEIGHT = 0.3`.** The peak (0.76 @ 0.6) is a +0.02 / +1-decision change on 50 qrels — within noise — and recall *degrades* above 0.6. The 0.3 default was deliberately calibrated for LEGI statute known-item recall (0.55→0.60); on the unified index a global increase trades statute recall for a noise-level administrative gain. Per-request `--rrf-dense-weight` remains available for callers who want to bias toward dense. Net: the statute-calibrated default is near-optimal for the combined corpus; no code change warranted.

## Authority-aware ranking — DEFERRED (codex recommendation C)

Considered building a tunable publication-authority boost (Publié au bulletin / au recueil > Inédit) into hybrid ranking. **Decided NOT to build it now.** Codex's reasoning (qa/20260624-111150…): authority weighting is a presentation preference, not a relevance model — it only reorders candidates RRF already found, is not a Phase 2 gate concern, and a defensible non-zero production default cannot be justified from the available graph-edge gold (the labels would be partly defined by the same authority feature being tuned). Even the safe default-0.0 slice would add a cursor-format version bump + pagination tests + RetrievalOptions/CLI/tune plumbing to the core `hybrid_candidates_json` search path used by everything — too much maintenance surface for an unproven, non-gating feature. RRF tuning is the more direct lever for ranking quality.

**Revisit trigger:** user-facing review shows authoritative decisions consistently buried under similarly-relevant unpublished ones; or a legal-domain reviewer wants authority ordering as product behavior; or the retrieval SQL is being revised anyway. If revisited, build codex's "option A" shape (hook behind default 0.0 + advisory graph/judged-pool eval, tuned only on staging, non-zero default only behind a human-reviewed artifact).
