# Plan Validation: Discard + Rebuild/Re-publish `core-1-2`

Verdict: **GO-with-adjustments**.

The source supports the core plan: delete the seq-2 producer catalog row, remove the existing published `core-1-2` artifact directory, then run the producer so the next ordinary incremental is rebuilt from seq 1 over the same `(0, 1445243]` change window and rematerializes replace sets from current `public` rows. Because step (a) has now stamped `public.chunks.embedding_fingerprint`, the rebuilt `chunks_with_embeddings.replace_set.jsonl` should no longer contain null fingerprints for the affected chunks.

The adjustments are operational, not conceptual:

1. **Move the old artifact aside and back up the catalog row/manifest instead of blind `rm -rf`.** `publish_package` requires the destination package id not to exist when the content digest differs, but preserving the old seq-2 artifact and catalog row gives you a clean rollback if the rebuild fails before the new manifest is published.
2. **Treat `--dry-run` only as a config/fetch smoke test.** In `update.rs`, dry-run exits after fetch/cursor resolution and never opens the DB, takes the update lock, ingests, embeds, builds, or publishes.
3. **Do not assume `--skip-fetch` forces ordinary incremental.** It prevents a new fetch, but `group_run_kind` is still recomputed from existing fetched/adopted baseline markers under the update lock. If an unadopted fetched baseline already exists, the run can still route to rebaseline.
4. **Plan for a transient served-feed inconsistency window.** Once the old `core-1-2` directory is moved/removed, the existing `manifest.json` still points at it until the producer publishes the rebuilt manifest. That does not corrupt `core-1-1`, but any live client polling during that window can fail. Block/maintenance the feed or keep the window as short as possible.

## Source Grounding

`package_catalog` is the sequence/window authority. The migration defines primary key `(corpus, package_sequence)`, a globally unique `package_id`, and no inbound foreign key in the schema definition (`crates/jurisearch-storage/src/migrations.rs:977`). `insert_package_catalog_row` is identity-checked: an existing `package_id` is accepted only if immutable fields match; otherwise it errors (`crates/jurisearch-storage/src/package_catalog.rs:35`). The next incremental reads only `latest_package_for_corpus`, ordered by `package_sequence DESC`, for the previous package and `lo` (`crates/jurisearch-storage/src/package_catalog.rs:133`, `crates/jurisearch-package-build/src/incremental.rs:142`).

So after deleting seq 2, latest becomes seq 1. `build_incremental_inner` sets `lo = prev.included_change_seq_high`, computes `hi = current_change_seq`, derives `from_sequence = prev.package_sequence`, `to_sequence = from_sequence.next()`, and `package_id = core-1-2` (`crates/jurisearch-package-build/src/incremental.rs:148`, `:361`). With your live facts, that rebuilds `(0, 1445243]`, not seq 3 and not an empty window.

The published artifact id really must be cleared. `publish_package` treats published ids as immutable: same embedded `artifact_sha256` is an idempotent no-op, different content is an error (`crates/jurisearch-package-build/src/publish.rs:40`). Since the rebuilt artifact must differ, the existing `packages/core-1-2/` directory cannot remain at publish time.

Do not hand-edit `manifest.json`. `producer_cycle` publishes the package, marks its row published, then rebuilds and atomically renames the signed remote manifest (`crates/jurisearch-package-build/src/cycle.rs:170`, `:188`; `crates/jurisearch-package-build/src/publish.rs:91`). `build_remote_manifest` reads catalog rows, reads each published embedded manifest, verifies catalog identity, and emits the package list in catalog sequence order (`crates/jurisearch-package-build/src/remote_manifest.rs:93`, `:131`). Manual manifest edits would only create a signature/catalog mismatch risk.

`corpus_state` is not part of the producer build decision. It is client/apply state; the producer's package head is from catalog rows (`crates/jurisearch-package-build/src/cycle.rs:930`). `PackageHighWaterMark` is a run-report/checkpoint output from `published_head`, not a build input.

The replace-set rematerialization reads current `public` rows. `materialize_replace_set` calls `replace_set_rows(tx, "public", group, document_id)` (`crates/jurisearch-package-build/src/incremental.rs:575`). For `ChunksWithEmbeddings`, the replace-set group includes both `chunks` and `chunk_embeddings` (`crates/jurisearch-storage/src/incremental.rs:67`). Therefore the rebuilt payload should reflect the post-stamp `chunks.embedding_fingerprint`.

## Corrected Procedure

Keep timers disabled and re-check no producer process is active.

Preserve rollback state first:

```bash
ts=20260701-core-1-2-rebuild
root=/srv/jurisearch/storebox/packages/core
cp -a "$root/manifest.json" "$root/manifest.json.pre-$ts"
mkdir -p "$root/.staging/manual-discard-$ts"
```

Back up and delete exactly the seq-2 catalog row, asserting one row:

```sql
BEGIN;
CREATE TABLE public.package_catalog_backup_core_1_2_20260701 AS
  SELECT *
  FROM public.package_catalog
  WHERE corpus = 'core' AND package_sequence = 2;

-- expect 1
SELECT count(*) FROM public.package_catalog_backup_core_1_2_20260701;

DELETE FROM public.package_catalog
WHERE corpus = 'core' AND package_sequence = 2
RETURNING package_id, status, included_change_seq_low, included_change_seq_high;
-- expect: core-1-2, published, 0, 1445243
COMMIT;
```

Move the old published artifact out of the immutable package path:

```bash
mv "$root/packages/core-1-2" "$root/.staging/manual-discard-$ts/core-1-2.old"
```

Then run the rebuild:

```bash
/usr/local/bin/jurisearch-producer update \
  --config /etc/jurisearch/producer.toml \
  --group legislation \
  --skip-fetch \
  --skip-enrich
```

`--skip-enrich` is harmless and reasonable here; the legislation group does not need the Judilibre enrichment phase. It is not required for package correctness.

A prior `--dry-run` is safe but weak: it stops before DB lock/build/publish and only proves config/cursor access. It will not prove seq 2 can be rebuilt.

## Pre-run Checks

Before deleting anything, confirm:

```sql
SELECT package_sequence, package_id, status, included_change_seq_low, included_change_seq_high
FROM public.package_catalog
WHERE corpus = 'core'
ORDER BY package_sequence;

SELECT max(change_seq) FROM public.package_change_log;
```

Also confirm no pending unadopted baseline is already on disk/state. Source detail: with `--skip-fetch`, `read_fetch_cursor` is used instead of fetching, but `group_run_kind` still computes `new_baselines` from current state under the update lock (`crates/jurisearch-producer/src/update.rs:210`, `:273`). If status says `rebaseline_pending=true`, stop; the proposed command could route to `run_rebaseline_cycle`, not ordinary incremental.

## Failure and Rollback

The discard itself does not damage `core-1-1`. It only removes one catalog row and moves the seq-2 artifact directory. The baseline package directory and baseline catalog row are untouched.

The vulnerable served-feed window is real:

- after catalog delete but before moving the old dir, the old manifest still points at the old broken `core-1-2`;
- after moving/removing the old dir and before the new manifest is published, the old manifest points at a missing artifact;
- after the new artifact is published but before remote manifest rename, the old manifest may point at the old seq-2 digest while the path now contains the new seq-2 artifact.

Those are client-visible inconsistency windows. With no clients and a maintenance window, this is acceptable. If external clients can poll the feed, block access or schedule downtime.

Recovery is good as long as the pending staging slot remains intact. `producer_cycle` starts with `resume_pending`; a staged artifact with a matching catalog row is published and marked, while an uncataloged staged artifact is discarded (`crates/jurisearch-package-build/src/cycle.rs:153`, `:386`). If the run fails before a catalog row is inserted, rerun builds fresh. If it fails after catalog insert and before publish, rerun should resume. If it fails after publish but before manifest rewrite, rerun should rebuild the remote manifest from catalog/artifact.

If you need to roll back to the old broken-but-coherent feed before a successful rebuild, move `core-1-2.old` back, restore the catalog row from `package_catalog_backup_core_1_2_20260701`, and restore `manifest.json.pre-$ts`.

## Expected Drift

The rebuilt seq-2 will not be byte-equivalent to the old package except fingerprints in an absolute sense. The embedded manifest includes a fresh `created_at` and `builder_run_id`, and the signed embedded manifest plus remote manifest signature/digests will change (`crates/jurisearch-package-build/src/incremental.rs:366`). The payload order and window should be the same, and payload files unrelated to the stamped rows should be byte-identical if the DB has had no other raw drift. The decisive intended payload drift is the non-null `embedding_fingerprint` values inside `chunks_with_embeddings.replace_set.jsonl`, plus derived per-file/package/table digests and signatures.

## Post-run Verification

Required success signals:

```sql
SELECT package_sequence, package_id, status, included_change_seq_low, included_change_seq_high
FROM public.package_catalog
WHERE corpus = 'core' AND package_sequence = 2;
-- expect: core-1-2, published, 0, 1445243
```

Verify the served remote manifest:

```bash
jq '.payload.head_sequence, .payload.packages[] | select(.package_id=="core-1-2") | {package_id,from_sequence,to_sequence,sha256}' \
  /srv/jurisearch/storebox/packages/core/manifest.json
```

Expect `head_sequence = 2`, `core-1-2` from 1 to 2, and a new sha256 compared with the old manifest.

Decisive payload check:

```bash
grep -c '"embedding_fingerprint":null' \
  /srv/jurisearch/storebox/packages/core/packages/core-1-2/payload/chunks_with_embeddings.replace_set.jsonl
```

Expect `0`.

Optional stronger checks:

```bash
jq '.payload.identity.package_id, .payload.identity.from_sequence, .payload.identity.to_sequence, .payload.apply.preconditions.embedding_fingerprint' \
  /srv/jurisearch/storebox/packages/core/packages/core-1-2/manifest.json
```

and, if available:

```bash
jurisearch-package verify --root /srv/jurisearch/storebox/packages --corpus core
```

## Bottom Line

**GO-with-adjustments**: the source confirms there is no hidden producer counter/cursor that would force seq 3 or an empty window after deleting seq 2. The next ordinary incremental will rebuild `core-1-2` from seq 1 and current `public` rows. Use a catalog/artifact/manifest backup, move the old artifact aside instead of deleting it, ensure no pending baseline can route to rebaseline, and treat the served feed as temporarily inconsistent until the new remote manifest is atomically published.
