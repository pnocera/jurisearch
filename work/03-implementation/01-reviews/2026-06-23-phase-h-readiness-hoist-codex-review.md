# Phase H Readiness Hoist Review

No severity-tagged findings.

Review notes:

- `search_with_postgres` now has exactly two callers: `search_payload` and `france_legi_search_documents`. `search_payload` passes `verify_readiness=true`, preserving the one-shot CLI/session/eval-phase1 readiness behavior and the existing dense-vs-BM25 gate selection.
- `eval_france_legi_payload` performs `ensure_query_readiness(&postgres, QueryReadinessGate::Search)` immediately after `open_index`, before gold extraction and before any benchmark loop, so an incomplete index still fails fast.
- `france_legi_search_documents` constructs `CliSearchMode::Hybrid` / `RetrievalMode::Hybrid`, so the up-front `Search` gate is the correct gate: it covers projection and embedding coverage, whereas `SearchLexical` would only cover projection.
- The reviewed eval path only reads gold data and runs retrieval through the already-open `ManagedPostgres`; this change does not introduce an index mutation between the up-front readiness check and per-query searches.
- Dense embedding-runtime readiness remains inside `search_with_postgres` under `retrieval_mode.uses_dense()`, so skipping the coverage re-count does not skip the runtime/dependency check.

Validation reviewed:

- Inspected `git diff -- crates/jurisearch-cli/src/main.rs`.
- Confirmed with CodeGraph that `search_with_postgres` has only the two expected callers.
- Inspected the relevant current source for `eval_france_legi_payload`, `france_legi_search_documents`, `search_with_postgres`, `ensure_query_readiness`, `france_legi_gold_json`, and `hybrid_candidates_json`.
- Did not rerun `cargo test -p jurisearch-cli`; the provided instructions state that the full suite was already run for this change.

VERDICT: GO
