# Delta-Only Ingest Re-Review R2

## Findings

None.

## Verification

The r1 BLOCKER is resolved. Both LEGI and JURI derive `member_limited` directly from `limit_members.is_some()` before the ingest loop and pass it into both the initial `running` manifest and final manifest (`crates/jurisearch-pipeline/src/ingest/legi.rs:145`, `crates/jurisearch-pipeline/src/ingest/legi.rs:152`, `crates/jurisearch-pipeline/src/ingest/legi.rs:285`, `crates/jurisearch-pipeline/src/ingest/juri.rs:137`, `crates/jurisearch-pipeline/src/ingest/juri.rs:144`, `crates/jurisearch-pipeline/src/ingest/juri.rs:235`). The flag is emitted only inside a non-null `freshness` object built from `latest_processed.map_or(Value::Null, ...)`, so no-op/empty-window runs with no selected archive still leave `freshness: null` unaffected (`legi.rs:70`, `juri.rs:59`).

`stopped_by_limit` cannot be true when `limit_members` is absent: `read_archive_members_batched` computes it as `limit_members.is_some_and(|limit| visited >= limit)` (`crates/jurisearch-pipeline/src/ingest/run.rs:122`). Marking every run that supplied `--limit-members` as member-limited is conservative, including the case where the cap was not actually reached, and is acceptable because these are CLI partial/smoke-oriented runs rather than producer runs.

The completed-run cursor now excludes member-limited runs with `AND COALESCE((manifest->'freshness'->>'member_limited')::boolean, false) = false` while still requiring `source = $1`, `status = 'completed'`, and a non-null `latest_archive_timestamp_compact` (`crates/jurisearch-storage/src/ingest_accounting/runs.rs:210`). This prevents a completed `--limit-members` run from advancing the producer cursor. Treating historical rows without the flag as not limited is acceptable for this slice: absent flags preserve existing completed-run semantics, while newly limited runs explicitly write `member_limited: true`. Producer-created ingest requests remain eligible because producer `ingest_one` always passes `limit_members: None` (`crates/jurisearch-producer/src/update.rs:522`).

The r1 WARN is resolved. `ingest_one` computes the per-source `full_scan` predicate before opening a storage client for the cursor, and calls `latest_completed_ingest_archive_compact_with_client` only when `full_scan` is false (`crates/jurisearch-producer/src/update.rs:511`). Passing `None` into `choose_ingest_mode` for full-scan sources remains correct because the pure selector re-computes the same per-source baseline predicate first and returns a full scan before inspecting the cursor (`update.rs:442`).

The r1 verified items still hold. The cursor helper is completed-run based, not member-max based; the producer still fails closed after ingest unless `report.run_status == IngestRunStatus::Completed` (`update.rs:538`); the predicate remains per-source; `select_archives_to_process` keeps the inclusive `>=` delta bound and skips the baseline in incremental mode (`crates/jurisearch-pipeline/src/ingest/mod.rs:88`); the `select_tests` cover the full/incremental/boundary cases; and the accepted quarantine-gap deferral is unchanged by this delta.

The new PG-gated `completed_run_cursor_excludes_member_limited_runs` regression test is meaningful: it inserts a prior completed full run at `20250714000000`, inserts a later completed member-limited run at `20250715000000`, and asserts the helper still returns the prior full cursor (`crates/jurisearch-storage/tests/ingest_accounting.rs:497`). I do not see a false-green in that test for the intended SQL regression; without the new member-limited predicate, the `max(...)` would return the later limited compact.

I did not run the DB-backed tests in this review; the instruction notes they were not expected to run locally without the PG stack.

VERDICT: GO
