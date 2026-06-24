# Code Review: Zone Retrieval Z4 R3

Reviewed `main` at `c4a753bef7b4a614433f631dc9acba66b95d3694` (`Zone retrieval Z4 r2 fixes (codex)`) against the prior reviewed commit `4d1fb8f47409f29fc4782d65ccb55a41d5e0e0f9`.

The R2 findings are materially addressed. `help schema --json` now advertises `SearchRequest.zone`, the zone-specific routing values, and a `SearchResponse.scope` block. The zone route also shares the ordinary search metadata surface for expansion, pagination note/guidance, and detailed diagnostics.

## Findings

No blocking issues found in the reviewed patch.

## Notes

- `search_pagination_value` centralizes the pagination block used by both ordinary search and zone search without changing the ordinary path's cursor semantics.
- `zone_search_payload` now adds `expansion_seed_version`, `expanded_terms`, `pagination.cursor_note`, `pagination.guidance`, `routing.fallback_path`, and detailed retrieval diagnostics when `format=detailed`.
- `compiled_schema()` now exposes the zone request field, the zone route enum values, and the zone scope response object.
- The added `cli_contract` assertions cover the schema-visible parts of the R2 schema finding.

## Verification

- `git status --short`
- `git log --oneline -8`
- `git show --stat --oneline --decorate --name-status c4a753b`
- `git diff --stat 4d1fb8f..c4a753b`
- `git diff --check 4d1fb8f..c4a753b`
- `git show --no-ext-diff --find-renames --find-copies --unified=30 c4a753b -- crates/jurisearch-cli/src/main.rs`
- CodeGraph context/node review for `zone_search_payload`, `search_with_postgres`, `search_pagination_value`, `zone_candidates_json`, `ranked_zone_ctes`, `document_cursor_predicate`, `parse_search_cursor`, `SearchArgs`, `SessionSearchArgs`, and `CliZone`.
- Static reads of `crates/jurisearch-core/src/schema.rs`, `crates/jurisearch-cli/tests/cli_contract.rs`, and the prior R2 review artifact.

I did not run Cargo tests because the review request prohibited modifying files other than this review artifact, and Cargo test/check would write build outputs.

VERDICT: GO
