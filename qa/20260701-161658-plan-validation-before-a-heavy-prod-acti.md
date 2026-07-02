# Plan Validation: `rebaseline` Jurisprudence

## Verdict: NO-GO for the proposed production run

Do not run `jurisearch-producer rebaseline --group jurisprudence` as stated.

There are two separate issues:

1. The CLI shape is wrong. The source accepts `rebaseline --source <src>`, not `--group`. The selected source is resolved to its fetch group by `ProducerConfig::group_for_source` (`crates/jurisearch-producer/src/bin/jurisearch_producer.rs:332-368`, `crates/jurisearch-producer/src/config.rs:420-437`).
2. More importantly, the data-loss/regression argument is not safe. `build_rebaseline` does snapshot the current producer DB, but the producer `rebaseline` command first runs fetch and a full ingest for every source in the group before it cuts that snapshot (`crates/jurisearch-producer/src/update.rs:207-340`). With an empty mirror and a stale/incomplete DILA delta chain, that ingest can either fail or project archive bytes that are not a complete path from the adopted July 2025 baseline to the DB's current Dec 2025 jurisprudence state.

The served feed publish path is staged and crash-safe, but the DB-mutating ingest phase is not a no-op proof. I would not run this on production until archive/cursor consistency is proven in a clone or the plan is changed to build a rebaseline from the existing DB state without re-ingesting stale/incomplete archives.

## What The Command Actually Does

The real command form is:

```bash
/usr/local/bin/jurisearch-producer rebaseline --config /etc/jurisearch/producer.toml --source cass
```

Any one of `cass`, `inca`, `capp`, or `jade` selects the `jurisprudence` group if the config maps it that way. There is no `--group` flag for `rebaseline`.

The dry-run form is supported:

```bash
/usr/local/bin/jurisearch-producer rebaseline --config /etc/jurisearch/producer.toml --source cass --dry-run
```

But note the implementation: the `rebaseline` dry-run returns the forced-baseline plan directly and does not call `fetch_source` (`crates/jurisearch-producer/src/bin/jurisearch_producer.rs:343-363`). It is useful to confirm group/source resolution and which fetched baselines would be adopted; it does not prove the mirror contains the archives that ingest will read.

For a real run, `UpdateOptions::rebaseline(group)` sets `force_rebaseline=true` (`crates/jurisearch-producer/src/update.rs:89-93`). `run_update_inner` then:

- fetches each source unless `--skip-fetch` is set (`update.rs:207-214`);
- computes forced `new_baselines` from the fetch cursors, bypassing the automatic "newer than adopted" gate (`update.rs:269-281`);
- sets `do_rebaseline=true` (`update.rs:295`);
- ingests every source before publish (`update.rs:301-307`);
- enriches unless skipped (`update.rs:319-325`);
- embeds pending chunks and zone units (`update.rs:329-334`);
- runs the rebaseline publish cycle (`update.rs:338-356`).

The stale-cursor guard does not block the forced rebaseline path. `choose_ingest_mode` returns `incremental=false` immediately when the source is in `new_baselines` (`update.rs:451-465`), so it does not consult or age the completed-ingest cursor in that branch.

## Fetch And Empty Mirror Risk

Fetch selection is cursor-based, not mirror-file-existence-based. `Fetcher::plan` loads the persisted `FetchCursor`, lists DILA, and skips any remote file whose name is already in `cursor.fetched` (`crates/jurisearch-fetch/src/engine.rs:112-138`). `Fetcher::run` only downloads `plan.to_fetch` and reports cursor-listed entries as `already_present` (`engine.rs:145-188`). The producer report exposes `planned_or_downloaded` and `already_present` from that cursor decision (`crates/jurisearch-producer/src/fetch.rs:96-132`).

That matters with the stated live fact "mirror is EMPTY". If the fetch cursor still claims archives were fetched, a real rebaseline may skip downloading files that are absent from `/srv/jurisearch/storebox/archives/{cass,inca,capp,jade}`. Then `ingest_juri_archives` plans from the local archive directory (`crates/jurisearch-pipeline/src/ingest/juri.rs:125-139`) and can fail before doing useful work because the mirror does not actually contain the baseline/delta chain.

Before any real run, check each source with:

```bash
/usr/local/bin/jurisearch-producer fetch --config /etc/jurisearch/producer.toml --source cass --dry-run
/usr/local/bin/jurisearch-producer fetch --config /etc/jurisearch/producer.toml --source inca --dry-run
/usr/local/bin/jurisearch-producer fetch --config /etc/jurisearch/producer.toml --source capp --dry-run
/usr/local/bin/jurisearch-producer fetch --config /etc/jurisearch/producer.toml --source jade --dry-run
```

If files are reported as `already_present` while absent from the mirror, the fetch cursor and mirror are inconsistent. Do not run the rebaseline until that is repaired under its own gate.

## Package Outcome If It Reaches Publish

The rebaseline package is not jurisprudence-scoped. It is a full media package for the whole `core` corpus.

`rebaseline_cycle` explicitly builds from the current locked DB state and last published head (`crates/jurisearch-package-build/src/cycle.rs:302-310`), then publishes the artifact, marks the row published, clears staging, and rebuilds the remote manifest (`cycle.rs:319-342`).

`build_rebaseline_locked` reads the latest catalog row and derives the rebaseline over that head (`crates/jurisearch-package-build/src/baseline.rs:215-223`). With current head sequence 2, the package would be the next full media root, expected as `core-2-3`, with `from_sequence=2`, `to_sequence=3`, and a new generation. It does not reset sequence numbering to 1.

The copy-out itself is a full snapshot of the producer `public` tables, not a package derived directly from archive files. `build_media_package` is the shared full media builder, and the comments identify rebaseline as a result that "already lives in the producer's public" (`baseline.rs:178-179`). The builder reads producer table digests and streams table payloads; the first-baseline preflight is separate (`crates/jurisearch-package-build/src/cycle.rs:624-647`, `cycle.rs:810-910`) and is not the rebaseline path.

After publish, `build_remote_manifest` will make the new rebaseline the active baseline. Old `core-1-1` and `core-1-2` artifacts are not deleted by this path, but the served manifest will no longer use them as the active chain head. The manifest publish is staged through the normal package publish and manifest publish sequence (`cycle.rs:319-342`).

## Critical Data-Loss Assessment

The claim "the build snapshots current DB, so Dec 2025 data is preserved" is incomplete. The source shows this order:

1. fetch;
2. full ingest for each jurisprudence source;
3. enrich/embed;
4. rebaseline package build from DB.

So the build snapshots the DB *after* the full ingest.

The JURI ingest path does have compatibility-based resume. For each archive member, `process_juri_archive_member` computes compatibility from parser/schema/code/source payload hash and calls `ingest_resume_decision_with_client` (`crates/jurisearch-pipeline/src/ingest/juri.rs:384-397`). A compatible prior `Inserted` or `Skipped` member returns `Skip` and exits before projection (`juri.rs:397-417`; `crates/jurisearch-storage/src/ingest_accounting/resume.rs:35-103`).

That helps only if the local archives being read correspond to prior ingest-member rows and compatibility matches. It is not a proof for this production state:

- the mirror is empty;
- DILA's server-side delta retention has a known gap relative to the Dec 2025 DB state;
- a forced full scan selects the baseline plus all locally planned deltas, not "only missing deltas" (`crates/jurisearch-pipeline/src/ingest/mod.rs:91-109`);
- incompatible or missing resume rows cause processing, and processing projects into `public` tables (`juri.rs:451-480`).

For jurisprudence, document identity is `source:source_uid` (`crates/jurisearch-package/src/identity.rs:25-31`). Reprocessing an older global baseline member for an existing `source_uid` can update the same logical row. If the missing Dec 2025 to recent delta chain contains later versions/updates that are not available locally, source inspection does not prove those newer DB values are preserved through a full ingest.

Therefore the statement "the Dec 2025 to May 2026 gap simply means updates are not added, not lost" is not safe against the actual code. It is safe only under an additional, source-verified condition: all archive members that overlap existing Dec 2025 data are skipped by compatible resume, or the local archive chain is complete enough that reprocessing replays the same or newer content.

That condition has not been established by the provided live facts.

## Fingerprint Safety

The deployed `2c9c962` projection fix is the right mitigation for the known fingerprint-null regression on unchanged re-projects, assuming the deployed source matches the intended patch. On compatible resume skip, projection does not run. On reprocess, the chunk upsert should preserve fingerprints when body/context are unchanged and invalidate only real content changes; then embed can reselect invalidated chunks.

However, do not rely on `build_rebaseline` itself to reject every bad fingerprint state. The explicit coverage/fingerprint preflight in the source is `bootstrap_preflight`, and it is called in the first-baseline bootstrap path (`crates/jurisearch-package-build/src/cycle.rs:624-647`, `cycle.rs:810-910`), not in `rebaseline_cycle` (`cycle.rs:251-355`). A stray fingerprint inconsistency should be caught by manual SQL coverage checks and by client apply readiness, but the rebaseline builder is primarily a copy/sign/publish path.

Required manual pre/post check before considering a published rebaseline usable:

```sql
SELECT count(*) AS null_chunk_fingerprints
FROM public.chunks
WHERE embedding_fingerprint IS NULL;

SELECT count(*) AS chunks_without_current_embedding
FROM public.chunks c
LEFT JOIN public.chunk_embeddings ce
  ON ce.chunk_id = c.chunk_id
 AND ce.embedding_fingerprint = 'bge-m3:1024:normalize:true'
 AND ce.model = 'bge-m3'
 AND ce.dimension = 1024
WHERE ce.chunk_id IS NULL;
```

Both should be zero.

## Memory, Time, Blast Radius

The package-copy portion is memory-bounded by the baseline streaming design: full media packages stream table COPY payloads and hash while writing instead of materializing the full corpus in Rust memory. That addresses the old full-snapshot memory shape.

This run is still heavy:

- fetch may download several GB per the current DILA listing if the cursor allows it;
- full ingest over four jurisprudence sources can be hours of XML parsing and DB writes;
- because this is a full-scan cycle, replay-snapshot refresh is enabled unless the environment skip is set;
- the rebaseline media package is full corpus, not jurisprudence-only, so expect a payload in the same class as the prior full baseline, not a small delta.

Served-feed blast radius is contained but not zero:

- `core-1-1` and `core-1-2` are not overwritten by the rebaseline publisher;
- a publish failure before manifest replacement should leave the currently served manifest intact;
- an incomplete rebaseline staging slot is discarded and rebuilt by the next rebaseline attempt (`crates/jurisearch-package-build/src/cycle.rs:282-310`);
- DB mutations from ingest are committed in batches before publish. A failure after partial/full ingest but before publish can leave producer tables changed without a new served package.

That last point is the operational risk that makes the current plan NO-GO.

## Enrichment

Use `--skip-enrich` for any rehearsal or first production attempt. The update path otherwise calls `enrich_group` for the group after ingest (`crates/jurisearch-producer/src/update.rs:319-325`). For jurisprudence this can trigger Judilibre work for `cass`/`inca`; it is not needed to validate rebaseline packaging and it adds time, external dependency, and rate-limit risk.

## What Would Make This A GO

A safe path needs one of these adjustments:

1. Rehearse the exact command on a restored prod clone first, with the empty-mirror/fetch-cursor state copied, and diff the resulting jurisprudence tables against prod before allowing the operation.
2. Repair the archive mirror and fetch cursor so the local archive set is complete and contiguous through the DB's known current point, then run the rebaseline.
3. Use or add a DB-snapshot-only rebaseline operation that skips fetch and ingest and only publishes the current producer DB state as a new full media root. That would match the desired "current DB state re-published as fresh baseline" semantics, but it is not what `jurisearch-producer rebaseline` currently does.

Minimal non-mutating preflight that is safe now:

```bash
/usr/local/bin/jurisearch-producer rebaseline --config /etc/jurisearch/producer.toml --source cass --dry-run

/usr/local/bin/jurisearch-producer fetch --config /etc/jurisearch/producer.toml --source cass --dry-run
/usr/local/bin/jurisearch-producer fetch --config /etc/jurisearch/producer.toml --source inca --dry-run
/usr/local/bin/jurisearch-producer fetch --config /etc/jurisearch/producer.toml --source capp --dry-run
/usr/local/bin/jurisearch-producer fetch --config /etc/jurisearch/producer.toml --source jade --dry-run
```

Do not proceed if any fetch dry-run reports archive names as `already_present` that are absent from the mirror.

If a clone rehearsal passes and you deliberately accept the ingest semantics, the production command would be:

```bash
/usr/local/bin/jurisearch-producer rebaseline --config /etc/jurisearch/producer.toml --source cass --skip-enrich
```

I would not use `--skip-fetch` with an empty mirror. It would rely entirely on the existing cursor/mirror state and is likely to fail planning/ingest rather than repair anything.

## Success And Abort Signals

If this becomes safe after the adjustments above, success signals are:

- command JSON has `status:"ok"`, `command:"rebaseline"`, `rebaselined:true`, `published_package:"core-2-3"` or the sequence implied by the then-current catalog head;
- `package_catalog` newest `core` row is `package_kind='rebaseline'`, `status='published'`, `package_sequence=3` if starting from head 2;
- served `manifest.json` verifies and has `active_baseline.package_kind='rebaseline'`, `active_baseline.sequence=3`, `head_sequence=3`, and `min_available_sequence=3`;
- `packages/core/packages/core-2-3/` exists and its embedded manifest verifies;
- jurisprudence ingest journals for `cass`, `inca`, `capp`, `jade` completed, and their latest processed archive timestamps are fresh enough that a subsequent ordinary `update --group jurisprudence --dry-run` will not hit `IngestCursorStale`;
- fingerprint coverage SQL above returns zero/null-free;
- a test client can plan/apply from empty state and from sequence 2 through the forward rebaseline.

Abort signals:

- fetch dry-run indicates cursor/mirror inconsistency;
- real fetch reports unexpected `already_present` for absent files;
- ingest reports `BlockedIncompatible`, nonzero failed members, or unexpected large `inserted_documents`/`inserted_chunks` for already-known old archives;
- RSS approaches the operator threshold despite streaming fixes;
- replay snapshot or COPY-out saturates CT110/Storebox beyond acceptable I/O;
- any fingerprint coverage count becomes nonzero after ingest/embed.

## Bottom Line

NO-GO on running production `rebaseline` now. The publisher side is designed as a staged, full-corpus rebaseline and should produce a new active media root such as `core-2-3`; it does not delete `core-1-1`/`core-1-2`. The unsafe part is earlier: `jurisearch-producer rebaseline` is not a pure "publish current DB" operation. It fetches and full-ingests first, and the stated empty mirror plus stale/missing DILA delta chain is not proven safe against regressions in existing jurisprudence rows.
