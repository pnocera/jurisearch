# Review: DB Snapshot Rebaseline + Ingest Cursor Seed

## Findings

WARN: `--from-db --dry-run` is not mutation-free.

`crates/jurisearch-producer/src/bin/jurisearch_producer.rs:377` routes `rebaseline --from-db --dry-run` through `run_update`, and `run_update` writes a durable run record before the dry-run branch can return (`crates/jurisearch-producer/src/update.rs:185`). `run_update_inner` also writes the checkpoint at start/fetched before returning from the dry-run branch (`crates/jurisearch-producer/src/update.rs:233`, `crates/jurisearch-producer/src/update.rs:249`). That contradicts the requested CLI contract that `--from-db --dry-run` previews the snapshot baseline set without mutation; the sibling non-`--from-db` dry-run still uses the read-only `plan_forced_rebaseline` short-circuit. Fix: add a read-only snapshot planning path for CLI dry-run, e.g. expose/reuse `planned_rebaseline_baselines(..., snapshot_only = true)` through a planner that returns the same preview JSON shape without calling `run_update`, so no `RunRecord` or `RunCheckpoint` files are written.

## Notes

The main non-dry-run repair path otherwise matches the design. `snapshot_only` reads fetch cursors instead of fetching, skips ingest/enrich/embed, recomputes the forced baseline set under the update lock, runs `rebaseline_preflight` before `rebaseline_cycle`, adopts baselines only after publish, and seeds ingest cursors only after adoption while still under the lock. The normal update, ordinary forced rebaseline, automatic rebaseline, and first-baseline preflight paths remain structurally unchanged.

The baseline fallback adjustment is implemented through one planning seam: `snapshot_only_baselines` prefers `FetchCursor.baseline_file_name`, falls back to `AdoptedBaseline.baseline_file_name`, omits sources with neither, and the empty group still surfaces `NothingToRebaseline`.

The cursor seed writes a completed memberless `ingest_run` via the standard lifecycle helpers, with `manifest.freshness.latest_archive_timestamp_compact` and `member_limited=false`; that is exactly what `latest_completed_ingest_archive_compact_with_client` reads. It does not touch the fetch cursor, and publish/preflight failures do not reach the seed call.

The package-build preflight wrapper reuses `bootstrap_preflight`, so it checks schema currency, chunk embedding coverage/fingerprint consistency, and zone-unit embedding coverage/fingerprint consistency. Build errors map to `publish-failed`.

The added unit and PG-gated tests are meaningful for the routing/fallback/cursor/preflight/package-generation behavior described in the brief. I did not rerun the validation commands during this review; the PG-gated tests still depend on external PostgreSQL configuration.

VERDICT: FIXES_REQUIRED
