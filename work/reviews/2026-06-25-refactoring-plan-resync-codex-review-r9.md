# BLOCKER

None. The r8 blocker is resolved: `legifrance_code_search_body` / `sanitize_legifrance_query` / `LEGIFRANCE_QUERY_MAX_CHARS` are assigned to a shared `legifrance_search.rs` leaf, not to `enrichment/legislation.rs`, and source shows the body builder is used by both `enrich_legislation_citations_payload` (`crates/jurisearch-cli/src/main.rs:4776`) and the retrieval `cite --online` path through `apply_online_citation_confirmation` (`crates/jurisearch-cli/src/main.rs:11716`).

I also found no remaining explicit move-list assignment where a helper/type is assigned to one command module while source shows direct use by two or more proposed command modules in contradiction with the new shared-leaf principle.

# WARN

None. The r8 warnings are resolved:

- `today_utc` and `unix_seconds` are now included in the `date.rs` leaf. Source confirms `today_utc` is used across eval and retrieval paths (`crates/jurisearch-cli/src/main.rs:1843`, `2264`, `2962`, `3414`, `3682`, `3910`, `5183`) and depends on `unix_seconds` (`crates/jurisearch-cli/src/main.rs:11925` -> `11918`).
- Phase 4 no longer assigns storage/embedding error mapping to `index_runtime.rs`; it explicitly puts `storage_error_object` and `embedding_error_object*` in `errors.rs`. Source confirms those mappings are used broadly across eval, retrieval, enrichment, ingest, embedding, and status paths, so the shared leaf is the right owner.

# NIT

- `work/06-refactoring/refactoring-plan.md:109` says `legifrance_response_has_results` is "currently uncalled", but source calls it from `enrich_legislation_citations_payload` via `.is_some_and(legifrance_response_has_results)` at `crates/jurisearch-cli/src/main.rs:4792`. This does not affect the module assignment, because it is still legislation-only, but the parenthetical should be made source-accurate. Concrete fix: replace it with "`legifrance_response_has_results` is currently only used by `enrich_legislation_citations_payload`, so it stays with `enrichment/legislation.rs`."

VERDICT: GO
