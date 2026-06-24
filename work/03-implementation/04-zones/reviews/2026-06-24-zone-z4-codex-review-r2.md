# Code Review: Zone Retrieval Z4 R2

Reviewed `main` at `4d1fb8f47409f29fc4782d65ccb55a41d5e0e0f9` (`Zone retrieval Z4 r1 fixes (codex)`) against `origin/main` at `e2bc5baab7270340515c0bffecb815dbfca7340f`.

The R1 blockers are materially addressed in the implementation: `search --zone --as-of` now threads an as-of anchor into `ZoneCandidateQuery`, zone/decision/as-of filters are applied inside the lexical and dense candidate arms before ranking and limiting, and dense zone readiness compares the expected query storage fingerprint with `index_manifest['zone_embedding']`.

## Findings

### 1. `help schema --json` does not describe the new zone request or routing response values

- Location: `crates/jurisearch-core/src/schema.rs:78-91`, `crates/jurisearch-core/src/schema.rs:122-130`, `crates/jurisearch-cli/src/main.rs:3075-3109`
- Severity: High

`SearchArgs` and `SessionSearchArgs` now accept `zone`, and `zone_search_payload` emits `routing.query_type = "zone"` plus `routing.chosen_backend = "official_zone_retrieval"`. The published schema still has no `SearchRequest.properties.zone`, and its routing enums only allow `query_type` values `citation|semantic` and backend values `hybrid|bm25|dense|structured_citation`.

That makes the new machine-facing search feature invisible to agents using `help schema --json`, and it makes successful zone-search responses schema-invalid according to the repository's own contract. This is especially risky because session JSON already forwards `zone` through `SessionSearchArgs`; an agent that follows the schema cannot discover or validate that field.

Actionable fix: update `compiled_schema()` so `SearchRequest` includes `zone: motivations|moyens|dispositif`, and update `SearchResponse.routing` to include the zone route values and any zone-specific routing fields. Add CLI contract assertions that `help schema --json` advertises `zone`, `query_type = zone`, and `chosen_backend = official_zone_retrieval`.

### 2. The zone route skips the standard search response decoration while still advertising `format`

- Location: `crates/jurisearch-cli/src/main.rs:3073-3109`, compared with `crates/jurisearch-cli/src/main.rs:3366-3429`
- Severity: Medium

The normal search path always decorates search responses with `expansion_seed_version`, `expanded_terms`, pagination guidance (`cursor_note` and `guidance`), and, when `--format detailed` / session `"format":"detailed"` is requested, a `diagnostics` block. The new zone route returns before that shared decoration and only sets `format`, `limit`, `scope`, `pagination`, and `routing`.

As a result, `search --zone ... --format detailed` returns `"format":"detailed"` without detailed diagnostics, and all zone-search responses omit the expansion fields that the implementation plan and `SearchResponse` schema expose as standard search metadata. The pagination object also lacks the established cursor note/guidance fields. This creates a surprising compatibility split between ordinary search and the new `--zone` search surface.

Actionable fix: factor the common response decoration out of `search_with_postgres` and reuse it from `zone_search_payload`, with zone-aware retrieval diagnostics (`mode`, `uses_lexical`, `uses_dense`, `lexical_limit`, `dense_limit`, `query_limit`, `zone`, `after_cursor`, and fingerprint when dense). Add CLI/session regression tests for `search --zone --mode bm25 --format detailed` that assert `diagnostics`, `expanded_terms`, `expansion_seed_version`, and pagination guidance are present.

## Verification

- `git status --short --branch`
- `git log --oneline --decorate -n 12`
- `git diff --stat origin/main..HEAD`
- `git diff --find-renames --find-copies origin/main..HEAD`
- `git show --find-renames --find-copies --stat 2ef7b42..HEAD`
- `git show --find-renames --find-copies --unified=80 4d1fb8f -- crates/jurisearch-cli/src/main.rs crates/jurisearch-storage/src/zone_retrieval.rs crates/jurisearch-storage/tests/zone_units.rs`
- `git diff --check origin/main..HEAD`
- CodeGraph context/explore for `zone_search_payload`, `ensure_zone_retrieval_readiness`, `ZoneCandidateQuery`, `zone_candidates_json`, `ranked_zone_ctes`, `DecisionFilters::predicate`, and `document_cursor_predicate`
- Focused static reads of `crates/jurisearch-cli/src/main.rs`, `crates/jurisearch-storage/src/zone_retrieval.rs`, `crates/jurisearch-storage/tests/zone_units.rs`, `crates/jurisearch-core/src/schema.rs`, `crates/jurisearch-cli/tests/cli_contract.rs`, and `work/03-implementation/IMPLEMENTATION_PLAN.md`

I did not run Cargo tests because the review request prohibited modifying files other than the requested review artifact, and Cargo test/check would write build outputs.

VERDICT: FIXES_REQUIRED
