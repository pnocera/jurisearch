# Advice: Ingest Dec2025-Jun2026 Jurisprudence Deltas

## Recommendation

Choose **Option B: add a one-shot stale-cursor override to producer `update`**, then run a delta-only jurisprudence update from the existing completed-ingest cursor over the already-present contiguous mirror.

This is both safer and more efficient than forced rebaseline:

- It ingests exactly the missing Dec2025-Jun2026 deltas.
- It preserves the ordinary incremental chain instead of forcing a 150+ GB full media rebaseline.
- It advances the completed-ingest cursor to Jun2026, so the 45-day stale guard stops firing afterward.
- It avoids re-walking the July 2025 global baseline and all already-ingested deltas.
- It still runs the normal producer embed and publish phases, so the published package contains the newly inserted documents/chunks/embeddings.

There is no existing producer flag that does this today. The standalone CLI has `jurisearch sync --since`, but that is not the producer update path and does not run the producer's locked enrich/embed/publish cycle. The missing production primitive is small: an `update --accept-stale-cursor` flag that turns a stale-but-operator-verified cursor into the same delta-only mode a fresh cursor already gets.

One correction to the prompt: after current head sequence 2, the ordinary incremental package id is **`core-2-3`**, not `core-1-3`. A forced rebaseline from the same head also uses `core-2-3`, but with `package_kind = rebaseline` and a new generation.

## Why Not Option A

`rebaseline --source cass` would work only if ingest compatibility for the already-ingested archive members matches. It is not guaranteed to "just reprocess everything".

The source behavior is:

- A forced rebaseline makes the source appear in `new_baselines`, so `choose_ingest_mode` returns `incremental=false` and bypasses the stale-cursor guard (`crates/jurisearch-producer/src/update.rs:529-544`).
- A full-scan ingest selects the selected baseline plus all deltas in timestamp order (`crates/jurisearch-pipeline/src/ingest/mod.rs:91-109`; `crates/jurisearch-ingest/src/archive/planner.rs:90-131`).
- For each member, resume checks `archive_name`, `member_path`, parser version, schema version, code version, and source payload hash (`crates/jurisearch-storage/src/ingest_accounting/resume.rs:35-75`).
- If compatible and previous status is `inserted` or `skipped`, it returns `Skip` (`resume.rs:86-103`).
- If incompatible, it returns `BlockedIncompatible` (`resume.rs:76-83`). The JURI ingest path records a failed member and returns without projection (`crates/jurisearch-pipeline/src/ingest/juri.rs:418-449`), then the producer refuses to publish a non-completed ingest (`crates/jurisearch-producer/src/update.rs:629-640`).

So Option A has two outcomes for the old July-Dec archive members:

- If compatibility matches, most old members resume-skip and the run eventually ingests the Dec-Jun deltas.
- If compatibility differs, the run fails early as an incompatible ingest, not a slow full reprocess.

If it does complete, Option A should be non-regressing with a truly contiguous full chain: the planner replays baseline then deltas chronologically, and jurisprudence document identity is `source:source_uid`; the final state should be the latest archive state. The deployed fingerprint-preserve fix also protects unchanged chunks during re-projection (`crates/jurisearch-storage/src/projection/legi.rs:98-105`).

But Option A is still the wrong first choice because it pays for:

- scanning the whole 5.1 GB jurisprudence mirror instead of just missing deltas;
- full-scan replay-snapshot refresh;
- embedding new/invalidated chunks;
- a full `core` rebaseline publish over all replicated tables, streamed but still about full-baseline scale.

That final publish is the expensive part. `build_media_package` streams table `COPY ... FORMAT binary` through `tee_digest`, so memory is bounded, but it still writes the entire corpus payload (`crates/jurisearch-package-build/src/baseline.rs:276-330`). There is no correctness need to do that now because the archive gap is not real.

## Why Option B Is Correct

The producer already has all pieces needed for a safe delta-only catch-up except the stale override.

Current producer behavior:

- For non-rebaseline sources, `ingest_one` reads the DB-authoritative completed-run cursor using `latest_completed_ingest_archive_compact_with_client` (`crates/jurisearch-producer/src/update.rs:593-607`).
- `choose_ingest_mode` rejects that cursor if older than 45 days (`update.rs:551-565`).
- If the cursor is accepted, it returns `incremental=true` and `since_compact=Some(cursor)` (`update.rs:566-570`).
- `ingest_one` passes that into `ArchiveSyncFilter` (`update.rs:609-628`).
- `select_archives_to_process` skips the baseline and selects deltas with `delta.timestamp.compact() >= since` (`crates/jurisearch-pipeline/src/ingest/mod.rs:91-109`).

Given the corrected live facts, the old cursor is stale by age but not unsafe by continuity: the Dec2025-Jun2026 chain is already on disk and contiguous. Proceeding delta-only from the current completed cursor is exactly what the stale guard would have done if the cursor were fresher.

The inclusive `>= since` bound is safe. It may re-read the cursor archive itself, but compatible prior members resume-skip. Newer deltas process normally.

The JURI archive plan is deterministic and chronological. `plan_from_dir` sorts recognized archives by timestamp and filename, selects the newest baseline, and sorts deltas after that baseline (`crates/jurisearch-ingest/src/archive/planner.rs:90-131`). That gives a stable Dec-Jun replay order for the delta subset.

The embed phase will cover new chunks. The chunk embedding selector loads rows where `chunks.embedding_fingerprint IS NULL`, no embedding exists, or the embedding config mismatches (`crates/jurisearch-storage/src/dense.rs:89-114`). The producer treats a no-pending result as success, otherwise finalizes the dense rebuild after inserting embeddings (`crates/jurisearch-pipeline/src/embed.rs:212-230`; `crates/jurisearch-producer/src/update.rs:621-658`).

The package publish then builds an ordinary incremental from the latest catalog head:

- `build_incremental` reads the latest package row and uses its `included_change_seq_high` as `lo` (`crates/jurisearch-package-build/src/incremental.rs:142-149`).
- It freezes `hi` after ingest/embed and gathers changed scopes in `(lo, hi]` (`incremental.rs:178-191`).
- From current head sequence 2, it sets `from_sequence=2`, `to_sequence=3`, and `package_id=core-2-3` (`incremental.rs:356-374`).
- The streamed JSONL fixes keep incremental payload memory bounded; the remaining key sets are O(changed scopes), not O(full corpus) (`incremental.rs:197-203`).

## Code Change For B

Add an explicit producer-only override:

```text
jurisearch-producer update --accept-stale-cursor
```

Implementation shape:

1. Add `accept_stale_cursor: bool` to `UpdateOptions`, default `false`.
2. Add `--accept-stale-cursor` to the `Update` subcommand only.
3. Pass it into `choose_ingest_mode`, or make it part of an options struct.
4. Change the stale branch from:

```rust
if stale {
    Err(ProducerError::IngestCursorStale { ... })
} else {
    Ok(ArchiveModeChoice {
        incremental: true,
        since_compact: Some(compact.to_owned()),
    })
}
```

to:

```rust
if stale && !accept_stale_cursor {
    Err(ProducerError::IngestCursorStale { ... })
} else {
    Ok(ArchiveModeChoice {
        incremental: true,
        since_compact: Some(compact.to_owned()),
    })
}
```

Keep the normal parsing/shape validation. If `unix_from_compact_archive_timestamp(compact)` returns `None`, I would still fail even with the override unless you also add an explicit `--since`. This flag should override age, not malformed cursor data.

I would not add `--since` for this incident unless you need manual cursor selection. `--accept-stale-cursor` is safer because it uses the DB's completed-run cursor rather than an operator-typed timestamp. The source comments make that cursor intentionally conservative: it is the max completed, non-member-limited `ingest_run.manifest.freshness.latest_archive_timestamp_compact`, not a member-level max that could skip a failed earlier archive (`crates/jurisearch-storage/src/ingest_accounting/runs.rs:176-231`).

Add a visible JSON field to the update output, e.g. `"accepted_stale_cursor": true`, so this remains auditable.

## Command

After deploying the small override:

```bash
/usr/local/bin/jurisearch-producer update \
  --config /etc/jurisearch/producer.toml \
  --group jurisprudence \
  --skip-fetch \
  --accept-stale-cursor
```

Use `--skip-fetch` for this one-shot because the mirror has already been verified as present and contiguous. It avoids network/listing variability and makes the run consume exactly the on-disk mirror state you validated.

I would not add `--skip-enrich` if the operator's definition of "FULL current data" includes Judilibre-derived zones for cass/inca. If the immediate target is strictly DILA archive currency and the fastest safe package, add `--skip-enrich` and run enrichment later; that is a product/ops choice, not required for the Dec-Jun DILA gap.

## Expected Result

Expected run behavior:

- No fetch.
- For each jurisprudence source, choose delta-only from its stale completed-ingest cursor because `--accept-stale-cursor` allows the age override.
- Process only deltas at or after the current cursor:
  - cass/inca from around `20251201...` through `20260629...`;
  - capp from around `20251117...` through `20260612...`;
  - jade from around `20251205...` through `20260630...`.
- Already-ingested boundary members resume-skip if re-read.
- New decision members insert/upsert documents/chunks/edges and emit outbox changes.
- Embed only newly pending or invalidated chunks/zone units.
- Publish ordinary incremental `core-2-3` from sequence 2 to 3, unless the head changes before the run.
- Keep the active generation unchanged; no full rebaseline generation is created.
- Advance completed-ingest cursors to the latest processed archive per source, so the normal stale guard no longer fires.

Expected success checks:

```sql
SELECT source,
       max(manifest->'freshness'->>'latest_archive_timestamp_compact') AS completed_cursor
FROM public.ingest_run
WHERE source IN ('cass','inca','capp','jade')
  AND status = 'completed'
  AND COALESCE((manifest->'freshness'->>'member_limited')::boolean, false) = false
GROUP BY source
ORDER BY source;
```

Expect cursors near the mirror tails: cass/inca `20260629...`, capp `20260612...`, jade `20260630...`.

```sql
SELECT corpus, package_sequence, package_id, package_kind, status,
       included_change_seq_low, included_change_seq_high
FROM public.package_catalog
WHERE corpus = 'core'
ORDER BY package_sequence DESC
LIMIT 3;
```

Expect newest row `package_id='core-2-3'`, `package_kind='incremental'`, `status='published'`, `included_change_seq_low=1445243`, and `included_change_seq_high > 1445243`.

Served manifest should have `head_sequence=3` with `core-2-3` in `packages`, while `active_baseline` remains the existing baseline/rebaseline for the current generation.

Also check coverage remains clean:

```sql
SELECT count(*) FROM public.chunks WHERE embedding_fingerprint IS NULL;
```

Expect 0 after embed/finalize.

Before re-arming timers, run a normal dry run or status check equivalent and verify `update --group jurisprudence` would no longer fail `IngestCursorStale`.

## Existing Paths

There is no existing producer option that bypasses `IngestCursorStale` or forces producer update from a given `since`.

Current producer `update` flags are only `--config`, `--group`, `--dry-run`, `--skip-fetch`, and `--skip-enrich` (`crates/jurisearch-producer/src/bin/jurisearch_producer.rs:85-99`, `bin/jurisearch_producer.rs:275-287`). `rebaseline` has `--from-db`, but that skips ingest entirely and cannot bring the DB current (`bin/jurisearch_producer.rs:105-130`, `bin_jurisearch_producer.rs:342-417`).

The standalone CLI `sync` has `--since` and builds an incremental archive filter (`crates/jurisearch-cli/src/args.rs:453-470`; `crates/jurisearch-cli/src/ingest.rs:41-71`). I do not recommend using it for this production action because it is not the producer `update` orchestration: it does not run under the producer update lock, does not drive producer embed/publish as one operation, and would require a separate publish step.

## Risk Summary

Option B's main risk is operator misuse: accepting a stale cursor when the archive chain is not actually contiguous. Your corrected live facts remove that risk for this run. I would still keep the override name explicit and require it only for a single manual run.

Option A is safe only if compatibility rows match or if you are prepared for it to fail on `BlockedIncompatible`. It is much heavier and publishes a full rebaseline that is unnecessary when the missing deltas are present.

`--from-db` is not suitable for this goal because it republishes the current Dec2025-ish DB state and seeds cursors forward; it would make timers work while deliberately skipping the Dec-Jun archives. That is the opposite of "FULL CURRENT data".

Bottom line: implement the small `--accept-stale-cursor` override and run the producer delta update with `--skip-fetch`. This gives the desired full current jurisprudence data with the least mutation surface, least I/O, and cleanest package history.
