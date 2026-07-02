# Design Validation: DB-Snapshot-Only Rebaseline And Cursor Seed

## Verdict: GO-with-adjustments

The core idea is sound: a `--from-db` rebaseline can safely publish the current producer `public.*` data as a new full media root without fetch/ingest/enrich/embed. The package-build path is already a pure DB snapshot once it is entered.

The proposed design needs four adjustments before I would call it production-safe:

1. Do not make `--from-db` depend only on `forced_rebaseline_baselines()`. That helper reads `FetchCursor.baseline_file_name`, not adopted-baseline markers. For a mirror-independent repair, derive the rebaseline label/adoption set from fetch cursor if present, else from `AdoptedBaseline`, or accept an explicit `--baseline-id`.
2. Run the cursor seed only after the rebaseline publish succeeds, while still holding the update lock. Do not advance the completed-ingest cursor before the package exists.
3. Be explicit that the ingest cursor seed does not seed the fetch cursor. The next normal timer may still download any currently-listed DILA archives not in `fetch-cursor-*.json`; it will skip pre-seed deltas at ingest time, but fetch itself is not bounded to "only after now".
4. Add the coverage/schema preflight, but expose it as a package-build helper with a rebaseline-neutral name. `bootstrap_preflight` is currently private and only wired into first-baseline bootstrap.

With those corrections, `--from-db` is a good minimal repair path for "publish current DB state as a fresh rebaseline generation, accept the DILA archive gap, resume future delta-only ingest from a declared anchor".

## 1. DB-Snapshot Publish Core

Confirmed. Once `run_rebaseline_cycle` is called, the package builder does not read archive files or ingest journals.

The call chain is:

- `run_rebaseline_cycle` builds `RebaselineCycleConfig` and calls `rebaseline_cycle` (`crates/jurisearch-producer/src/update.rs:702-724`).
- `rebaseline_cycle` stages a fresh rebaseline and calls `build_rebaseline` (`crates/jurisearch-package-build/src/cycle.rs:302-310`).
- `build_rebaseline_locked` reads the latest catalog head via `latest_package_for_corpus`, derives `from_sequence`, `to_sequence`, `package_id`, generation, and low-water mark, then delegates to `build_media_package` (`crates/jurisearch-package-build/src/baseline.rs:223-257`).
- `build_media_package` opens one `REPEATABLE READ`, read-only transaction, freezes `change_seq_high`, computes table digests, and copies every replicated table from the producer DB snapshot (`baseline.rs:276-309`).
- The table payload is streamed through `COPY (...) TO STDOUT (FORMAT binary)` into `BufWriter<File>` via `tee_digest`, with no multi-GB materialization in Rust (`baseline.rs:314-330`).

`build_rebaseline_locked` errors if there is no previous package head: it calls `latest_package_for_corpus(db, corpus)?.ok_or_else(...)` before deriving the rebaseline (`baseline.rs:223-229`). With `core-1-2` present, that precondition is satisfied.

The replicated table list is data-only: `documents`, `chunks`, `chunk_embeddings`, `graph_edges`, LEGI metadata, zone tables, citation tables, and official API responses (`crates/jurisearch-storage/src/generations.rs:71-86`). Operational tables such as `ingest_run`, `ingest_member`, `index_manifest`, and `package_change_log` are explicitly non-generation tables (`generations.rs:88-97`).

## 2. `--from-db` Skip-Phase Design

The skip-phase approach is correct, with one important source-of-baseline adjustment.

Current `run_update_inner` always fetches/read-fetches, then ingests every source, then enriches, then embeds, before publish (`crates/jurisearch-producer/src/update.rs:207-340`). Adding `snapshot_only: bool` to `UpdateOptions` and a `--from-db` flag on `Command::Rebaseline` is the right shape because this is a different operational contract, not merely `--skip-fetch`.

The existing CLI proves where this belongs:

- `Command::Rebaseline` currently accepts `config`, `source`, `dry_run`, `skip_fetch`, and `skip_enrich` (`crates/jurisearch-producer/src/bin/jurisearch_producer.rs:332-338`).
- It resolves `--source` to a group via `group_for_source` and then creates `UpdateOptions::rebaseline(group)` (`bin/jurisearch_producer.rs:339-368`; `crates/jurisearch-producer/src/config.rs:420-437`).
- `UpdateOptions` currently has `group`, `dry_run`, `skip_fetch`, `skip_enrich`, `lock_wait`, and `force_rebaseline` (`update.rs:58-71`).

The guarded phases should be:

- Phase 1: for `snapshot_only`, read fetch cursors only; do not network fetch.
- Phase 2/3: skip the ingest loop entirely.
- Phase 4: set `EnrichmentMode::Disabled`; do not call `enrich_group`.
- Phase 5: skip both `embed_pending` calls.
- Phase 6: run `run_rebaseline_cycle(config, &db, run_id, EnrichmentMode::Disabled, &new_baselines)`.

Skipping those phases leaves no required in-process state unset for package build. `run_rebaseline_cycle` only needs config, DB handle, run id, enrichment mode, and a baseline-label input (`update.rs:702-724`). `build_rebaseline` reads the current DB and catalog head itself.

Empty `ingest_journals` are safe for the report shape. `RunRecord` stores `ingest_journals: Vec<IngestJournalCoordinate>` with a default empty vector (`crates/jurisearch-producer/src/runrecord.rs:55-66`), and the current dry-run path already returns empty journals (`update.rs:230-244`). The `any_full_scan` computation only matters if embedding runs; in the snapshot-only branch embedding should not run.

### Required Adjustment: Baseline Source For `--from-db`

The proposed text says `forced_rebaseline_baselines` will be non-empty because the adopted July 2025 baselines exist. That is not what the source does.

`forced_rebaseline_baselines` loads `FetchCursor` and uses `cursor.baseline_file_name` (`crates/jurisearch-producer/src/update.rs:775-787`). It does not read `AdoptedBaseline`.

`AdoptedBaseline` is a separate marker stored in `adopted-baseline-<source>.json` (`crates/jurisearch-producer/src/baseline.rs:29-49`). The normal baseline decision compares fetch cursor baseline to adopted baseline (`baseline.rs:114-126`).

For `--from-db`, a missing or stale fetch cursor should not block a mirror-independent DB snapshot. Use one of these policies:

- Preferred: build a `snapshot_only_baseline_set` that uses `FetchCursor.baseline_file_name` when present, else falls back to `AdoptedBaseline.baseline_file_name`; error only if neither exists for any source.
- Also acceptable: add `--baseline-id <id>` and allow `new_baselines` to be empty for `--from-db`, skipping `adopt_new_baselines`.

If you keep the current `forced_rebaseline_baselines` precondition unchanged, `--from-db` can still fail `NothingToRebaseline` on a host whose adopted markers are present but fetch cursor is empty. That contradicts the purpose of this repair.

## 3. Package, Generation, And Supersession

Confirmed with current head sequence 2:

- `build_rebaseline_locked` sets `from_sequence` to the latest catalog package sequence and `to_sequence = from_sequence.next()` (`crates/jurisearch-package-build/src/baseline.rs:238-244`).
- It sets `package_id = "{corpus}-{from}-{to}"`, so head 2 yields `core-2-3` (`baseline.rs:240-245`).
- It increments the generation counter from the previous generation, so `core_g0001` becomes `core_g0002` (`baseline.rs:245-249`).
- It sets `included_change_seq_low` to the previous package's `included_change_seq_high` (`baseline.rs:250-253`).
- `build_media_package` inserts a catalog row with `package_sequence = to_sequence`, kind `rebaseline`, and `included_change_seq_high = change_seq_high` frozen from the snapshot (`baseline.rs:475-501`).

The remote manifest will select the newest media row, where media means `baseline` or `rebaseline` (`crates/jurisearch-package-build/src/remote_manifest.rs:93-104`). It includes only incrementals after that media sequence (`remote_manifest.rs:120-129`). With no sequence > 3 incrementals, `head_sequence = 3` and `min_available_sequence = 3` (`remote_manifest.rs:156-166`), and the catch-up range below 3 becomes `RequiresBaseline` (`remote_manifest.rs:168-179`).

Old `core-1-1` and `core-1-2` artifacts are retained. `publish_package` creates a new immutable package directory and refuses to overwrite an existing different artifact (`crates/jurisearch-package-build/src/publish.rs:40-80`). Nothing in `rebaseline_cycle` deletes older published package directories (`crates/jurisearch-package-build/src/cycle.rs:319-342`).

Served manifest replacement is atomic: `publish_remote_manifest` writes `manifest.json.tmp` and renames it over `manifest.json` (`publish.rs:91-109`).

### Crash Safety Caveat

The served `core-1-2` manifest remains intact until the new remote manifest is published. That part is safe.

However, the existing `rebaseline_cycle` is not a perfect self-healing retry for every crash point. If the process crashes after `publish_package` creates `packages/core-2-3/` but before `mark_package_published` or remote-manifest publish (`cycle.rs:319-342`), the old served manifest still points at `core-1-2`, but an orphaned `core-2-3` directory may block a later rebuild because package ids are immutable and a new run's manifest bytes can differ. This is an existing rebaseline-cycle limitation, not introduced by `--from-db`. Operationally, the failure mode is fail-closed for clients, but retry may require deleting the unreferenced orphan after a source-grounded repair check.

## 4. Coverage Preflight

Confirmed: `rebaseline_cycle` does not call `bootstrap_preflight`.

`bootstrap_preflight` is only used by the first-baseline bootstrap path before `build_baseline` (`crates/jurisearch-package-build/src/cycle.rs:624-647`). The function itself is private and checks:

- producer schema version equals `CURRENT_SCHEMA_VERSION` (`cycle.rs:821-836`);
- every chunk has a matching `chunk_embeddings` row under fingerprint/model/dimension (`cycle.rs:838-862`);
- every chunk's `chunks.embedding_fingerprint` equals the configured fingerprint (`cycle.rs:863-875`);
- every zone unit has a matching current embedding (`cycle.rs:877-897`);
- every zone unit fingerprint is consistent (`cycle.rs:898-910`).

Adding a public helper such as `preflight_media_snapshot` or `rebaseline_preflight` in `jurisearch-package-build` is the right guard. I would call it for `--from-db` before `run_rebaseline_cycle`; it would also be defensible to call it for all rebaseline cycles, but the minimal required slice is snapshot-only because that path deliberately skips embed/finalize.

This preflight should not wrongly reject the stated clean DB if:

- `schema_migrations` is current;
- all 4,792,053 chunks have `bge-m3:1024:normalize:true`;
- all chunk embeddings exist with model `bge-m3`, dimension `1024`, fingerprint `bge-m3:1024:normalize:true`;
- zone unit coverage is either empty or fully current.

If zone-unit fingerprints are not clean, the preflight will reject. That is desirable because client readiness uses both projection and embedding coverage.

## 5. Cursor Seed

The ingest cursor seed mechanism is correct for the stale-cursor guard, but the proposed ordering and fetch-side expectation need correction.

### What The Cursor Query Reads

Confirmed. `latest_completed_ingest_archive_compact_with_client` returns:

```sql
SELECT max(manifest->'freshness'->>'latest_archive_timestamp_compact')
FROM ingest_run
WHERE source = $1
  AND status = 'completed'
  AND manifest->'freshness'->>'latest_archive_timestamp_compact' IS NOT NULL
  AND COALESCE((manifest->'freshness'->>'member_limited')::boolean, false) = false;
```

Source: `crates/jurisearch-storage/src/ingest_accounting/runs.rs:204-231`. The comments explicitly say this is the producer's delta-only cursor and excludes member-limited runs (`runs.rs:176-199`).

A synthetic completed `ingest_run` with:

```json
{
  "freshness": {
    "latest_archive_timestamp_compact": "YYYYMMDDHHMMSS",
    "member_limited": false
  },
  "kind": "cursor_seed",
  "note": "operator accepted DILA retention gap; DB-snapshot rebaseline published"
}
```

will advance the cursor.

`choose_ingest_mode` then sees `Some(cursor)` and, if it is under the 45-day stale threshold, returns `incremental=true` and `since_compact=Some(cursor)` (`crates/jurisearch-producer/src/update.rs:451-492`). The next normal ingest will skip the baseline and select only deltas whose archive compact timestamp is `>= since` (`crates/jurisearch-pipeline/src/ingest/mod.rs:91-109`).

### Memberless Completed Run Is Harmless

The schema does not require an `ingest_run` to have members. `ingest_member.run_id` references `ingest_run`, but the FK is only child-to-parent (`crates/jurisearch-storage/src/migrations.rs:139-176`).

The health view tolerates a memberless latest run. It reads the latest `ingest_run` by `started_at`, then counts members with `WHERE ($1::text IS NULL OR run_id = $1)` (`crates/jurisearch-storage/src/ingest_accounting/health.rs:47-103`). A seed run will show zero members and no failed-member warning. That is observability noise, not a correctness break. Make the manifest explicit so operators can recognize it.

Using `start_ingest_run_with_client`, `update_ingest_run_manifest_with_client`, and `finish_ingest_run_with_client` is acceptable. `start_ingest_run_with_client` invalidates the query-readiness cache by deleting `index_manifest['query_readiness']` (`runs.rs:55-96`; `readiness.rs:179-188`). That is conservative and harmless; the next readiness check recomputes. If you want to avoid that cache invalidation, add a dedicated seed helper, but correctness does not require it.

### Required Ordering Adjustment

Seed after successful rebaseline publish, still under the update-core lock.

Do not seed before Phase 6. If publish fails, a pre-publish seed would make the next normal timer look fresh and delta-only even though no new rebaseline was published. The intended invariant should be:

1. preflight current DB;
2. publish `core-2-3`;
3. publish/replace remote manifest;
4. adopt/re-adopt baseline markers;
5. insert cursor-seed completed runs;
6. finish the producer run record.

The seed does not affect the rebaseline package window. It does not emit `package_change_log` rows, and `ingest_run` is not a replicated generation table (`crates/jurisearch-storage/src/generations.rs:71-97`). Therefore seeding after publish does not create a missing package delta.

### Fetch Cursor Is Separate

The statement "the next daily fetch downloads only NEW deltas" is not true unless the fetch cursor is also handled.

Fetch selection is based on `FetchCursor.fetched.contains_key(file_name)`, not on the completed ingest cursor (`crates/jurisearch-fetch/src/cursor.rs:44-55`, `cursor.rs:117-153`; `crates/jurisearch-fetch/src/engine.rs:112-138`). A normal timer still runs fetch before ingest (`crates/jurisearch-producer/src/update.rs:207-214`).

So after seeding only `ingest_run`, the next timer may download every currently-listed DILA archive not already present in the fetch cursor, including retained pre-anchor deltas. Ingest will then skip pre-anchor deltas because `since_compact = seed_now`, but fetch may still cost time and bandwidth once.

This is not a data-corruption problem. It is an operational expectation problem. If "no re-fetch backlog" is a hard requirement, add a separate fetch-floor/fetch-cursor design. I would not forge `FetchCursor.fetched` entries casually because `FetchCursor::record` is documented as "strictly after verify_targz" and stores sha256/size metadata (`crates/jurisearch-fetch/src/cursor.rs:129-153`). A safer future design would add an explicit per-source "ignore remote archives older than accepted gap anchor" floor rather than pretending files were fetched.

### Seed Subcommand Or Folded Into `--from-db`

For this repair, fold the seed into `rebaseline --from-db` behind an explicit flag name in the JSON output, and run it after publish. The two operations are semantically coupled: the cursor anchor is only safe because a fresh full baseline has just re-anchored clients to the accepted DB state.

If you also want a general operator tool, add a separate `seed-ingest-cursor --group <g> --compact <ts> --reason <text>` later. It should require an explicit reason and probably refuse to run unless a recent media rebaseline exists, because this is an intentional data-gap acceptance operation.

## 6. Blast Radius, Memory, And Safety

Memory: bounded. The full media payload is streamed table-by-table through `COPY ... FORMAT binary` and `tee_digest` (`crates/jurisearch-package-build/src/baseline.rs:314-330`). Expect Storebox/CIFS I/O roughly in the prior full-baseline class, around 150+ GB payload, not an incremental-sized artifact. Wall time is likely hours, dominated by DB COPY, digest pass, CIFS writes, and manifest/package copy.

No re-projection: confirmed if phases 2/3 are skipped. The chunk fingerprint preservation fix is not exercised because no ingest projection runs.

Locks: the producer update-core lock still serializes the orchestration (`crates/jurisearch-producer/src/update.rs:256-259`). The package builder also takes the corpus build lock in `build_rebaseline` (`crates/jurisearch-package-build/src/baseline.rs:188-199`).

Served feed: old artifacts are retained; the remote manifest is only replaced after the new package exists and is marked published (`crates/jurisearch-package-build/src/cycle.rs:319-342`; `publish.rs:91-109`). A failure before manifest publish should leave clients seeing the old `core-1-2` head, though see the crash-safety caveat above for orphan cleanup.

Legislation: the new rebaseline is full `core`, so it republishes legislation plus jurisprudence from current DB state. It does not mutate legislation tables if ingest/embed are skipped.

Cursor seed: producer-local accounting only. It mutates `ingest_run` and invalidates query-readiness cache; it does not alter replicated data tables, change-log rows, package payloads, or served artifacts.

## 7. Missed Risks / Required Tests

Add tests for:

- `rebaseline --from-db --dry-run` does not fetch and reports planned baseline label source.
- `--from-db` with empty fetch cursor but adopted markers present still works, if you implement the adopted fallback.
- `--from-db` skips `ingest_archives`, `enrich_group`, and both embed targets.
- `--from-db` calls media preflight and rejects missing/inconsistent chunk or zone embeddings.
- successful `--from-db` from head 2 publishes `core-2-3`, generation `core_g0002`, active baseline kind `rebaseline`, manifest head/min sequence 3.
- cursor seed row advances `latest_completed_ingest_archive_compact_with_client` and is ignored if `member_limited=true`.
- seed happens after publish; inject a publish failure and assert no seed row is written.
- next normal `update --group jurisprudence` chooses delta-only, not stale, after seed.

## Bottom Line

GO-with-adjustments.

`--from-db` is the right repair primitive for this state because the rebaseline package builder is already a pure, streamed DB snapshot once entered. The design must not rely on archive/mirror state for the snapshot-only baseline label, must run a media coverage preflight, must seed completed-ingest cursors only after a successful publish, and must not claim the ingest seed also suppresses future fetch backlog. With those changes, it gives jurisprudence a fresh full media root and lets future delta-only timers resume from an explicit, auditable gap-acceptance anchor.
