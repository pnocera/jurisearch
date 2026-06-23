# Phase III Gate Split Review r2

Reviewed commit `8256d474ffb635acf1abc88423728cfa46648360` (`Harden split gate: require routing audit + floor gate metrics (Phase III fixes)`) against `/tmp/codex-phase3-r2.md`.

## Findings

No severity-tagged findings.

## Verified Behavior

- The routing audit is now enforced on the accepted artifact path. `phase1_france_legi_artifact_errors` calls `phase1_france_legi_validate_category` for both gating categories with `advisory = false` and for `semantic_retrieval` with `advisory = true`. The helper requires `categories.<category>.routing_backends` to be an object, sums the backend counts, requires the total to equal `queries`, and for non-advisory categories additionally requires `routing_backends.structured_citation == queries`. This rejects the previously open cases: missing audit, hybrid-only gating audit, and partial accounting.

- The advisory category also requires complete routing accounting. It does not require a specific backend, which matches the split design, but omission or partial totals still produce validation errors through the shared `routing_backends` object and total-count checks.

- The runner/status boundary divergence is fixed for the reviewed thresholds. `france_legi_artifact` still decides `state` from raw metrics, but records `floor_metric(...)` for each category, so a below-floor raw value cannot be rounded upward into a passing recorded `metric_value`. The new boundary test covers `0.9496 -> 0.949` as failed and `0.9504 -> 0.950` as passed.

- The committed benchmark artifact passes the stricter France-LEGI gate legitimately: `structured_citation_resolution.routing_backends.structured_citation == 60`, `temporal_version_pinning.routing_backends.structured_citation == 12`, and `semantic_retrieval.routing_backends.hybrid == 120`, matching each category's `queries`.

## Validation

- `cargo test -p jurisearch-cli france_legi` passed: 11 France-LEGI tests passed.
- `cargo test -p jurisearch-cli -p jurisearch-storage` passed: 22 CLI unit tests, 45 CLI contract tests, 9 storage unit tests, and the non-ignored integration tests passed.
- `git diff --check 8256d47^ 8256d47` passed.
- With `JURISEARCH_PHASE1_FRANCE_LEGI_BENCHMARK=work/03-implementation/02-evidence/2026-06-23-france-legi-benchmark.json`, `target/debug/jurisearch status` reports the `france_legi_official_eval` check as `pass` and `phase1_gate.france_legi_benchmark.artifact_error` as `null`. In this checkout the overall `phase1_gate.claim_allowed` is still `false` because unrelated live index/model gates are pending or failing, not because of the France-LEGI artifact hardening.

VERDICT: GO
