# Codex Review r2 - Phase 2.5 incremental sync

## BLOCKER

- `crates/jurisearch-cli/src/main.rs:4055` / `crates/jurisearch-storage/src/ingest_accounting.rs:254` - The r2 regression path is still broken because default archive run IDs are only second-resolution and `start_ingest_run_with_client` overwrites on `ON CONFLICT (run_id)`. The new test performs a delta sync and then a no-op sync immediately, both without `--run-id`; both can get the same default `cass-{unix_seconds}` run ID. The second no-op run then updates the first completed delta run's manifest to `source_version: null`, and `corpus_source_coverage_json` correctly filters that row out, leaving status to fall back to the baseline run. I reproduced this with:
  `cargo test -p jurisearch-cli --test cli_contract sync_pulls_new_deltas_incrementally_with_since_filter`
  which fails at `crates/jurisearch-cli/tests/cli_contract.rs:3714` with status reporting `20250101-000000` instead of `20250201-000000`. This means the advertised validation does not pass, and in production two rapid same-source syncs can erase the freshness evidence from the first run. Make default run IDs collision-resistant (subsecond timestamp plus suffix/UUID, or equivalent) and keep a regression assertion that two immediate same-source syncs do not reuse the same `run_id`. Passing explicit distinct `--run-id`s would fix only the test; the CLI default should be fixed too because the overwrite semantics are real.

## WARN

- None.

## NIT

- None.

## Verification

- `cargo test -p jurisearch-cli --bin jurisearch tests::normalize_since` - passed.
- `cargo test -p jurisearch-cli --test cli_contract sync_pulls_new_deltas_incrementally_with_since_filter` - failed as described above.

VERDICT: FIXES_REQUIRED
