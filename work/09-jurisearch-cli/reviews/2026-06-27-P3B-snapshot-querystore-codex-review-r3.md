# P3B Snapshot QueryStore Re-Review r3

## Findings

No BLOCKER/WARN/NIT findings.

## Re-Review Notes

The r2 blocker is addressed in the live working tree. `zone_search_payload` now opens one request snapshot at `crates/jurisearch-cli/src/retrieval/zone.rs:125`, runs the zone readiness gate through that same handle at `crates/jurisearch-cli/src/retrieval/zone.rs:126`, and then passes the same snapshot to `zone_candidates_and_coverage_in_snapshot` at `crates/jurisearch-cli/src/retrieval/zone.rs:157-176`. The response `scope.indexed_decisions` is built from the coverage value returned by that same helper at `crates/jurisearch-cli/src/retrieval/zone.rs:183-189`, so candidates and response coverage no longer split across separate snapshots.

The storage side now matches the intended shape: `zone_retrieval_coverage_json(&ManagedPostgres)` is a legacy one-shot wrapper over `zone_retrieval_coverage_in_snapshot(&mut dyn ReadSnapshot)` at `crates/jurisearch-storage/src/zone_units.rs:689-702`, and `zone_candidates_json(&ManagedPostgres, ...)` is a legacy wrapper over `zone_candidates_in_snapshot(&mut dyn ReadSnapshot, ...)` at `crates/jurisearch-storage/src/zone_retrieval.rs:205-216`. The zone dense-probe manifest read also goes through the passed snapshot via `manifest_default_probes(snapshot, "zone_embedding")` at `crates/jurisearch-storage/src/zone_retrieval.rs:229-234`.

The new swap test is not false-green for the prior regression. `zone_candidates_and_coverage_share_one_snapshot_across_a_swap` opens generation A's snapshot, activates generation B with an additional zone-bearing decision, then asserts both coverage and candidates remain `1` through the old snapshot at `crates/jurisearch-cli/src/retrieval/zone.rs:404-416`; a fresh snapshot must then see both values as `2` at `crates/jurisearch-cli/src/retrieval/zone.rs:419-432`. If either the candidate read or the coverage read reopened a fresh snapshot after the swap, the old-snapshot assertions would observe generation B and fail.

## Validation

Static/source re-review of `/tmp/codex-review-p3b-r3.md`, the r2 review, the working-tree diff, `zone.rs`, the snapshot QueryStore implementation, and the zone storage helpers. I did not rerun the listed cargo validation; the brief says the focused cargo checks/tests were already run green.

VERDICT: GO
