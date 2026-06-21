# Claude Review — Chunk Provenance Materialization in Storage

Verdict: GO

- Step reviewed: uncommitted Phase 1.2 chunk-provenance materialization (baseline `10283bb`)
- Reviewer: Claude (Opus 4.8), 2026-06-21

The change is correct, backward-compatible, and embedding-safe. Migration 6 materializes existing
canonical-JSON chunk fields into columns without invalidating or drifting existing embeddings; the
insert, hierarchy-backfill, embed, and fetch paths are kept consistent. Storage + CLI tests pass and
`clippy --all-targets -D warnings` is clean. Only non-blocking suggestions below.

## Why GO

### Migration 6 is backward-safe and drift-free
- `ALTER TABLE … ADD COLUMN IF NOT EXISTS` is idempotent; `chunking`/`boundary`/`hierarchy_path` use
  constant `NOT NULL DEFAULT`s (metadata-only on PG11+), and `contextualized_body` is nullable
  (`migrations.rs:244-275`).
- The backfill `UPDATE … SET contextualized_body = COALESCE(NULLIF(d.canonical_json->'chunks'->c.chunk_index->>'contextualized_body',''), c.contextualized_body) …`
  mirrors the **exact** value the old embed path read (`canonical_json->chunks->[chunk_index]->contextualized_body`).
  So for an existing v5 index the column is populated to the same text the existing embeddings were
  computed from — and the migration does **not** clear `embedding_fingerprint` or delete
  `chunk_embeddings`. Result: existing embeddings stay valid, no forced re-embed, and a later
  `embed-chunks` re-run produces identical `embedding_text` (no silent drift). Verified by tracing
  both the old `contextualized_body()` helper (removed) and the new column read.
- `chunking`/`boundary`/`hierarchy_path` were already serialized in `CanonicalChunk`/`canonical_json`,
  so the backfill materializes real existing values (`boundary='article'`, etc.), with the
  `'structural'`/`'unknown'`/`'[]'` defaults only catching missing/legacy data. The
  `jsonb_typeof(...->'chunks')='array'` guard avoids touching malformed rows.

### Read/write paths stay consistent (no canonical_json ↔ columns divergence)
- I confirmed the **only** writers of `documents.canonical_json` are the insert
  (`projection.rs:68`) and the hierarchy-backfill `UPDATE documents` (`projection.rs:436`). Both now
  also write the chunk columns — the insert from `CanonicalChunk` fields (`projection.rs:178`), and
  the backfill via the new `update_chunks` statement (`projection.rs:444-456`, executed at
  `:489`) which re-derives the columns from the freshly enriched JSON, in the **same transaction**
  as the document update and embedding invalidation. No other path mutates `canonical_json`, so the
  materialized columns cannot drift from it.
- The backfill's `update_chunks` matches `c.chunk_index = ordinality-1` (1-based ORDINALITY → 0-based
  index), consistent with the migration's `->c.chunk_index` direct access and with
  `CanonicalChunk.chunk_index` being the array position.

### Embedding and fetch behavior is safe
- `load_chunk_embedding_inputs` now reads `c.contextualized_body` directly (dropping the per-chunk
  `documents` join + JSON reparse — a real simplification) and uses
  `contextualized_body.filter(|t| !t.trim().is_empty()).unwrap_or(body)` (`dense.rs:62-76`). NULL
  binds to `Option<String>` (no panic), and the `trim().is_empty()` filter preserves the old
  whitespace-only → body fallback even though the migration's `NULLIF(...,'')` only collapses exact
  empties. So embedding text is identical to the previous behavior. The `dense_rebuild` test was
  strengthened to prove exactly this: it sets a document-level `contextualized_body` of
  `"ignored document-level context"` but a different chunk-column value, and a NULL column with a
  fallback body — confirming the column wins and NULL falls back.
- Fetch additively returns the four new fields from columns (`retrieval.rs:241-244`); a NULL
  `contextualized_body` simply serializes as JSON null. Non-breaking.

### Tests
- `schema_migrations` (migration 6 present, 4 columns exist, fresh-insert defaults `structural:unknown:[]`),
  `dense_rebuild` (column-over-JSON + NULL fallback), and the `cli_contract` ingest test
  (post-backfill chunk shows `contextualized_body LIKE '%Titre preliminaire%Article 1240%'`,
  `hierarchy_path->>3 = 'Titre preliminaire'`, `structural:article`) all pass locally; clippy clean.

## Non-blocking suggestions

1. **Cover the migration backfill path for pre-existing rows.** Every test exercises either the new
   insert path or the column defaults; the one path not directly asserted is migration 6's
   `UPDATE chunks … FROM documents` that populates the columns for chunks that existed *before* the
   migration. That is precisely the path that matters when an operator upgrades a populated v5 index
   and then re-embeds. The logic mirrors the old read and the risk is low, but a focused test (insert
   a chunk with NULL/default provenance columns but a populated `canonical_json`, run the backfill
   SQL, assert the columns are filled and match the JSON) — or an ignored upgrade test on a real
   index — would close it. Pair it with an assertion that re-running `embed-chunks` after the
   migration yields the same `embedding_text` (drift guard).
2. **Operator note for large-index migration cost.** Migration 6's backfill `UPDATE` rewrites every
   `chunks` row once (whole-table update under the startup migration). On a corpus-scale existing
   index this is a one-time but potentially long, locking operation at startup; worth a line in the
   plan/runbook so an upgrade isn't mistaken for a hang.
3. **Minor cleanliness.** `load_chunk_embedding_inputs` no longer has a fallible op inside the map
   closure, so the `Ok(...)` wrap + `.collect::<Result<Vec<_>, StorageError>>()` (`dense.rs:71-76`)
   could simplify to an infallible `.collect()`; harmless as-is.
4. **Consider whether `contextualized_body` should be the lexical/BM25 source.** Out of scope here
   (search is untouched), but now that contextualized text is a first-class column, a later slice may
   want the BM25 index to use it rather than `body`; noting so it isn't lost.

## Verification performed

- Read the full diffs for `migrations.rs`, `projection.rs`, `dense.rs`, `retrieval.rs`, and the
  three test files; traced the migration backfill vs. the removed `contextualized_body()` helper to
  confirm value equivalence and no embedding invalidation.
- Grepped all `documents`/`chunks` mutators to confirm there is no `canonical_json` writer that skips
  the column refresh (only insert + hierarchy backfill, both updated).
- Ran `cargo test -p jurisearch-storage --test schema_migrations --test dense_rebuild` (3 passed),
  `cargo test -p jurisearch-cli ingest_legi_archives_records_accounting_and_quarantines_failures`
  (1 passed), and `cargo clippy -p jurisearch-storage -p jurisearch-cli --all-targets -- -D warnings`
  (clean). Did not separately re-run the ignored `legi_canonical_retrieval` end-to-end test (the
  author reports it green; the embedding-text equivalence is established by code tracing and the
  `dense_rebuild` assertions).
