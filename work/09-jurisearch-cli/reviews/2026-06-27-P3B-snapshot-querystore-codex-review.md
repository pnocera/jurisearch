# P3B Snapshot QueryStore Review

## Findings

### BLOCKER: `search --zone` still reads response state outside the request snapshot

`zone_search_payload` wraps only the candidate query in `postgres.begin_snapshot()` (`crates/jurisearch-cli/src/retrieval/zone.rs:129-152`). It then drops that snapshot and calls `zone_retrieval_coverage_json(&postgres)` (`crates/jurisearch-cli/src/retrieval/zone.rs:156-171`) to fill `scope.indexed_decisions`. That coverage function still opens a fresh read via `postgres.execute_read_sql` (`crates/jurisearch-storage/src/zone_units.rs:684-690`) and reads `decision_zones`, `zone_units`, `zone_unit_embeddings`, `documents`, and `index_manifest` (`crates/jurisearch-storage/src/zone_units.rs:692-723`). The same fresh coverage read is also used as the zone readiness gate before the snapshot is opened (`crates/jurisearch-cli/src/retrieval/zone.rs:18-20`, `crates/jurisearch-cli/src/retrieval/zone.rs:99`).

This is the half-moved state the design consultation explicitly rejected: `search --zone` had to move `zone_candidates_json`, `zone_retrieval_coverage_json`, and zone `manifest_default_probes` onto the same snapshot, or leave zone search out of 3B. The implementation did move candidates and `manifest_default_probes`, but not coverage/readiness. A generation activation between the candidate read and the coverage read can return candidates from generation A with `scope.indexed_decisions`/coverage metadata from generation B; a swap between the pre-snapshot zone readiness check and `begin_snapshot()` can also approve a different topology than the one queried. The existing zone test only asserts metadata in a quiescent DB, so it would stay green.

Concrete fix: add `zone_retrieval_coverage_in_snapshot(&mut dyn ReadSnapshot)` and have the existing `zone_retrieval_coverage_json(&ManagedPostgres)` become a one-shot wrapper, matching the other P3B storage helpers. In `zone_search_payload`, open one snapshot before zone readiness, run a snapshot-backed `ensure_zone_retrieval_readiness_in_snapshot`, run `zone_candidates_in_snapshot`, and build `scope.indexed_decisions` from the same snapshot-backed coverage value. Add a no-sleep concurrent-swap test with different zone-unit counts in gen A/B that proves both candidates and scope coverage remain wholly old for an open request and wholly new for the next one.

## Deviation Assessment

- Builder crate scope: acceptable as a 3B cut only because `search` and `cite` result reads are now snapshot-bound in the CLI path and their CLI-entangled shaping can move in P4 before the service consumes them. Do not let P4 depend on `jurisearch-cli`.
- Readiness gate placement: acceptable for the main local CLI gates in 3B because P3A writer-owned activation stamps the installed topology before commit and the local public path is explicitly legacy. This does not cover the zone gate called out above, since that gate reads response-relevant zone coverage and remains a fresh-session read.
- Multi-corpus refusal: acceptable for query snapshots. The rebaseline acceptance read correctly switched to the documented non-indexed `jurisearch_server` union view instead of trying to force a query snapshot over two active corpora.

## Validation

Static/source review of the working-tree diff against `main`, the untracked `jurisearch-query` and `query.rs` files, the P3B design consultation, and the P3B working notes. I did not rerun the cargo validation because the blocker is visible directly in the call path.

VERDICT: FIXES_REQUIRED
