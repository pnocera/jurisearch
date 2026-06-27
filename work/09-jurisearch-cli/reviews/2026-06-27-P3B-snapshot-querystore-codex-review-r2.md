# P3B Snapshot QueryStore Re-Review

## Findings

### BLOCKER: `search --zone` still splits readiness, candidates, and response coverage across separate snapshots

The attempted fix added snapshot-bound storage helpers, but the production adapter does not use one request snapshot. `zone_search_payload` opens a snapshot for the zone readiness gate at `crates/jurisearch-cli/src/retrieval/zone.rs:106-107`, then opens a second snapshot for `zone_candidates_in_snapshot` at `crates/jurisearch-cli/src/retrieval/zone.rs:137-160`, then reads `scope.indexed_decisions` through `zone_retrieval_coverage_json(&postgres)` at `crates/jurisearch-cli/src/retrieval/zone.rs:164-179`. That wrapper opens its own fresh snapshot at `crates/jurisearch-storage/src/zone_units.rs:689-691`.

So the response can still mix generations: a swap after the readiness gate can make candidates read generation B after readiness approved generation A, and a swap after candidate retrieval can make `scope.indexed_decisions` report generation B/C while the candidates came from an earlier generation. The comment at `crates/jurisearch-cli/src/retrieval/zone.rs:103-105` says one snapshot spans the whole request, but the code immediately violates that by shadowing `snapshot` inside the candidate block and by calling the one-shot coverage wrapper for response state. The `scope.indexed_decisions` field is not merely adapter-side diagnostics; it is part of the public `search --zone` response.

The new zone test is false-green for this regression. `zone_coverage_is_read_through_the_request_snapshot_across_a_swap` only proves that `zone_retrieval_coverage_in_snapshot` is stable when a caller keeps passing the same snapshot (`crates/jurisearch-storage/tests/query_snapshot_p3b.rs:185-210`). It never exercises `zone_search_payload`, so it would still pass with the current production code that opens new snapshots for candidates and response coverage.

Concrete fix: in `zone_search_payload`, keep the snapshot opened at `crates/jurisearch-cli/src/retrieval/zone.rs:106` and pass that same `&mut *snapshot` to `ensure_zone_retrieval_readiness`, `zone_candidates_in_snapshot`, and `zone_retrieval_coverage_in_snapshot`. Remove the inner `begin_snapshot()` and do not call `zone_retrieval_coverage_json(&postgres)` for response `scope`. Add an acceptance test that drives the actual zone-search adapter path, or a factored adapter helper with the same snapshot lifetime, across a no-sleep activation swap and asserts the candidate generation and `scope.indexed_decisions` stay wholly old for the open request and wholly new for the next request.

## Validation

Static/source re-review of the working-tree diff, the updated `zone_search_payload` path, the snapshot-bound zone helpers, and `query_snapshot_p3b`. I did not rerun the listed cargo validation; the blocker is visible in the live call path and explains why the current new test can remain green.

VERDICT: FIXES_REQUIRED
