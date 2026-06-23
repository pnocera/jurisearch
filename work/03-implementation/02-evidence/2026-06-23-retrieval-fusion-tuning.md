# Retrieval fusion tuning — France-LEGI gate (2026-06-23)

## Why
The first completing France-LEGI calibration (after the gold-extraction speedup) failed all three
categories. Per-mode diagnosis of an exact-citation known-item query (gold = "Article 33" of a
multi-article decree) showed the dense arm actively hurting:

| mode | gold rank |
|---|---|
| bm25 | 4 |
| dense | not in top 10 |
| hybrid (equal RRF) | 7 |

The dense embedding of a long, mostly-shared citation ("Décret … RELATIF AU DIPLOME D'EXPERTISE
COMPTABLE Article 33") is near-identical across sibling articles, so equal-weight RRF dilutes the
much sharper BM25 ranking. Codex (Q&A) confirmed equal RRF is wrong for LEGI's near-duplicate
sibling articles and recommended weighted RRF / dense-as-recall-expander.

## Change
`crates/jurisearch-storage/src/retrieval.rs`: hybrid `fused_score` is now
`W_lex/(k+lexrank) + W_dense/(k+denserank)` with `k=60`, weights from
`JURISEARCH_RRF_LEXICAL_WEIGHT` / `JURISEARCH_RRF_DENSE_WEIGHT` (default `1.0` / `0.3`). The artifact
provenance records the weights used.

## Calibration sweep (production index `phase1-freemium-20250713`, top-10, 60/12/120 qrels)

| category | base 1.0/1.0 | **dense=0.3 (new default)** | dense=0.15 | threshold |
|---|---|---|---|---|
| known_item recall@10 | 0.55 | **0.60** | 0.667 | 0.85 |
| temporal exactness@10 | 0.75 | **0.75** | 0.667 | 0.90 |
| cross_reference recall@10 | 0.143 | 0.116 | 0.11 | 0.60 |

`dense=0.3` is the chosen default: it lifts known-item with **no temporal regression**; the
cross-reference change is immaterial (that category fails by 4×). `dense=0.15` trades temporal away.

## Conclusion (important)
**No global fusion weight passes the gate.** Best achievable per category (known-item 0.667,
temporal 0.75, cross-reference 0.143) all remain below threshold:

- **known-item / temporal** share a root cause — the shared parent-text title dominates ranking, so
  the specific article among its siblings ranks past 10 in *both* arms. The ~0.667 known-item ceiling
  is set by query/ranking quality, not fusion weight. Fixes: query construction (lead with the
  discriminating article number, not the long shared title), or a cross-encoder reranker over the
  fused top-N.
- **cross-reference** at 0.143 is full-body → cited-article retrieval, inherently hard. Codex
  recommends redesigning the gate task to query from extracted citation spans / outbound reference
  text, separating "reference extraction" from "retrieve cited article".

Fusion tuning is a real, measured production improvement (and keeps the gate honest — production and
eval use the same default), but closing the gate needs query construction + reranking + a
cross-reference task redesign. The Phase 1 claim correctly stays gated (fail-closed).
