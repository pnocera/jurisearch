# Code Review: SOLID/DRY-C search_with_postgres decomposition

## Findings

No BLOCKER/WARN/NIT findings.

The commit appears to be a mechanical extraction of the previous `search_with_postgres` body into `SearchExecution` plus `RoutedSearch`, with the behavior-preserving details from the review brief intact:

- Readiness gating still happens before context construction and still chooses `Search` only when `retrieval_mode.uses_dense()` is true, otherwise `SearchLexical`.
- `SearchExecution::new` preserves the old request-derived locals: `as_of`, `kind_filter`, `group_by`, `pool_multiplier`, `lexical_limit`, `dense_limit`, and `query_limit`.
- `run_hybrid_candidates` preserves the old `run_hybrid` closure inputs field-for-field, including reused embedder behavior for eval callers, `PreparedQueryEmbedder::from_env()` only when dense retrieval is used, `after_cursor.map(ParsedSearchCursor::as_retrieval_cursor)`, the same retrieval options/decision filters, and the same storage/JSON parse error mapping.
- `run_structured_citation_or_fallback` preserves the old citation-routing match: `query_type` is computed from citation intent before the retrieval-mode arm, structured resolution runs before any fallback for Hybrid citation-shaped queries, structured misses fall back to hybrid, and structured storage/parse errors still abort instead of being swallowed.
- `apply_search_response_envelope` preserves the JSON contract ordering: `routed_candidate_count` is read before format/limit/expansion decoration and before truncation; `returned` is computed after truncation; cursor support still depends on `chosen_backend != "structured_citation"`; pagination, routing, Detailed diagnostics, and no-results mapping remain in the old order.
- The Detailed diagnostics block is behavior-equivalent: `query_input`, lexical query text gating, retrieval mode flags, limits, kind filter, and request cursor are all still sourced from the same effective values.
- The borrowed `SearchExecution<'a>` fields are sound for the existing one-shot and eval call shapes: the context does not outlive the borrowed request, Postgres handle, query text, optional cursor, or optional embedder, and all methods only borrow `&self` during the synchronous search call.

## Validation

Reviewed:

- `git show 8a6926f -- crates/jurisearch-cli/src/retrieval/search.rs`
- Previous implementation via `git show 8a6926f~1:crates/jurisearch-cli/src/retrieval/search.rs`
- Current `search_with_postgres`, `SearchExecution::new`, `run_hybrid_candidates`, `run_structured_citation_or_fallback`, and `apply_search_response_envelope`
- Eval call sites using `verify_readiness: false` and `Some(embedder)` in `france_legi_search_documents` and `france_juris_search_documents`

I did not rerun `cargo test -p jurisearch-cli` or the ignored live-embeddings test; the review focused on the requested byte-for-byte/ordering equivalence of the extraction.

VERDICT: GO
