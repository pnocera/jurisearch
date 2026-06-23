# Codex Review r3 - Phase 2.5 incremental sync

## BLOCKER

- None.

## WARN

- None.

## NIT

- None.

## Review Notes

- The r3 change fixes the r2 blocker at the default CLI run-id source. `default_juri_run_id` and `default_legi_run_id` now use `unique_run_suffix()`, which includes nanosecond time, process ID, and an atomic in-process sequence. That removes the second-resolution collision that allowed `start_ingest_run_with_client`'s `ON CONFLICT (run_id) DO UPDATE` path to overwrite a prior completed run's manifest.
- The CLI contract test now asserts that two immediate same-source syncs receive different generated `run_id` values before checking that the no-op sync does not erase the prior processed freshness. This directly covers the previously failing production-shaped path.
- I did not find a remaining correctness issue in the reviewed diff.

## Verification

- `git diff --check HEAD~1 HEAD` - passed.
- `CARGO_TARGET_DIR=/tmp/jurisearch-review-target cargo test -p jurisearch-cli --bin jurisearch default_run_ids_are_unique_across_rapid_calls` - passed.
- `CARGO_TARGET_DIR=/tmp/jurisearch-review-target cargo test -p jurisearch-cli --bin jurisearch normalize_since` - passed.
- `CARGO_TARGET_DIR=/tmp/jurisearch-review-target cargo test -p jurisearch-cli --test cli_contract sync_pulls_new_deltas_incrementally_with_since_filter` - passed.

VERDICT: GO
