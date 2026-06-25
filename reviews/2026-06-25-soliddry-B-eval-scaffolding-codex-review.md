# Code Review: SOLID/DRY-B eval scaffolding

Commit reviewed: `51607d4` (`cli: factor benchmark qrel-scoring + search-request scaffolding (SOLID/DRY-B)`)

## Findings

No findings.

## Equivalence Notes

- `score_known_item_qrels` preserves the scored-qrel set for the three original loop shapes. Missing `gold_document_id` is skipped before resolver execution; missing `query` in France-juris/zone and missing `query` or `as_of` in France-LEGI return `Ok(None)` from the resolver and are skipped without incrementing `queries`, matching the parent loops' `continue` behavior.
- Hit windows are preserved: France-juris retrieval, zone retrieval, and France-LEGI known/temporal score with `docs.iter().take(top_k).any(...)`; France-juris citation still uses `docs.iter().any(...)` with no `take`.
- France-juris retrieval still resolves with `overfetch` and scores with `top_k`. The refactor did not swap these values.
- Backend accounting remains France-LEGI-only. The resolver returns `Some(backend)` only for France-LEGI known/temporal, and the helper increments the `BTreeMap` after successful resolve and before hit scoring, matching the old placement before `*_done += 1`. `json!(known_score.backends)` and `json!(temporal_score.backends)` preserve the previous ordered object shape expected by `phase1_france_legi_artifact_errors`.
- Resolver errors still propagate through `?`, preserving the previous abort behavior for search/citation storage errors and dependency-unavailable guard errors.
- `benchmark_search_request` is field-equivalent to the removed literals: Hybrid mode, Concise format, supplied `kind`/`group_by`/`as_of`/`top_k`, and `None` for cursor, weights, probes, decision filters, zone, and index_dir. The France-juris call supplies Decision/Document/None; the France-LEGI call supplies Code/Chunk/Some(as_of).
- The France-LEGI `score_legi_category` closure uses the same resolver and scoring window for known-item and temporal qrels, and it does not alter the later cross-reference loop or `index_dir` use.
- `cross_reference`, the artifact builders, `FranceLegiCategoryResult`, `FranceJurisCategoryResult`, and gate thresholds are unchanged by the commit. The Phase 1 gate still requires `routing_backends` to account for every query and all gating queries to be served by `structured_citation`.

## Test Notes

- The new `score_known_item_qrels` unit tests cover hit/miss scoring, skipped qrels, backend accumulation, caller-controlled hit window, empty input, and resolver error propagation.
- I did not run the live benchmark runners; per the review brief they require a built index plus live embeddings and are not covered by integration tests. This review is based on source comparison against `51607d4~1` and focused static verification.

VERDICT: GO
