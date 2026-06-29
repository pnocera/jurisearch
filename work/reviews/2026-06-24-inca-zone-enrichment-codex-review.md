# Code Review: INCA Judilibre Zone Enrichment

## Findings

No findings.

## Review Notes

- `annotate_fetched_parts()` now uses `is_judilibre_cassation_source(document["source"].as_str())` for the offline `official_zones_available` hint, so `inca` gets the same user-visible online hint as `cass` for Judilibre-backed parts while `capp` and `jade` do not.
- `zone_cache_action()` now enriches missing or expired rows only when `online` is set and the source is `cass` or `inca`; `capp` and `jade` continue to fall back for uncached rows.
- The enrichment path itself is unchanged after the source gate: `official_decision_part()` still calls `enrich_decision_from_judilibre()`, which resolves by parser-valid pourvoi plus decision date, fetches `/decision`, normalizes Judilibre zones, and caches the row.
- The migration comment now matches the implemented scope: Cour de cassation sources `cass` and `inca` are supported, while `capp` and `jade` are excluded.
- The added unit coverage exercises the new source predicate and confirms `zone_cache_action(..., "inca")` enriches while `capp` and `jade` fall back for uncached rows.

## Checks

- Static review only, per instructions.
- Did not run `fetch --online` or mutate any index.
- Ran `git diff --check -- crates/jurisearch-cli/src/main.rs crates/jurisearch-storage/src/migrations.rs`; no whitespace errors were reported.
- Did not rerun the test suite or build, because the review brief requested static review only.

VERDICT: GO
