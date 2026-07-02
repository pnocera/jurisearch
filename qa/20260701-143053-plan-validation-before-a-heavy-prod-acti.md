# Q&A — 20260701-143053

## Question

# Plan validation (BEFORE a heavy prod action) — first manual `update --group jurisprudence` run

Repo `/home/pierre/Work/jurisearch`. **Read the real source; don't trust this prose.** Pre-action gate. I will NOT
execute until you validate. The producer binary deployed is `b3ee839` (all OOM/streaming fixes + delta-only ingest +
replay-snapshot-skip). Timers DISABLED. Legislation `core-1-2` is published (head_sequence 2); we just fixed its
fingerprints. Now the user wants to run the jurisprudence group manually before re-arming timers.

## Live jurisprudence state (read-only verified)
- Group `jurisprudence`, sources: cass, inca, capp, jade.
- `rebaseline_pending=false`; `last_outcome=None` (NEVER run through the pipeline).
- Per-source baseline markers: adopted == fetched == `Freemium_<src>_global_20250713-140000.tar.gz`, state `current`.
- Per-source `fetch_cursor.latest_file_name = None` (nothing fetched).
- **Archive mirror is EMPTY: 0 archives in `/srv/jurisearch/storebox/archives/{cass,inca,capp,jade}/`.**
- Un-embedded chunks total = 0 (all embedded after the fingerprint fix). The jurisprudence corpus is already in the
  DB from the original hand-load.
- No completed jurisprudence ingest run exists (so the delta-only cursor from commit 60b46c5 is None → full scan).

## Questions to adjudicate against source
1. **What does `jurisearch-producer update --group jurisprudence` actually do given this state?** Trace fetch →
   ingest → enrich → embed → publish for jurisprudence. Specifically:
   - **Fetch:** with adopted==fetched markers already at 20250713 but the mirror EMPTY, does fetch re-download the
     global baselines + deltas (because the archives are absent from the mirror), or does it skip because the cursor
     says already-adopted? (`crates/jurisearch-fetch/src/engine.rs` selection logic.) How much data (~4 globals + all
     deltas since 20250713)?
   - **Ingest mode:** with no completed ingest run → `choose_ingest_mode` cursor=None → `incremental=false` (FULL
     scan). Confirm. So it re-walks the just-downloaded globals + deltas, idempotent vs the hand-loaded rows.
   - **Rebaseline vs incremental:** `rebaseline_pending=false` (adopted==fetched) → is a new baseline detected? If
     fetch downloads the SAME 20250713 global that's already adopted, does that make `new_baselines` non-empty
     (rebaseline) or empty (incremental)? i.e. will Phase 6 be `run_cycle` (incremental core-1-3) or
     `run_rebaseline_cycle`? This matters a lot.
   - **Enrich:** cass/inca get Judilibre enrichment (needs PISTE creds, which are present). Blast radius / time /
     rate-limit risk? Is `--skip-enrich` advisable for a first validation run, and what does skipping cost
     (zone enrichment deferred)?
   - **Embed:** 0 pending → NoResults (fast). Confirm the dense finalize (ivfflat rebuild) — will it run and is it
     safe now (CREATE granted, indexes owned by jurisearch_owner)?
   - **Publish:** does it produce `core-1-3` (an incremental over the jurisprudence change_log window since core-1-2),
     and could that window be huge (full re-ingest touching many rows → large change_log → large core-1-3, like the
     ~25 GB legislation core-1-2)? Memory bounded by the streaming fixes?
2. **Is this SAFE to run?** Blast radius: does it risk the served core-1-1/core-1-2 (staged-then-atomic publish)?
   Could the full re-ingest of jurisprudence corrupt or duplicate the hand-loaded rows (idempotency)? Any OOM risk
   given b3ee839 is deployed? Any risk from the empty-mirror + adopted-marker mismatch (e.g. it tries to rebaseline
   and discards something)?
3. **What's the LIGHTEST correct command** to validate the jurisprudence pipeline end-to-end without a surprise
   (full download + hours + Judilibre)? Options: full `update --group jurisprudence`; `--skip-enrich`;
   `--dry-run` first (config/fetch only). Recommend the safest sequence, with the expected time/data profile and the
   success signals to watch (published core-1-3? or no-op?).
4. Anything that would make this run FAIL or behave surprisingly (missing creds, the empty mirror, the
   adopted-but-not-in-mirror baseline, a rebaseline it shouldn't do).

## Deliver
GO / GO-with-adjustments / NO-GO on running jurisprudence now, the exact recommended command(s) + expected
behavior/time/data/memory profile, the success + abort signals, and whether to skip-enrich. Ground it in source + the
live facts.

## Answer

# Plan Validation: First Manual `update --group jurisprudence`

Verdict: **NO-GO for the full `update --group jurisprudence` right now**.

Read-only/dry-run and fetch-only probes are safe, but the first DB-mutating jurisprudence `update` is not safe on the stated state unless you first prove the relevant `ingest_member` rows already exist and will cause a compatible skip. With a hand-loaded DB and no completed jurisprudence ingest cursor, the producer resolves a full archive scan. If the per-member accounting is also absent, that full scan reprocesses every jurisprudence XML member, unconditionally upserts every decision scope, emits outbox changes for every decision, and, most importantly, writes `chunks.embedding_fingerprint = NULL` for those chunks. The later embed step will not repair that, because the chunk pending selector keys on `chunk_embeddings`, not on `chunks.embedding_fingerprint`, and returns `NoResults` before dense finalize.

That is enough to risk rebuilding and publishing a broken `core-1-3` analogous to the `core-1-2` fingerprint problem you just repaired.

## Source Findings

### Fetch

Fetch selection is cursor-driven, not adoption-marker-driven and not filesystem-reconciled. `Fetcher::plan` loads `FetchCursor`, lists the remote DILA directory, and skips only names for which `cursor.is_fetched(file_name)` is true (`crates/jurisearch-fetch/src/engine.rs:116`). It does not check whether the file is actually present in `/srv/jurisearch/storebox/archives/<source>/`.

`FetchCursorCoordinate.latest_file_name` is computed from `cursor.fetched`, not from `baseline_file_name` (`crates/jurisearch-producer/src/fetch.rs:40`). So your live `latest_file_name = None` strongly implies `cursor.fetched` is empty. If so, a real fetch will download every parsed listing entry for each source: the global baseline plus whatever deltas DILA currently lists for `cass`, `inca`, `capp`, and `jade`.

If the cursor is actually stale/non-empty despite the empty mirror, fetch would skip those cursor-recorded files and ingest would then fail at local archive planning if the baseline is missing. `plan_from_dir` requires a local baseline archive (`crates/jurisearch-ingest/src/archive/planner.rs:96`). That failure is fail-closed, but it means the dry-run plan must be inspected before running anything mutating.

### Rebaseline Routing

`rebaseline_pending=false` before fetch is not the final routing decision. `update` fetches first, then computes `new_baselines` under the update lock from the fetch cursor and adopted markers (`crates/jurisearch-producer/src/update.rs:207`, `:273`). `group_run_kind` compares `FetchCursor.baseline_file_name` to `adopted-baseline-<src>.json` (`crates/jurisearch-producer/src/baseline.rs:116`).

If fetch only downloads the same `Freemium_<src>_global_20250713-140000.tar.gz` that is already adopted, `new_baselines` stays empty and Phase 6 is ordinary `run_cycle`, not `run_rebaseline_cycle`.

If DILA now lists a newer global baseline for any jurisprudence source, fetch records it; `group_run_kind` will make `new_baselines` non-empty. In `auto-on-new-baseline` mode this routes to `run_rebaseline_cycle`; in manual mode it refuses ordinary incremental with `needs-rebaseline`. The source does not make “same adopted marker” a permanent guard after a new fetch.

### Ingest Mode

With no completed jurisprudence ingest run, `latest_completed_ingest_archive_compact_with_client` returns `None`; `choose_ingest_mode(..., cursor=None)` returns `incremental=false`, `since_compact=None` (`crates/jurisearch-producer/src/update.rs:451`). `select_archives_to_process` then includes the baseline first and all deltas (`crates/jurisearch-pipeline/src/ingest/mod.rs:91`).

That means the first mutating run is a full archive replay unless a source is blocked before ingest.

### The Blocking Safety Issue

Jurisprudence ingest calls:

```rust
insert_decision_documents_with_statements(..., None, Some(&outbox))
```

in `crates/jurisearch-pipeline/src/ingest/juri.rs:474`. That `None` is the `chunk_embedding_fingerprint`.

The shared projection statement writes chunks with that value and, on conflict, updates:

```sql
embedding_fingerprint = EXCLUDED.embedding_fingerprint
```

in `crates/jurisearch-storage/src/projection/legi.rs:79`.

So a replayed existing jurisprudence chunk is set back to `NULL` unless the member is skipped by ingest accounting before projection. The skip path depends on `ingest_member` compatibility, not just on rows already existing in `documents`/`chunks` (`crates/jurisearch-storage/src/ingest_accounting/resume.rs:35`; `crates/jurisearch-pipeline/src/ingest/juri.rs:397`).

Your live fact says no completed jurisprudence ingest run exists. That guarantees a cold completed-run cursor, but it does **not** by itself prove whether compatible `ingest_member` rows exist from a failed/partial historical run. If `ingest_member` rows are absent, the run processes every member as new.

Embed will not repair the null parent fingerprints. `load_chunk_embedding_inputs_with_client` selects pending rows only when the `chunk_embeddings` row is missing or has mismatched fingerprint/model/dimension (`crates/jurisearch-storage/src/dense.rs:89`). It does not check `chunks.embedding_fingerprint`. With current `chunk_embeddings` already correct, `embed_chunks_inner` returns `NoResults` before `finalize_dense_rebuild_with_client` (`crates/jurisearch-pipeline/src/embed.rs:212`). The producer treats `NoResults` as a successful no-op (`crates/jurisearch-producer/src/update.rs:654`). Therefore no dense finalize stamp runs and no ivfflat rebuild runs.

### Enrichment

`enrich_group` runs only for `cass` and `inca`; `capp` and `jade` are skipped (`crates/jurisearch-producer/src/update.rs:574`). With PISTE credentials present, it pages all eligible candidates with `limit=None`, `since=None`, and `order=Oldest` (`crates/jurisearch-producer/src/update.rs:592`; `crates/jurisearch-pipeline/src/enrich.rs:132`). That can be a long, externally rate-limited API job, and it emits outbox changes for archived responses / `decision_zones`.

For the first validation run, `--skip-enrich` is advisable. The cost is that official Judilibre zone enrichment for `cass`/`inca` is deferred; it does not make the package chain less valid. But `--skip-enrich` does not avoid the ingest/fingerprint problem above.

### Publish and Served Feed Safety

If a package build reaches Phase 6, ordinary publish is staged and atomic for the served root: `producer_cycle` builds in `.staging/pending`, publishes the package directory, marks the catalog row, then rebuilds and atomically renames the signed remote manifest (`crates/jurisearch-package-build/src/cycle.rs:150`). Existing `core-1-1` and `core-1-2` are not overwritten by a failed publish.

The risk here is producer DB/content correctness and a bad new `core-1-3`, not corruption of already-served seq 1 or seq 2.

## Expected Behavior If You Ran It Anyway

Assuming `cursor.fetched` is empty and DILA has no newer global baselines:

1. Fetch downloads the four jurisprudence baselines plus all currently listed deltas.
2. Routing remains ordinary incremental, not rebaseline, if the fetched baseline names still equal the adopted markers.
3. Ingest full-scans baseline plus deltas for each source.
4. If compatible `ingest_member` rows are absent, it re-upserts all jurisprudence decisions, emits package changes for all those document scopes, and nulls `chunks.embedding_fingerprint` for those chunks.
5. `--skip-enrich` skips Judilibre; without it, enrichment may run for a long time against `cass`/`inca`.
6. Embed likely returns `NoResults` for chunks and zone units if embedding tables are already current; no dense finalize runs.
7. Publish builds `core-1-3` from the `package_change_log` window after seq 2. If full reingest emitted changes for the whole jurisprudence corpus, this can be a very large incremental. Streaming fixes bound memory, but disk/time and client apply size can still be large.

## Safe Commands Now

Safe read-only smoke:

```bash
/usr/local/bin/jurisearch-producer update \
  --config /etc/jurisearch/producer.toml \
  --group jurisprudence \
  --dry-run \
  --skip-enrich
```

This lists fetch plans only; source shows dry-run exits after fetch/cursor reporting and never opens the DB, locks, ingests, embeds, or publishes (`crates/jurisearch-producer/src/update.rs:221`). Inspect that each jurisprudence source has `planned_or_downloaded` containing the expected baseline and no unexpected newer `Freemium_*_global_*`.

Even clearer per-source fetch plans:

```bash
for s in cass inca capp jade; do
  /usr/local/bin/jurisearch-producer fetch \
    --config /etc/jurisearch/producer.toml \
    --source "$s" \
    --dry-run
done
```

Safe but network/disk-heavy fetch-only mutation:

```bash
for s in cass inca capp jade; do
  /usr/local/bin/jurisearch-producer fetch \
    --config /etc/jurisearch/producer.toml \
    --source "$s"
done
```

This can download a large amount of data, but it does not touch the DB or served package root. Do it only if the dry-run shows `already_present` is not incorrectly hiding missing local files.

## Required Preflight Before Any Full Update

Check whether compatible ingest accounting exists:

```sql
SELECT source,
       count(*) AS members,
       count(*) FILTER (WHERE status IN ('inserted','skipped')) AS complete_members
FROM public.ingest_member
WHERE source IN ('cass','inca','capp','jade')
GROUP BY source
ORDER BY source;
```

If this is zero or far below the expected archive member count, a full update is unsafe for the reasons above.

Check current jurisprudence fingerprint coverage before and after any experiment:

```sql
SELECT d.source,
       count(*) AS chunks,
       count(*) FILTER (WHERE c.embedding_fingerprint IS NULL) AS null_chunk_fp
FROM public.chunks c
JOIN public.documents d ON d.document_id = c.document_id
WHERE d.source IN ('cass','inca','capp','jade')
GROUP BY d.source
ORDER BY d.source;
```

Expect `null_chunk_fp = 0`. If it rises during a run, stop before publish and repair; do not let a package build proceed.

## What To Fix Before End-to-End Jurisprudence Update

Pick one of these before the first full jurisprudence pipeline run:

1. **Code fix:** make full-scan ingest preserve/stamp the active storage embedding fingerprint for already-current chunks, or ensure changed existing chunks become pending by a selector that considers `chunks.embedding_fingerprint`. This needs careful design because `None` currently acts as an invalidation marker, but the embed selector does not consume that marker.
2. **Accounting repair:** seed accurate `ingest_member`/completed ingest-run accounting only if you can prove it exactly matches the current hand-loaded archive bytes and parser/build versions. This is risky and still requires the local archive chain to be present.
3. **Operational two-step:** fetch archives first, then run a deliberately bounded non-publishing ingest validation in a clone/staging DB to see whether member resume skips or reprocesses. Do not use production `update` as the first proof.

## Success and Abort Signals If/When Fixed

For a later safe run, use:

```bash
/usr/local/bin/jurisearch-producer update \
  --config /etc/jurisearch/producer.toml \
  --group jurisprudence \
  --skip-enrich
```

Success signals:

- output JSON has `"status":"ok"`, `"rebaselined":false`, `"built_incremental":"core-1-3"`;
- `package_catalog` has seq 3 `core-1-3`, status `published`, with `included_change_seq_low = 1445243`;
- served `core/manifest.json` has `head_sequence = 3`;
- jurisprudence `chunks.embedding_fingerprint` remains 0 NULL;
- `chunks_with_embeddings.replace_set.jsonl` in `core-1-3` has no `"embedding_fingerprint":null`.

Abort signals:

- dry-run shows a newer jurisprudence `Freemium_*_global_*` than the adopted 20250713 baseline unless you intend a rebaseline;
- dry-run shows `already_present` for files absent from the local mirror;
- `ingest_member` has no compatible coverage for the hand-loaded jurisprudence archives;
- production `chunks.embedding_fingerprint` for `cass`/`inca`/`capp`/`jade` becomes nonzero NULL during ingest;
- RSS or disk grows beyond your operational threshold, though memory should be bounded by `b3ee839` streaming fixes.

## Bottom Line

Do **not** run the full jurisprudence update yet. Run dry-run/fetch-only probes if desired, and use `--skip-enrich` for the first eventual validation. But a DB-mutating `update --group jurisprudence` is a **NO-GO** until you close the full-scan ingest fingerprint regression or prove, with `ingest_member`, that the archive replay will skip rather than reproject the hand-loaded corpus.
