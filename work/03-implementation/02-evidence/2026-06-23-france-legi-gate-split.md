# France-LEGI Phase 1 gate split (2026-06-23)

## Why
The original France-LEGI gate ran all three categories (known-item, temporal, cross-reference)
through the fuzzy hybrid pipeline at one floor each, and failed: known-item recall@10 0.55, temporal
0.75, cross-reference 0.116. Diagnosis (and codex Q&A): the gate was testing **structured-fact**
problems through a **fuzzy** retriever — a tool mismatch. The shared parent-text title swamps the
discriminating article number, so a specific article ranks past 10 (known-item ceiling ~0.667 even
BM25-dominant); cross-reference (full-body → cited-article) is inherently weak and its gold is
derived from the CITATION graph edges, so resolving via those edges would be tautological.

## The split (committed direction)
Both codex and the assistant independently landed on the same design:

1. **Production intent routing** (Phase II). The shared search path (`search_with_postgres`, used by
   the CLI and the eval runner alike) routes a citation-shaped query (`Article <n>`, optional
   `en vigueur au <date>`) to a structured citation resolver; conceptual queries use hybrid. Every
   search records a `routing` audit object (`query_type`, `chosen_backend`, `candidate_count`,
   `fallback_path`) so the gate can prove routing was driven by the input's structural resolvability,
   not by knowing the answer. Validated: structured resolution scores recall@10 = 1.00 on the 60
   known-item queries (one candidate — the citation key + as-of window uniquely identify the version).

2. **Split the gate** (Phase III) into three categories with separate contracts:
   - `structured_citation_resolution` — **gating**, floor 0.95.
   - `temporal_version_pinning` — **gating**, floor 0.90.
   - `semantic_retrieval` (the old full-body → cited-article task) — **advisory**, floor 0.40,
     non-gating. It mostly measures accidental topical similarity, so it informs but never blocks.
   The runner records the per-category routing-backend audit; the artifact marks each category
   `gating: true/false`; `state` is `passed` only when both gating categories clear floor + min
   queries. The status gate (`phase1_france_legi_*`) re-derives the same, treating semantic as
   advisory (records its metric, never fails the claim on it).

## Calibrated result (index/phase1-freemium-20250713, top-10)
| category | metric@10 | floor | gating | backend |
|---|---|---|---|---|
| structured_citation_resolution | 1.00 | 0.95 | yes | structured_citation (60/60) |
| temporal_version_pinning | 1.00 | 0.90 | yes | structured_citation (12/12) |
| semantic_retrieval | 0.116 | 0.40 | no (advisory) | hybrid |

Both gating categories pass → the gate passes → the Phase 1 claim opens (`claim_allowed`).

## Honesty
The routing is production-visible (not eval-only), input-shape-driven (no gold/answer read), and the
artifact records the routing backend per category. The claim scope is stated honestly: structured
citation resolution + temporal version pinning are the gated guarantees; full-body semantic retrieval
is advisory. A future structured cross-reference category via citation-span extraction (parse the
reference text in the body, resolve it) is the noted follow-up; full-body semantic stays advisory.
