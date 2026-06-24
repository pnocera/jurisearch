# Code Review: authority-aware ranking analysis r2

## Findings

No findings.

## Verification

The r2 analysis resolves the r1 blocker. Section 5 option (b) now distinguishes the three relevant pools:

- the BM25/dense arm limits (`top_k * 4` for chunk grouping, `top_k * 20` for document grouping and zone search),
- the final SQL output limit (`query_limit = top_k + 1`),
- and the returned `top_k` after Rust truncation.

That correction matches the live source. The main path computes arm limits at `crates/jurisearch-cli/src/main.rs:3691-3697`, passes `limit: query_limit` into `hybrid_candidates_json` at `main.rs:3711-3727`, and truncates the returned candidates at `main.rs:3777-3784`. Storage applies that limit in the final SQL page for chunk grouping at `crates/jurisearch-storage/src/retrieval.rs:317-319` and document grouping at `retrieval.rs:366-371`. The zone path has the same shape: arm limits and `query_limit = top_k + 1` at `main.rs:3422-3441`, final SQL limit at `crates/jurisearch-storage/src/zone_retrieval.rs:245-250`, and Rust truncation at `main.rs:3466-3474`.

The analysis also correctly states the consequence: a Rust rerank placed after SQL currently sees only the displayed page plus one pagination sentinel, not the larger arm pools. A useful post-SQL rerank must widen the final SQL output limit to a rerank window, rerank that materialized window, truncate to `top_k`, and rework the cursor because current paging keys on the SQL order. The cursor claim matches `retrieval.rs:535-567`, where chunk and document cursors are keyed on rounded `fused_score` / `cursor_score` plus the stable id tie-breaker. The same cursor predicate is reused by the zone path via `zone_retrieval.rs:222-250`.

The r2 analysis resolves the r1 warning. Section 4.3 no longer claims authority reranking cannot improve recall@10. It now says the Phase 2 gate observes only whether the single known-item gold crosses the top-10 boundary, can catch regressions, may show incidental recall gains, and is not a metric for authority-ordering quality. That is accurate for a recall@10 known-item gate: it cannot validate "most authoritative relevant result first," but a rerank can still move a gold document into or out of the first 10.

I did not find a new inaccuracy in the revised areas. The comparison table's option (b) row now preserves the same correction by making the default-off case restore the original `top_k + 1` limit and cursor behavior. The surrounding claims that matter to these fixes also match the source: candidate JSON currently includes `source` but not `publication` in the main path (`retrieval.rs:331-334`, `:384-388`) and zone path (`zone_retrieval.rs:264-269`); `DecisionFilters` are reused by zone retrieval (`zone_retrieval.rs:206-212`); and changing displayed order without changing cursor semantics would risk pagination skips or duplicates.

VERDICT: GO
