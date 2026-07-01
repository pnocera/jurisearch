# Delta-Only Ingest Review

## Findings

BLOCKER: `latest_completed_ingest_archive_compact_with_client` treats every `ingest_run.status='completed'` row as a safe archive cursor, but the ingest pipeline can mark a `--limit-members` smoke/partial CLI run as `Completed` even though it did not process all selected archive members. In both LEGI and JURI, `latest_processed` is set before the loop from `archives.last()` (`crates/jurisearch-pipeline/src/ingest/legi.rs:136`, `crates/jurisearch-pipeline/src/ingest/juri.rs:129`), the archive loop breaks on `report.stopped_by_limit` (`legi.rs:193`, `juri.rs:186`), and the terminal status is still `Completed` whenever there are no failed members or fatal errors (`legi.rs:285`, `juri.rs:236`). The CLI exposes `--limit-members` specifically for partial runs (`crates/jurisearch-cli/src/args.rs:731`, `crates/jurisearch-cli/src/args.rs:755`) and passes it into the same ingest-run table (`crates/jurisearch-cli/src/ingest.rs:149`, `crates/jurisearch-cli/src/ingest.rs:183`). A completed partial full ingest can therefore write a manifest freshness cursor at the planned last archive, and the new producer cursor query at `crates/jurisearch-storage/src/ingest_accounting/runs.rs:201` can later return that cursor; `producer update` will then run delta-only and skip archives/members that were never actually processed. Concrete fix: partial/limited ingest runs must not be eligible for completed-run cursors. Prefer making `stopped_by_limit` terminally non-completed (or at least omit `freshness.latest_archive_timestamp_compact` / mark the run in `manifest` and filter it out) for both LEGI and JURI, and add a regression test that a completed-looking limited run cannot advance the producer cursor.

WARN: `ingest_one` reads and validates the completed-run cursor before checking whether the source is in `new_baselines` (`crates/jurisearch-producer/src/update.rs:507`). The pure predicate correctly says a pending new baseline full-scans regardless of cursor (`update.rs:448`), but the call site can still fail on a malformed stored cursor from `latest_completed_ingest_archive_compact_with_client` before it reaches that full-scan decision. That blocks the intended repair/re-anchor path even though a full scan does not need the archive cursor. Concrete fix: compute the per-source `full_scan` predicate before opening the cursor, and only call `latest_completed_ingest_archive_compact_with_client` for non-full-scan sources; or make the cursor read lazy inside the mode selector.

## Verified

The main producer wiring matches the intended flow: `new_baselines` is computed before the ingest loop, `now_unix()` is sampled once for the loop, and each source receives the same pending-baseline set (`crates/jurisearch-producer/src/update.rs:286`, `update.rs:303`, `update.rs:306`). Manual mode still calls `ensure_incremental_may_proceed` before ingest, and dry-run returns before this path.

`choose_ingest_mode` is per-source and otherwise implements the requested predicate: source in `new_baselines` -> full scan, no cursor -> full scan, unparseable/stale cursor -> `IngestCursorStale`, fresh cursor -> incremental with `since_compact`. The 45-day limit is a reasonable fail-closed margin under the stated ~62-day DILA delta retention window.

The run-status guard is present and in the right place after `ingest_archives` for both full and delta modes (`crates/jurisearch-producer/src/update.rs:529`, `update.rs:533`). `IngestRunStatus::Completed` is written only when `failed_members == 0` and no fatal error remains in the LEGI/JURI lifecycle, excluding the limited-run hole above.

The storage cursor helper uses `ingest_run.status='completed'`, not member maxima, and pulls `manifest->'freshness'->>'latest_archive_timestamp_compact'` (`crates/jurisearch-storage/src/ingest_accounting/runs.rs:201`). It validates the returned cursor as 14 ASCII digits and returns `None` for no completed run. Surfacing malformed stored cursor state as `StorageError` is the right default fail-closed behavior for delta-only mode, but full-scan sources should not need this read.

`select_archives_to_process` keeps the correctness-safe inclusive `>=` lower bound and skips the baseline when `incremental=true` (`crates/jurisearch-pipeline/src/ingest/mod.rs:92`). The new select tests assert baseline inclusion/exclusion and the `==` / `<` boundary. The `choose_ingest_mode` unit tests cover the requested branches and would catch the obvious predicate regressions.

The run-status variants are correctly classed as `ingest-failed`, and using `source_token` instead of `source` is justified because `thiserror` treats a field named `source` as an error source/cause.

The quarantine gap-guard deferral is acceptable for this slice. Fetch moves integrity-failed downloads to `state_dir/quarantine/<source>` without advancing the fetch cursor (`crates/jurisearch-fetch/src/engine.rs:219`), while later successful re-fetch promotes the accepted archive into the mirror and records the cursor (`engine.rs:207`, `engine.rs:212`) without removing the stale quarantine copy. A naive guard on quarantine presence would therefore false-fail normal re-fetch recovery; a correct guard needs to cross-check the accepted mirror/cursor state. The stale-age guard remains the primary protection here.

Publish/enrich/embed interactions look intact: delta-only ingest only changes which archive files are opened; publish still derives package deltas from storage change-log state rather than the archive cursor, and the producer now refuses to proceed after partial ingest.

## Test Audit

The PG-gated cursor test exercises the core failed-run property by proving a later `failed` run does not advance past a prior completed cursor, and it also checks `None` for another source. It does not run without local PG/pgvector/pg_search, so I did not execute it here. The current tests do not cover the limited-run cursor poisoning case above and should be extended before this ships.

VERDICT: FIXES_REQUIRED
