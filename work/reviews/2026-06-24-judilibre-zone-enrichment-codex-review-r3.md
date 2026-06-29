# Code Review: Judilibre Zone Enrichment r3

Scope reviewed: the updated working-tree diff for `fetch --part --online`, with focus on the r2 duplicate-requested-ID WARN in `annotate_fetched_parts`. This was a static review only; I did not run the migration, `fetch --online`, mutate any index, or run tests/builds.

## Findings

No blocking findings.

## r2 WARN Resolution

- The duplicate-ID provenance bug is resolved. `annotate_fetched_parts` now gathers the response decisions first, deduplicates Judilibre/cache lookups with `looked_up.insert(document_id.as_str())` at `crates/jurisearch-cli/src/main.rs:3483`, and stores any official block in a `HashMap` keyed by `document_id`.
- The application pass now uses `official.get(document_id).cloned()` at `crates/jurisearch-cli/src/main.rs:3507` instead of consuming the entry with `remove`, so every copy of a duplicated requested ID receives the same official `part` block.
- The dedup guard also prevents repeated `official_decision_part` calls for the same fetched decision within one response, so duplicate requested IDs no longer cause extra Judilibre/cache resolution work.

## Regression Check

- The earlier cache/TTL behavior remains intact: `zone_cache_action` still serves fresh `ok` rows, suppresses fresh negative/error rows, and only enriches expired/missing rows when `online && source == "cass"`.
- The pourvoi/date guard remains intact: `find_matching_judilibre_id` still requires an exact remote date when the local decision date is known and only falls back to number-only matching when the local date is absent.
- `fetch_documents_json` preserves requested ordinals, including duplicates, and the revised annotation pass now matches that response shape instead of treating the official block as single-use.
- No live verification was performed, per instructions. A duplicate-ID integration test would still be useful coverage when an index-backed fixture is available, but I do not consider its absence blocking for this map-access fix.

VERDICT: GO
