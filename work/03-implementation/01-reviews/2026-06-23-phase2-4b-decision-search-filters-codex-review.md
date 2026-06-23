# Code Review: Phase 2.4-B Decision Search Filters

## BLOCKER

None.

## WARN

1. `--decided-from` / `--decided-to` are not decision-scoped, so explicit date filters can affect LEGI article searches by article version start rather than decision date.

   Evidence: `DecisionFilters::predicate()` appends the date clauses as plain `AND d.valid_from >= ...::date` / `AND d.valid_from <= ...::date` with no `d.kind = 'decision'` guard (`crates/jurisearch-storage/src/retrieval.rs:127-137`). In the CLI search path, `search_with_postgres()` only forces `kind_filter: Some("article")` for `LegalKind::Code`; the default `LegalKind::All` leaves `kind_filter` as `None` (`crates/jurisearch-cli/src/main.rs:2551-2555`). Since user `--kind decision` is still rejected before this path (`crates/jurisearch-cli/src/main.rs:2371-2377`), the practical CLI use of these decision-date flags is either `--kind all` or `--kind code`.

   Impact: empty filters are still a no-op, and the normal LEGI article path is unchanged. The footgun is opt-in: with `--kind code --decided-from ...`, articles are filtered by version start; with default `--kind all --decided-from ...`, articles can remain in results if their `valid_from` falls in the requested "decision" range. That does not match the flag wording or the `DecisionFilters` type name.

   Concrete fix: decide whether any non-empty `DecisionFilters` should imply `d.kind = 'decision'`, or at least whether the date filters should be wrapped as `AND d.kind = 'decision' AND d.valid_from ...`. If article-date filtering is intentionally supported through these flags, rename/document the CLI/session fields accordingly and add explicit tests for `kind_filter: Some("article")` plus date filters so this behavior is locked in.

## NIT

None.

## Verified

- Injection safety: all five user-controlled values in `DecisionFilters::predicate()` pass through `sql_string_literal`, including the `%{value}%` strings used for `ILIKE` (`crates/jurisearch-storage/src/retrieval.rs:109-137`). Each generated fragment starts with `AND`, so concatenation with the existing `kind_predicate` remains valid SQL.
- CTE placement: `hybrid_candidates_json()` builds `filter_predicate = kind_predicate + decision_filters.predicate()` and passes it into `ranked_candidate_ctes()` (`crates/jurisearch-storage/src/retrieval.rs:260-266`). The combined predicate is then used in the same `documents d` candidate-CTE positions that previously used `kind_predicate`: Hybrid lexical and dense CTEs, BM25 lexical CTE, and Dense dense CTE (`crates/jurisearch-storage/src/retrieval.rs:555-591`, `633-642`, `680-689`). Final SELECT, grouping, cursor, and pagination SQL are unchanged.
- No-op fidelity: `DecisionFilters::default()` returns an empty predicate string, so `filter_predicate` is byte-equal to the prior `kind_predicate` value for existing call sites (`crates/jurisearch-storage/src/retrieval.rs:107-140`, `260-266`).
- CLI/session wiring: `SearchArgs::decision_filters()` borrows the five optional strings with `as_deref()` and the value is passed into `HybridCandidateQuery` from `search_with_postgres()` (`crates/jurisearch-cli/src/main.rs:298-305`, `2580-2593`). `SessionSearchArgs` has serde defaults for all five new fields and forwards them into `SearchArgs` (`crates/jurisearch-cli/src/main.rs:376-385`, `3074-3102`). Internal eval/compare/france-legi call sites use `DecisionFilters::default()`.
- Publication exact-match: the ingest projection stores judicial `PUBLI_BULL@publie` and administrative `PUBLI_RECUEIL` as the canonical `publication` value (`crates/jurisearch-ingest/src/juri/mod.rs:671-674`), with tests covering `"oui"` and `"C"` values (`crates/jurisearch-ingest/src/juri/tests.rs:89`, `194`). Exact case-insensitive comparison is consistent with those stored code-like values.

VERDICT: GO
