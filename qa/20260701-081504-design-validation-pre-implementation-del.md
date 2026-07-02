# Q&A — 20260701-081504

## Question

# Design validation (pre-implementation) — delta-only steady-state ingest for `producer update`

Repo `/home/pierre/Work/jurisearch`. **Read the actual source; don't trust this prose.** Confirm the design is
correct/sufficient or push back, against the real code. This is a design gate before I implement.

## Problem
`producer update --group <g>` re-walks the FULL DILA baseline every run because `ingest_one`
(`crates/jurisearch-producer/src/update.rs:415-444`) hardcodes `ArchiveSyncFilter { incremental:false, since_compact:None }`.
DILA facts (confirmed from echanges.dila.gouv.fr/OPENDATA/LEGI/): the global baseline `Freemium_<src>_global_*.tar.gz`
is published ~1-2×/year; `LEGI_*` (and per-source) deltas are near-daily; DILA keeps deltas ~62 days server-side.
Goal: steady-state runs ingest ONLY new deltas; a full baseline re-walk happens ONLY when a new global baseline is present.

## Proposed design (verify each claim against source)
1. **The new-baseline signal is already computed BEFORE ingest.** `new_baselines: Vec<(source_token, baseline_file)>`
   and `do_rebaseline` are built under the update-core lock at `update.rs:~265-296` (from `forced_rebaseline_baselines`
   or `group_run_kind`), ~10 lines above the ingest loop at `:300-304`. So `ingest_one` can receive `new_baselines`
   with no phase reordering. Confirm this ordering and that `new_baselines` membership per source is the correct,
   authoritative "a new baseline is pending for THIS source" predicate (same signal that drives `run_rebaseline_cycle`).
2. **The delta-only path already exists and is correct.** `select_archives_to_process`
   (`crates/jurisearch-pipeline/src/ingest/mod.rs:88-105`): `incremental:true` skips `plan.baseline` entirely and
   selects only deltas with `compact >= since_compact`; the CLI `sync` (`crates/jurisearch-cli/src/ingest.rs:~46-71`)
   already uses this. Confirm `incremental:true + since_compact:Some(cursor)` never opens/reads the baseline tar and
   yields exactly deltas at/after the cursor.
3. **Cursor source = DB `ingest_member` (authoritative).** Add a storage helper
   `latest_ingested_archive_compact(db, source)` = max archive timestamp among that source's `ingest_member` rows with
   `status IN ('inserted','skipped')` (`crates/jurisearch-storage/src/ingest_accounting/members.rs`,`resume.rs`),
   parsing names via `ParsedArchive::parse_file_name`. Verify: (a) this cannot report a cursor AHEAD of what was
   actually ingested (so a delta can't be wrongly skipped); (b) it returns `None` for a cold DB or a hand-loaded corpus
   with no ingest rows (→ fall back to full); (c) using this DB read is safer than the on-disk RunRecord
   `ingest_journals` cursor. Flag if `ingest_member` semantics differ from this assumption.
4. **Decision predicate in `ingest_one`** (extract a pure, unit-testable helper):
   `full_scan = new_baselines.any(|(tok,_)| tok == source.as_str())`.
   if `full_scan` → `{incremental:false, since_compact:None}` (as today);
   else `latest_ingested_archive_compact` → `None`→full; `Some(c)` && stale(c) → `Err(IngestCursorStale)`;
   `Some(c)`→`{incremental:true, since_compact:Some(c)}`. Per-source (multi-source groups: only the source(s) in
   `new_baselines` full-scan). Confirm this keeps ingest-mode and the rebaseline publish consistent by construction.
5. **Correctness / edge cases — verify each against source:**
   - First run / cold DB → `None` → full; also the source is in `new_baselines` on a first run (fetched≠adopted), so
     full anyway.
   - **Recovery after an embed-failure** (the live scenario): adoption markers are written only AFTER publish
     (`update.rs:~343` `adopt_new_baselines`), so a failed REBASELINE keeps fetched≠adopted → source stays in
     `new_baselines` → next run full-scans again until publish succeeds. A failed ORDINARY incremental committed its
     delta members to `ingest_member` per batch, so the DB cursor already covers them and delta-only resumes without
     data loss. Confirm both.
   - **62-day retention gap:** the producer mirror RETAINS accepted archives (retention reclaims only temp/partial/
     quarantine — `retention.rs`, `rebaseline_retention_freshness.rs`), so a continuously-fetching producer keeps a
     contiguous delta chain and delta-only is safe. The gap only bites if fetching STOPPED > retention (deltas aged off
     the server before download). There is NO existing gap detection. So ship a conservative guard WITH this change:
     if `now - unix(cursor)` exceeds a window safely below DILA retention (e.g. 45 days), FAIL with a new
     `IngestCursorStale` error (needs-attention class) rather than silently delta-skip. Assess: is failing-closed on a
     stale cursor the right call, is 45d a sound margin under ~62d, and is the compact→unix math available
     (`crates/jurisearch-producer/src/timestamp.rs`)? Is there a subtler correctness hole than a simple age check
     (e.g. a mid-history hole with a fresh tail cursor) that the age guard would MISS — and if so, is deferring precise
     day-level continuity detection acceptable for the minimal slice, or must it ship now?
   - Idempotency unchanged: only WHICH archives are opened changes; per-member resume/compat (`resume.rs`) untouched.
     Note the `>=` (inclusive) bound re-reads the cursor archive (members resume-Skip, no double-insert) but triggers
     LEGI `full_resume_backfill` (`legi.rs:237-243`); the design defers switching to strictly-after as a pure
     efficiency tweak — confirm `>=` is correctness-safe.
6. **Anything the design MISSED** that would make steady-state delta-only unsafe or the full-scan trigger wrong
   (e.g. forced-rebaseline repair path, dry-run, a source in the group with no fetch cursor, enrich/embed assuming a
   full ingest ran, the publish/change_seq delta computation depending on ingest having re-walked the baseline).

## Deliver
GO / GO-with-adjustments / NO-GO on the design, with the specific corrected predicate/edge-case if mine is wrong,
grounded in the source. Especially adjudicate #3 (cursor can't skip a delta), #5 recovery + retention-gap guard, and #6.

## Answer

# Design Validation: Delta-Only Steady-State Ingest For `producer update`

Verdict: **GO-with-adjustments**.

The broad design is right: `producer update` already decides whether a source has a pending new DILA baseline before ingest, and the pipeline already has an `ArchiveSyncFilter { incremental: true, since_compact: Some(..) }` path that skips the baseline tar and scans only selected deltas. But the proposed DB cursor helper is **not sufficient as stated**. A raw `max(timestamp)` over `ingest_member` rows with `status IN ('inserted','skipped')` can advance past an archive/run that had failed members, because the ingest code records member-level failures but still returns an `IngestReport` instead of an `Err` for those cases.

Ship this with a stricter cursor and a fail-closed ingest-status check.

## Source Findings

`run_update_inner` fetches first, then acquires the single `update-core` lock, opens the writer DB, computes `new_baselines`, computes `do_rebaseline`, and only then enters the source ingest loop:

- `update.rs:204-216`: fetch runs outside the DB-mutating lock.
- `update.rs:253-296`: under the lock, it computes `new_baselines` from `forced_rebaseline_baselines` or `group_run_kind`, then sets `do_rebaseline`.
- `update.rs:300-304`: it then calls `ingest_one` once per source.
- `update.rs:326-343`: only after ingest/enrich/embed does the rebaseline path publish and call `adopt_new_baselines`.

So claim 1 is correct: no phase reordering is needed. `ingest_one` can be passed `&new_baselines`, and membership of `(source_token, baseline_file_name)` is the same source-level signal that drives `run_rebaseline_cycle`.

The per-source baseline predicate is authoritative for this producer:

- `baseline.rs:116-125`: `baseline_decision` compares the fetched cursor baseline with the adopted marker.
- `baseline.rs:163-181`: `group_run_kind` returns every source with `RebaselinePending`.
- `update.rs:270-285`: forced rebaseline uses `forced_rebaseline_baselines`; automatic rebaseline uses `group_run_kind`.
- `update.rs:337-343` and `baseline.rs:68-95`: adoption markers are written only after rebaseline publish.

The filter path exists:

- `ingest/mod.rs:20-28`: `ArchiveSyncFilter` carries `incremental` and `since_compact`.
- `ingest/mod.rs:88-105`: `select_archives_to_process` pushes `plan.baseline` only when `!filter.incremental`; deltas are included when `since_compact` is absent or `delta.timestamp.compact() >= since`.
- `cli/src/ingest.rs:67-71`: CLI `sync` already uses `ArchiveSyncFilter { incremental: true, since_compact: ... }`.

Thus `incremental:true + since_compact:Some(cursor)` does not open the baseline tar. It selects only deltas at or after the cursor, preserving planner order.

## Required Adjustment: Cursor Source

Do **not** implement `latest_ingested_archive_compact` as:

```sql
MAX(parsed_timestamp(archive_name))
FROM ingest_member
WHERE source = $1
  AND status IN ('inserted', 'skipped')
```

That is unsafe as an archive cursor.

Why: `ingest_member` is member-level accounting, not archive-completion accounting. The code records `Inserted`, `Skipped`, and `Failed` per member:

- `members.rs:5-23`: member statuses include `inserted`, `skipped`, and `failed`.
- `resume.rs:86-93`: only `Inserted | Skipped` are resume-skipped; `Failed`, `Discovered`, and `Parsed` are retried.
- `legi.rs:464-495` and `604-651`: LEGI compatibility/parse failures record `IngestMemberStatus::Failed` and increment `failed_members`.
- `juri.rs:395-426` and `506-534`: JURI does the same.
- `legi.rs:265-299` and `juri.rs:215-250`: a run with `failed_members > 0` is marked `run_status=failed`, but if there is no fatal read/flush error the function still reaches `Ok(IngestReport { run_status: Failed, ... })`.
- `update.rs:437-443`: `ingest_one` currently does not check `report.run_status`; it returns an ingest journal coordinate anyway.

Concrete failure mode: an archive `D1` has a member parse failure, the run continues to later archive `D2`, and `D2` has `inserted` or `skipped` members. A max over terminal member rows returns `D2`. A future delta-only run with `since_compact=D2` skips `D1`, so the failed `D1` member is no longer retried. The current full-scan behavior would re-open `D1`.

### Corrected Cursor

Use a DB-authoritative **completed ingest run** cursor, not a raw member max.

Recommended helper:

```text
latest_completed_ingest_archive_compact(db, source)
  = max(manifest.freshness.latest_archive_timestamp_compact)
    from ingest_run
    where source = $source
      and status = 'completed'
      and manifest->'freshness'->>'latest_archive_timestamp_compact' is not null
```

This is still DB state, not `RunRecord`, and it is tied to the ingest lifecycle:

- `ingest_run.status='completed'` is written only when `failed_members == 0` and no fatal error occurred.
- The final manifest stores `freshness.latest_archive_timestamp_compact` for the selected/latest processed archive (`legi.rs:60-79`, `juri.rs:48-72`).
- With producer’s `limit_members=None`, a completed run means every selected archive was read and every processed member reached `inserted` or `skipped`.

If you prefer to keep an `ingest_member`-based helper, it needs an extra safety condition: compute the latest status per `(archive_name, member_path)` and refuse to return a cursor that is ahead of any latest `failed`, `parsed`, or `discovered` row for that source. The completed-run cursor is simpler and less error-prone.

Also add an explicit producer-side guard:

```text
after ingest_archives(...):
  if report.run_status != IngestRunStatus::Completed {
      fail the update as ingest-failed / needs-attention
  }
```

Without that, the producer can publish after a member-failed ingest run today. Delta-only cursoring makes that existing weakness more consequential.

## Corrected Decision Predicate

The source-level mode predicate should be:

```text
full_scan = new_baselines.iter().any(|(tok, _)| tok == source.as_str())

if full_scan:
    incremental = false
    since_compact = None
else:
    cursor = latest_completed_ingest_archive_compact(db, source)
    if cursor is None:
        incremental = false
        since_compact = None
    else if cursor_is_stale(cursor):
        Err(IngestCursorStale)
    else:
        incremental = true
        since_compact = Some(cursor)
```

This stays per-source. In a multi-source group, a source with a pending baseline full-scans; other sources in the same rebaseline run can use their completed ingest cursor. The subsequent `rebaseline_cycle` is still consistent because it snapshots the full current DB state, not the set of archives opened in that run.

For forced rebaseline, the same predicate is acceptable because `forced_rebaseline_baselines` includes every group source that has a fetched baseline (`update.rs:637-654`). A source with a fetched baseline full-scans; a source without one has nothing to re-anchor to and can fall back through the cursor/None logic.

## Recovery Semantics

New-baseline recovery is as described:

- Adoption is after publish only (`update.rs:337-343`, `baseline.rs:68-95`).
- If a rebaseline run fails before adoption, fetched != adopted remains true; `group_run_kind` will put that source back in `new_baselines`, so the next run full-scans it again.

Ordinary incremental recovery needs the adjustment above:

- If ingest completed and a later step fails, the completed DB ingest-run cursor covers the ingested deltas, so the next run can delta-only resume safely.
- If ingest itself ended with member failures, the cursor must **not** advance past the last completed run. The inclusive `>=` bound then re-opens the last completed archive and all later deltas, letting member resume skip already-complete members and retry failed/unfinished ones.

This is especially important for the live “embed failed after ingest” case: if the ingest run was completed before embed failed, the completed-run cursor should cover it and delta-only resume is safe.

## Retention Gap Guard

Accepted archives are retained indefinitely by the producer retention tooling:

- `retention.rs:1-8`: retention only reclaims temporary, partial, and quarantined files; accepted official archives are retained.
- `retention.rs:135-183`: inside the mirror, only `.part` sidecars are deletable, not accepted `.tar.gz` archives.

So a continuously fetching producer keeps its local archive chain. The real server-retention risk is when fetching stops long enough that missing deltas age off DILA before download.

A 45-day stale-cursor guard is a reasonable fail-closed first slice under an observed ~62-day DILA retention window, but source support is not quite “already available”: `timestamp.rs` has `now_unix()` and RFC3339 conversion, not a compact `YYYYMMDDHHMMSS -> unix` helper. Add a small `unix_from_compact_archive_timestamp` helper or equivalent pure parser, with tests. Do not try to shoehorn this through `unix_from_rfc3339`.

There is a subtler gap the age check does **not** catch: a fresh tail cursor with a mid-history hole. Example: an archive is integrity-quarantined, fetch continues and accepts later deltas, then the quarantined archive ages off the upstream listing. The cursor age may be fresh, but the local delta chain is not contiguous.

That gap is not introduced by delta-only ingest; the current full-scan path over the local mirror would also miss a local archive that was never accepted. But since this change is explicitly adding fail-closed gap handling, I would add a cheap guard now:

- Before delta-only ingest, scan `state_dir/quarantine/<source>` for recognized archive names.
- If a quarantined delta belongs to the current selected baseline era and its compact timestamp is `<=` the intended tail/fetch cursor, fail with `IngestCursorStale` or a clearer `IngestCursorGap`.
- Ignore quarantined archives at or before the selected baseline timestamp.

Precise day-level continuity detection can be deferred because DILA delta cadence may not be exactly daily and the local code has no authoritative expected-calendar model. But a recognized quarantined delta in the active era is concrete evidence of a hole and should fail closed.

## Inclusive `>=`

Keeping `>=` is correctness-safe.

`select_archives_to_process` includes deltas whose compact timestamp is equal to the cursor (`ingest/mod.rs:96-101`). Re-reading the cursor archive is safe because resume uses the latest member accounting:

- completed prior members return `IngestResumeAction::Skip` (`resume.rs:86-89`);
- failed/unfinished prior members return `Retry` (`resume.rs:90-93`).

For LEGI, this may trigger `full_resume_backfill` when compatible members are skipped (`legi.rs:237-243`), so it is an efficiency cost. It is not a data-loss risk. Switching to strictly-after can be a later optimization once the cursor semantics are proven.

## Other Edge Cases

Cold DB / first run:

- With no completed ingest-run cursor, fall back to full scan.
- On a normal first fetch, fetched baseline != adopted `None`, so the source is also in `new_baselines` and full-scans anyway.
- If there is no baseline in the mirror, `plan_from_dir` fails with `MissingBaseline` (`planner.rs:96-107`), which is the right fail-closed behavior.

Dry run:

- `run_update_inner` returns before DB open/lock/ingest (`update.rs:218-241`), so this design does not affect dry-run behavior.

Manual baseline mode:

- If a new baseline is pending and auto-rebaseline is disabled, `ensure_incremental_may_proceed` runs before ingest (`update.rs:287-290`) and refuses the ordinary incremental. The delta-only path does not bypass that guard.

Enrich/embed/publish:

- `embed_pending` processes pending chunks/zone units; it does not require a full archive scan (`update.rs:319-323`, `493-527`).
- Publish computes package deltas from `package_change_log` / `change_seq`, not archive cursors. The package builder is independent of whether ingest opened old baseline members again.
- Skipping old archives suppresses pointless resume-skips and avoids re-walking the baseline; it does not remove already-emitted outbox rows.

## Implementation Notes

Recommended public surface:

- Add `latest_completed_ingest_archive_compact_with_client(client, source)` in storage ingest accounting, or producer if you want to keep it private initially.
- Parse and validate compact strings with `ParsedArchive::parse_file_name(source, archive_name)` only where deriving from archive names. If using `ingest_run.manifest.freshness.latest_archive_timestamp_compact`, still validate shape before trusting it.
- Add `ProducerError::IngestCursorStale { source, cursor, max_age_days }` or similar. Class it as a needs-attention hard failure, not a success/no-op.
- Add the `report.run_status == Completed` check in `ingest_one`.
- Unit-test the pure mode selector with:
  - source in `new_baselines` -> full;
  - no cursor -> full;
  - fresh cursor -> incremental since cursor;
  - stale cursor -> error;
  - multi-source `new_baselines` -> only matching source full.
- Add a storage/helper test showing a failed ingest run does not advance the cursor past the previous completed run.

## Final Verdict

**GO-with-adjustments.**

The phase ordering, per-source new-baseline signal, and existing `ArchiveSyncFilter` delta-only path are sufficient. The design must be adjusted so the cursor is based on completed DB ingest runs, or equivalently on member accounting plus a latest-status/no-failed validation. Also add a producer-side fail on `IngestReport.run_status != Completed`, and add the stale-age plus active-quarantine gap guards before enabling steady-state delta-only ingest.
