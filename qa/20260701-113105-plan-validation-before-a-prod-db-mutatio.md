# Q&A — 20260701-113105

## Question

# Plan validation (BEFORE a prod DB mutation) — stamp the 231,678 NULL embedding_fingerprint chunks on CT110

Repo `/home/pierre/Work/jurisearch`. **Read the real source to validate; don't trust my prose.** Pre-action gate on
a production DB mutation (CT110 PostgreSQL 18.4, db `jurisearch`). I will NOT execute until you validate. This is
STEP (a) of a two-step fix; step (b) (rebuild+re-publish core-1-2) is separate and will be gated on its own.

## Confirmed root cause (code-verified by a prior read-only investigation)
`public.chunks` has 4,792,053 rows; 231,678 have `embedding_fingerprint IS NULL`, and ALL 231,678 have a
`public.chunk_embeddings` row with `model='bge-m3', dimension=1024, embedding_fingerprint='bge-m3:1024:normalize:true'`
(verified live). The embed "pending" selector (`crates/jurisearch-storage/src/dense.rs:89-115`) keys on
`chunk_embeddings` (missing/mismatched embedding), NOT on `chunks.embedding_fingerprint`, so these chunks are never
"pending" → `embed_chunks_inner` returns `no_results` (`crates/jurisearch-pipeline/src/embed.rs:212-214`) before the
dense-finalize stamp (`crates/jurisearch-storage/src/dense.rs:213-220`,
`UPDATE chunks SET embedding_fingerprint=$1 WHERE embedding_fingerprint IS DISTINCT FROM $1`) can run. The stamp was
originally lost when an earlier finalize tx (stamp + `CREATE INDEX`) rolled back on `permission denied for schema
public`, while the per-page `chunk_embeddings` inserts had already committed. Impact: producer search is fine (it
filters on `chunk_embeddings.embedding_fingerprint`), but the client apply readiness gate
(`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:297-313,384-407`, called from
`crates/jurisearch-syncd/src/apply.rs:1106-1111`) requires `chunks.embedding_fingerprint = active`, so `core-1-2` is
un-appliable on clients (whole apply rolls back).

## Proposed STEP (a): a single bounded UPDATE (verify each claim)
```sql
BEGIN;
-- reversibility snapshot: capture the exact chunk_ids we will stamp
CREATE TEMP TABLE fp_stamp_backup ON COMMIT DROP AS
  SELECT c.chunk_id
  FROM public.chunks c JOIN public.chunk_embeddings ce ON ce.chunk_id = c.chunk_id
  WHERE c.embedding_fingerprint IS NULL
    AND ce.embedding_fingerprint = 'bge-m3:1024:normalize:true'
    AND ce.model = 'bge-m3' AND ce.dimension = 1024;
-- (persist the ids OUTSIDE the tx first if we want post-hoc revert — see Q4)
UPDATE public.chunks c
SET embedding_fingerprint = 'bge-m3:1024:normalize:true'
FROM public.chunk_embeddings ce
WHERE ce.chunk_id = c.chunk_id
  AND c.embedding_fingerprint IS NULL
  AND ce.embedding_fingerprint = 'bge-m3:1024:normalize:true'
  AND ce.model = 'bge-m3' AND ce.dimension = 1024;
-- expect: UPDATE 231678
COMMIT;
```

## Questions to adjudicate against source + the live facts
1. **Is the predicate correct and complete?** It stamps ONLY chunks that already have a CURRENT-model
   (`bge-m3`/`1024`/`bge-m3:1024:normalize:true`) embedding — matching what the pending-selector and finalize consider
   "current" (`dense.rs:89-115`). Confirm it can't stamp a chunk whose embedding is stale/wrong (so we never mark a
   bad chunk "ready"). Confirm the expected row count is exactly the 231,678 (i.e. this predicate == the NULL-fp set,
   not more, not fewer). Is stamping to the literal `'bge-m3:1024:normalize:true'` the exactly-correct value the rest
   of the system compares against (`EmbeddingFingerprint`, `crates/jurisearch-embed/src/fingerprint.rs`; the coverage
   gate; the activation stamp)?
2. **Is a raw UPDATE safe vs going through a producer code path?** Confirm there are NO triggers on `public.chunks`
   that this bypasses or should fire (change capture is app-level via `emit_change`→`package_change_log`,
   `outbox.rs:143-208`, NOT a DB trigger — so a raw UPDATE writes no change_log row; that's EXPECTED here since step
   (b) rebuilds seq-2 by rematerializing current `public.chunks`, not by consuming a change_log delta). Confirm the
   UPDATE has no other side effects (denormalized copies, generated columns, FKs) and that `chunks.embedding_fingerprint`
   is a plain nullable column.
3. **Is NO ivfflat rebuild genuinely correct?** The 231k already have `chunk_embeddings` rows, and
   `chunk_embeddings_embedding_ivfflat_idx` is a plain (non-partial) ivfflat on `chunk_embeddings.embedding` (verified
   `WITH (lists='2168')`), so their vectors are already indexed. Confirm stamping the `chunks` column alone leaves the
   index correct and makes these chunks fully "ready" without any reindex/coverage recompute — OR identify any cache/
   materialized state (readiness stamp, index_manifest, coverage cache) that must ALSO be refreshed for the producer
   to report 100% coverage. (Note: the producer's own coverage report `load_embedding_coverage`
   `readiness.rs:608-634` requires `c.embedding_fingerprint IS NOT NULL AND ce.embedding_fingerprint = c...` — the
   stamp fixes that directly; is anything else stale?)
4. **Role + reversibility.** `public.chunks` is owned by `jurisearch_owner`; `jurisearch_write` has UPDATE; I'd run it
   as `postgres` (superuser) for simplicity. Any reason to run as `jurisearch_write` instead (ownership/RLS/search_path)?
   For reversibility I'll persist the affected `chunk_id`s to a real table (not a temp ON COMMIT DROP) BEFORE the
   UPDATE so we can revert with `UPDATE public.chunks SET embedding_fingerprint=NULL WHERE chunk_id = ANY(<ids>)`.
   Is that the right revert, and is capturing ids in the SAME statement's set the cleanest (or use `RETURNING`)?
5. **Concurrency/timing.** Producer timers are DISABLED and no producer run is active (I'll re-verify). Confirm running
   this as one transaction is safe with no competing embed/finalize, and estimate cost (231,678-row UPDATE joined on
   the `chunk_embeddings` PK).
6. **Anything MISSED** that makes step (a) unsafe or insufficient on the producer side (independent of step (b)).

## Deliver
GO / GO-with-adjustments / NO-GO on the exact STEP (a) statement (corrected if wrong), the role to run as, the
reversibility approach, and whether any additional producer-side refresh (index/coverage/readiness) is required after
the stamp. Ground it in source + the stated live facts. (Step (b) re-publish is out of scope here — I'll gate it separately.)

## Answer

# Plan Validation: Stamp NULL `chunks.embedding_fingerprint` Rows On CT110

Verdict: **GO-with-adjustments**.

The proposed repair is source-consistent and narrowly targeted: stamping `public.chunks.embedding_fingerprint` for chunks that already have the current `public.chunk_embeddings` row is the missing state transition. It does not require an ivfflat rebuild. The main adjustments are operational: persist the backup set in a real table, update through that exact set, and invalidate the producer-side `query_readiness` cache after the raw update so producer status/readiness recomputes against the repaired rows.

Step (b) remains required if the already-built/published `core-1-2` artifact contains the bad NULL values. This step only repairs CT110 producer DB state.

## Source Findings

The pending selector ignores `chunks.embedding_fingerprint`, exactly as stated:

- `dense.rs:89-115`: `load_chunk_embedding_inputs_with_client` selects pending rows from `chunks LEFT JOIN chunk_embeddings`, where the embedding row is missing or its `embedding_fingerprint`, `model`, or `dimension` differs from the requested values.
- It does not test `c.embedding_fingerprint`.

The normal code paths stamp `chunks.embedding_fingerprint` in two places:

- `projection/embeddings.rs:87-96`: batch embedding insert first updates `chunks.embedding_fingerprint = s.embedding_fingerprint` for staged chunks whose current chunk fingerprint is NULL or already equal.
- `projection/embeddings.rs:126-139`: then it upserts `chunk_embeddings`.
- `dense.rs:213-220`: dense finalize stamps every chunk whose fingerprint differs: `UPDATE chunks SET embedding_fingerprint = $1 WHERE embedding_fingerprint IS DISTINCT FROM $1 RETURNING document_id`.

The failure mode is therefore coherent: committed `chunk_embeddings` rows plus rolled-back finalize stamp leaves no pending embedding inputs, so `embed_chunks_inner` can return no-results before reaching `finalize_dense_rebuild_with_client`.

The schema confirms `chunks.embedding_fingerprint` is a plain nullable text column:

- `migrations.rs:67-79`: `chunks` has `embedding_fingerprint text`, no generated expression.
- `migrations.rs:81-88`: `chunk_embeddings` has `embedding_fingerprint text NOT NULL`, `embedding vector(1024) NOT NULL`, `model text NOT NULL`, `dimension integer NOT NULL CHECK (dimension = 1024)`.
- I found no `CREATE TRIGGER` in the storage migrations/source. Change capture is application-level through `outbox::emit_change`, not DB triggers.

The client readiness failure is exactly the P3A gate:

- `readiness.rs:297-313`: generation dense coverage counts a chunk embedded only when `c.embedding_fingerprint = active` and `ce.embedding_fingerprint = active`.
- `readiness.rs:400-407`: incomplete dense coverage errors.
- `syncd/apply.rs:1106-1111`: incremental apply calls `stamp_query_readiness` before commit; incomplete coverage rolls the whole apply back.

The fingerprint literal is correct for `model=bge-m3`, `dimension=1024`, `normalize=true`:

- `fingerprint.rs:17-21`: storage fingerprint is `"{model}:{dimension}:normalize:{normalize}"`.
- Producer config exposes the same `storage_embedding_fingerprint()` via `config.rs:487-490`.

## Predicate

The predicate is correct for this repair:

```sql
c.embedding_fingerprint IS NULL
AND ce.embedding_fingerprint = 'bge-m3:1024:normalize:true'
AND ce.model = 'bge-m3'
AND ce.dimension = 1024
```

It cannot mark a chunk ready unless a current-model embedding row already exists. It does not stamp chunks with stale/wrong embeddings because all three embedding identity columns are checked.

Given your stated live facts, this predicate should match exactly the 231,678 NULL-fingerprint chunks. I would still make the production script assert that count before committing.

## Corrected Statement

Use a persistent backup table and update through it. This makes the affected set durable and makes the update row count tied to the captured set.

Run as `postgres`:

```sql
BEGIN;

CREATE TABLE public.fp_stamp_backup_20260701_113105 (
  chunk_id text PRIMARY KEY
);

INSERT INTO public.fp_stamp_backup_20260701_113105 (chunk_id)
SELECT c.chunk_id
FROM public.chunks c
JOIN public.chunk_embeddings ce ON ce.chunk_id = c.chunk_id
WHERE c.embedding_fingerprint IS NULL
  AND ce.embedding_fingerprint = 'bge-m3:1024:normalize:true'
  AND ce.model = 'bge-m3'
  AND ce.dimension = 1024
ORDER BY c.chunk_id;

-- Expect 231678.
SELECT count(*) AS backup_rows
FROM public.fp_stamp_backup_20260701_113105;

UPDATE public.chunks c
SET embedding_fingerprint = 'bge-m3:1024:normalize:true'
FROM public.fp_stamp_backup_20260701_113105 b
JOIN public.chunk_embeddings ce ON ce.chunk_id = b.chunk_id
WHERE c.chunk_id = b.chunk_id
  AND c.embedding_fingerprint IS NULL
  AND ce.embedding_fingerprint = 'bge-m3:1024:normalize:true'
  AND ce.model = 'bge-m3'
  AND ce.dimension = 1024;

-- Expect UPDATE 231678.

DELETE FROM public.index_manifest
WHERE key = 'query_readiness';

COMMIT;
```

Post-checks:

```sql
SELECT count(*) AS remaining_null_ready_chunks
FROM public.chunks c
JOIN public.chunk_embeddings ce ON ce.chunk_id = c.chunk_id
WHERE c.embedding_fingerprint IS NULL
  AND ce.embedding_fingerprint = 'bge-m3:1024:normalize:true'
  AND ce.model = 'bge-m3'
  AND ce.dimension = 1024;

SELECT count(*) AS mismatched_current_embeddings
FROM public.chunks c
JOIN public.chunk_embeddings ce ON ce.chunk_id = c.chunk_id
WHERE ce.embedding_fingerprint = 'bge-m3:1024:normalize:true'
  AND ce.model = 'bge-m3'
  AND ce.dimension = 1024
  AND c.embedding_fingerprint IS DISTINCT FROM ce.embedding_fingerprint;
```

Expected result for both is `0`, assuming every chunk has the current embedding row as stated.

## Reversibility

The backup table should persist until after step (b) and verification. The revert should update through the backup table and guard on the stamped value, not use a giant client-side `ANY(<ids>)` array:

```sql
BEGIN;

UPDATE public.chunks c
SET embedding_fingerprint = NULL
FROM public.fp_stamp_backup_20260701_113105 b
WHERE c.chunk_id = b.chunk_id
  AND c.embedding_fingerprint = 'bge-m3:1024:normalize:true';

DELETE FROM public.index_manifest
WHERE key = 'query_readiness';

COMMIT;
```

Do not use only a temp `ON COMMIT DROP` table if you want post-hoc rollback. A temp table is fine for an in-transaction sanity check, but it is not a durable revert plan.

`UPDATE ... RETURNING chunk_id` into a table would also work, but `INSERT backup SELECT ...` followed by `UPDATE ... FROM backup` is clearer operationally and makes the expected count visible before mutation.

## Raw Update And Outbox

A raw update is safe for step (a), with one important boundary:

- There are no storage-defined triggers to bypass.
- `chunks.embedding_fingerprint` is a plain nullable column.
- `outbox::emit_change` is called explicitly by application code (`outbox.rs:143-201`), not by a database trigger.
- `projection/embeddings.rs:141-172` and `dense.rs:221-252` show the normal paths emit document-scoped outbox rows only when called with an `OutboxContext`.

Therefore this raw SQL will not create `package_change_log` rows. That is acceptable only because your step (b) is to rebuild/re-publish by rematerializing current producer tables. If step (b) were a normal `producer_cycle` consuming only the outbox window, this raw update would be invisible and unsafe.

## No Ivfflat Rebuild

No ivfflat rebuild is required for step (a).

The vector rows already exist in `chunk_embeddings`, and the dense index is on `chunk_embeddings.embedding`:

- `dense.rs:253-264`: finalize drops/creates `chunk_embeddings_embedding_ivfflat_idx ON chunk_embeddings USING ivfflat (embedding vector_l2_ops)`.
- The stamp updates only `chunks.embedding_fingerprint`; it does not change `chunk_embeddings.embedding`, `chunk_embeddings.embedding_fingerprint`, model, or dimension.

The index remains correct. The readiness/coverage failure is from the parent chunk fingerprint column, not missing vectors.

## Readiness / Cache Refresh

No physical index or embedding manifest refresh is required for producer-side correctness.

But after a raw update, delete `index_manifest['query_readiness']` as shown above. Source reason:

- `readiness.rs:119-153`: the producer/local `query_readiness` key is a cache whose mere presence means fully ready at cache time.
- `readiness.rs:179-187`: ingest/embed mutation paths normally invalidate it before changing coverage.
- A raw update bypasses that invalidation.

If a stale cached value was already absent or already incomplete, deletion is harmless. If a stale “ready” value exists, deleting it forces the next producer-local readiness/status path to recompute against the repaired rows. This is not needed for client apply of the rebuilt package, because syncd stamps readiness inside the client apply transaction (`syncd/apply.rs:1106-1111`), but it is the right producer-side hygiene.

I do not recommend refreshing `replay_snapshot` in step (a). It is a separate observability digest and expensive; the later rebuild/publish gate can decide whether to refresh.

## Role And Concurrency

Running as `postgres` is acceptable and simplest for this one-off repair. There is no RLS or trigger behavior that depends on `jurisearch_write`, and the statements are schema-qualified. Running as `jurisearch_write` would be closer to the application role, but it adds no functional safety here and may hit privilege wrinkles around creating a backup table in `public`.

Re-verify before execution:

```sql
SELECT pid, usename, application_name, state, query
FROM pg_stat_activity
WHERE datname = 'jurisearch'
  AND query ILIKE '%jurisearch%'
  AND state <> 'idle';
```

Also confirm producer services/timers remain stopped as planned.

Cost should be bounded: about 231,678 row updates joined by `chunk_embeddings.chunk_id` primary key and `chunks.chunk_id` primary key. PostgreSQL will dirty those chunk rows and update any indexes that include `embedding_fingerprint` if present, but this is small compared with the 4.79M-row corpus. One transaction is appropriate with no competing embed/finalize run.

## Final Verdict

**GO-with-adjustments.**

Proceed with the stamp as a controlled one-off, but use a persistent backup table, update through that table, assert the 231,678 count, and delete `public.index_manifest['query_readiness']` after the update. No ivfflat rebuild is required for step (a). Step (b) must rematerialize current producer tables or explicitly capture these chunk changes; a plain outbox-only incremental build would not see this raw update.
