# Design Validation: Stop Re-ingest From Nulling Valid Chunk Fingerprints

Verdict: **GO-with-adjustments**.

The proposed shape is correct: preserve `chunks.embedding_fingerprint` when a re-projected chunk's embedded text did not change, and make the embed selector consume a NULL parent fingerprint so genuinely invalidated chunks are re-embedded. This addresses the exact failure mode that made `core-1-2` un-appliable.

The adjustment is to make the conflict-update expression null on content change explicitly, not merely assign `EXCLUDED.embedding_fingerprint`. Today production ingest passes `NULL`, so the proposed SQL works for the current bug. But the shared projection API also has callers/tests that can pass `Some(fingerprint)`. If a future or non-ingest caller reprojects changed text with `Some(fp)`, `THEN EXCLUDED.embedding_fingerprint` would certify a stale vector. Use `THEN NULL` for the changed-text conflict path.

Recommended chunk assignment:

```sql
embedding_fingerprint = CASE
  WHEN chunks.body IS DISTINCT FROM EXCLUDED.body
    OR chunks.contextualized_body IS DISTINCT FROM EXCLUDED.contextualized_body
  THEN NULL
  WHEN EXCLUDED.embedding_fingerprint IS NOT NULL
  THEN EXCLUDED.embedding_fingerprint
  ELSE chunks.embedding_fingerprint
END
```

The second branch preserves the existing convenience behavior for callers that deliberately seed chunks with a known fingerprint on unchanged conflicts. If you want the absolute minimal production fix, `THEN EXCLUDED.embedding_fingerprint ELSE chunks.embedding_fingerprint` is enough for current `legi`/`juri` ingest because they pass `None`, but it is weaker than the design claim "invalidate on real content change."

## Source Findings

The bug is real. `prepare_legi_projection_statements` owns the shared chunk UPSERT and currently sets `embedding_fingerprint = EXCLUDED.embedding_fingerprint` on every `chunk_id` conflict (`crates/jurisearch-storage/src/projection/legi.rs:79`). Jurisprudence reuses that exact statement via `DocumentProjectionStatements = LegiProjectionStatements` (`crates/jurisearch-storage/src/projection/decisions.rs:5`). Production LEGI and JURI ingest both pass `None` as `chunk_embedding_fingerprint` (`crates/jurisearch-pipeline/src/ingest/legi.rs:543`, `crates/jurisearch-pipeline/src/ingest/juri.rs:474`).

The embed selector currently ignores the replicated parent stamp. `load_chunk_embedding_inputs_with_client` selects a chunk only when the `chunk_embeddings` row is missing or has mismatched fingerprint/model/dimension (`crates/jurisearch-storage/src/dense.rs:89`, `:103`). `embed_chunks_inner` returns `NoResults` before `finalize_dense_rebuild_with_client` if that selector returns no rows (`crates/jurisearch-pipeline/src/embed.rs:212`), and the producer treats `NoResults` as a no-op. So a NULL parent fingerprint with a matching child embedding can persist until publish/apply readiness rejects it.

The readiness gates do require the parent stamp. The baseline/rebaseline preflight rejects any `chunks.embedding_fingerprint IS DISTINCT FROM $active` (`crates/jurisearch-package-build/src/cycle.rs:863`). Client generation readiness counts a chunk as embedded only when both `chunks.embedding_fingerprint` and `chunk_embeddings.embedding_fingerprint` equal the active fingerprint (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:297`).

## PostgreSQL Semantics

The CASE can safely compare old and proposed values in one `ON CONFLICT DO UPDATE` statement. PostgreSQL documents that `ON CONFLICT DO UPDATE` `SET`/`WHERE` clauses can access the existing row by the table name or alias and the proposed row via `excluded`; see the official `INSERT` docs: <https://www.postgresql.org/docs/current/sql-insert.html>. The same docs describe `DO UPDATE SET column = expression`, and the `UPDATE` docs state that assignment expressions can use old values of the row: <https://www.postgresql.org/docs/current/sql-update.html>.

So in:

```sql
ON CONFLICT (chunk_id) DO UPDATE SET
  body = EXCLUDED.body,
  contextualized_body = EXCLUDED.contextualized_body,
  embedding_fingerprint = CASE
    WHEN chunks.body IS DISTINCT FROM EXCLUDED.body THEN NULL
    ELSE chunks.embedding_fingerprint
  END
```

`chunks.body` is the pre-update conflicting row, and `EXCLUDED.body` is the proposed insert row. Co-assigning `body = EXCLUDED.body` in the same `SET` list does not make the CASE see the new body through `chunks.body`.

## Change Signal

`body` plus `contextualized_body` is the right minimal invalidation signal for chunk embeddings. The selector maps rows into `embedding_text` by preferring non-empty `contextualized_body`, otherwise `body` (`crates/jurisearch-storage/src/dense.rs:120`). If both values are unchanged, the text sent to the embedding endpoint is unchanged.

`source_payload_hash` is not a better primary signal. A source payload can change while the projected embedded text remains identical; re-embedding then buys nothing. Conversely, a chunker/parser change can alter `body` or `contextualized_body` even if the upstream source payload hash is the same, so text comparison is safer than payload-hash comparison for vector validity.

Cases where identical text still needs a new vector are already modeled elsewhere: current fingerprint/model/dimension mismatches are selected through the existing `ce.embedding_fingerprint <> $1`, `ce.model <> $2`, and `ce.dimension <> $3` clauses. A change to embedding behavior not represented in the storage fingerprint would remain an existing system-level fingerprint-definition problem, not a regression introduced by this fix.

## Changed-Body Flow

With the adjusted projection SQL, a conflicting chunk whose `body` or `contextualized_body` changed gets `chunks.embedding_fingerprint = NULL`.

Add `OR c.embedding_fingerprint IS NULL` to both branches of `load_chunk_embedding_inputs_with_client`:

```sql
WHERE c.embedding_fingerprint IS NULL
   OR ce.chunk_id IS NULL
   OR ce.embedding_fingerprint <> $1
   OR ce.model <> $2
   OR ce.dimension <> $3
```

Do this in both the limited and unlimited selector queries.

That selector reads `c.body, c.contextualized_body` after projection, so the embed input is the new text. `embed_and_insert_chunks_with_pool` computes a fresh vector from those inputs and calls `insert_chunk_embeddings_with_client` (`crates/jurisearch-pipeline/src/embedding/pool.rs:336`). The writer first stamps `chunks.embedding_fingerprint = s.embedding_fingerprint` when the parent is NULL or already equal, then upserts `chunk_embeddings`, including `embedding = EXCLUDED.embedding` on conflict (`crates/jurisearch-storage/src/projection/embeddings.rs:87`, `:128`). Therefore the old vector is overwritten.

Today, the changed-same-`chunk_id` path is broken: projection can leave the old `chunk_embeddings` row in place while changing chunk text, the selector sees the child row as current, and publish/apply coverage can be satisfied only if the parent stamp remains non-null. The proposed fix is a strict correctness improvement.

## No Regression

Fresh ingest is unchanged. A new chunk inserted with `embedding_fingerprint = NULL` has no child embedding, so it is selected and embedded as before.

The selector addition does not over-select unchanged replays after the projection fix. On a re-process with identical `body` and `contextualized_body`, the CASE preserves the existing parent fingerprint, so `c.embedding_fingerprint IS NULL` is false and the existing child-match clauses decide as before.

The dense finalizer remains a backstop, not the primary repair path. It already checks child coverage, stamps all parent rows whose fingerprint differs, rebuilds ivfflat, and writes `index_manifest['embedding']` (`crates/jurisearch-storage/src/dense.rs:145`). This change just ensures invalidated rows reach the embed writer/finalizer instead of `NoResults`.

## Resume Interaction

Compatible resume skips are unaffected. On `IngestResumeAction::Skip`, JURI returns before projection (`crates/jurisearch-pipeline/src/ingest/juri.rs:397`), and LEGI has the same skip-before-projection shape. The bug only bites on `Process` or `Retry`, including incompatible resume and real source changes.

On re-process but unchanged text, the adjusted CASE preserves the existing parent fingerprint. That is the desired common path for a hand-loaded or replayed corpus whose bytes parse identically.

## Blast Radius

One projection statement fixes both LEGI and jurisprudence. `decisions.rs` aliases the LEGI statement, and `rg` shows no second `INSERT INTO chunks ... ON CONFLICT (chunk_id)` projection variant. Other `embedding_fingerprint = EXCLUDED.embedding_fingerprint` hits are child embedding upserts, zone-unit embedding upserts, and generation metadata; they are not this replicated-parent chunk projection bug.

The selector change affects the chunk embed path only. It should be made in both query branches in `dense.rs`; otherwise limited/manual and unlimited/producer paths diverge.

## Zone Units

Do not include zone units in this change. `zone_units` are materialized by `replace_zone_units_for_document_with_client`, which deletes all units for a document and reinserts the current set (`crates/jurisearch-storage/src/zone_units.rs:140`). The new rows have no parent fingerprint until `insert_zone_unit_embeddings_with_client` stamps them. Because `zone_unit_embeddings` cascade-delete with `zone_units`, changed/replaced units are selected by missing child embeddings. This is not the same "unchanged conflict reproject nulls the parent while child remains matching" failure.

Zone-unit coverage has its own parent-stamp gate, but the current derivation and embedding path already line up with that model.

## Test Guidance

Add focused tests around the storage projection/dense selector boundary:

1. Reproject an existing chunk with identical `body` and `contextualized_body`, passing `None`; assert the old `chunks.embedding_fingerprint` is preserved.
2. Reproject the same `chunk_id` with changed `body`; assert `chunks.embedding_fingerprint` becomes NULL and `load_chunk_embedding_inputs_with_client` returns it even when `chunk_embeddings` still has the old matching fingerprint/model/dimension.
3. Insert a fresh chunk with NULL parent and no child; assert selector behavior remains unchanged.
4. If preserving the public API behavior matters, reproject unchanged text with `Some(fp)` over a NULL parent and assert it stamps to `fp`.

## Bottom Line

**GO-with-adjustments**: implement the projection CASE and add `OR c.embedding_fingerprint IS NULL` to both chunk selector queries. Use `THEN NULL` on real text change, with an optional `EXCLUDED.embedding_fingerprint IS NOT NULL` branch for unchanged direct-stamp callers. This preserves valid replays, invalidates genuinely stale vectors, causes changed chunks to be re-embedded from their new text, and does not require a zone-unit parallel fix.
