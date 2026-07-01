# Review - producer `--accept-stale-cursor`

No BLOCKER/WARN/NIT findings.

I verified the scoped working-tree diff against the requested behavior:

- `--accept-stale-cursor` is only exposed on the producer `update` subcommand and is copied into `UpdateOptions` for that path.
- `UpdateOptions::new`, `UpdateOptions::rebaseline`, and `UpdateOptions::rebaseline_from_db` keep the override false by default.
- The flag is threaded through `run_update_inner` -> `ingest_one` -> `choose_ingest_mode`.
- `choose_ingest_mode` still full-scans for a pending source baseline before consulting any cursor, still full-scans when no completed-ingest cursor exists, and still fails closed on an unparseable cursor even when the override is true.
- The override affects only the age comparison: an age-stale parseable cursor resolves to the same delta-only `ArchiveModeChoice { incremental: true, since_compact: Some(cursor) }` as a fresh cursor when the flag is enabled.
- `snapshot_only` / `--from-db` skips the ingest loop, so this override does not alter that path. The rebaseline CLI path has no new flag and continues to build its own `UpdateOptions`.
- The new unit test is meaningful: it would fail if the flag did not gate the stale-age error, and it would also fail if the override leaked to malformed cursors.
- The `accepted_stale_cursor` JSON field is consistently wired to the update invocation's override flag on successful update output.

I did not rerun the already-reported validation commands; this review was source/diff inspection only.

VERDICT: GO
