I have everything I need. I've verified the implementation against the schema, the retrieval path, the embed-crate fingerprint/manifest conventions, the existing test patterns, and confirmed it compiles.

# Review: Phase 0.6 Dense Rebuild Finalization (commit 7414b08)

Verdict: GO

## Summary
The slice adds `jurisearch_storage::dense::finalize_dense_rebuild`, a storage-level primitive that (1) validates full chunk→embedding coverage for a target fingerprint/model/dimension, (2) rebuilds the ivfflat ANN index, and (3) upserts the `embedding` manifest with coverage and index parameters — all in one transaction. It correctly scopes itself as a storage finalizer and the plan edits explicitly leave the endpoint-driven embedding loop as "Remaining." This is acceptable as the storage-level dense rebuild slice.

## What's correct
- **Coverage validation.** Because `chunk_embeddings.chunk_id` is the PK/FK to `chunks` (1:0 or 1:1), the `LEFT JOIN … WHERE ce.chunk_id IS NULL OR fingerprint/model/dimension <>` count cannot double-count and faithfully captures every chunk lacking a *matching* embedding. The early `missing != 0` return happens before any writes, so a failed coverage check leaves no partial state. Fails closed.
- **ANN index rebuild.** `DROP INDEX IF EXISTS` + `CREATE INDEX … ivfflat (embedding vector_l2_ops)` is idempotent and re-runnable. Index name (`chunk_embeddings_embedding_ivfflat_idx`), method, and operator class match exactly what `retrieval.rs` relies on (`ce.embedding <-> …`, L2) and what `target_spike_corpus.rs` builds — so the rebuilt index is actually usable by the search path.
- **Transaction/lifecycle safety.** Everything is wrapped in one `client.transaction()`. Non-concurrent `CREATE INDEX` and `ANALYZE` are both transaction-safe in Postgres, so the whole rebuild is atomic and rolls back on any failure. Choosing transactional atomicity over `CREATE INDEX CONCURRENTLY` availability is a reasonable, defensible trade-off for a finalizer.
- **Manifest semantics.** Upserts under a dedicated `embedding` key (distinct from the migrations' `schema` key) via `ON CONFLICT (key) DO UPDATE`, so re-runs are clean. Records coverage and full index params. `value` cast `$1::text::jsonb` is correct.
- **Injection safety.** `embedding_fingerprint`/`model`/`dimension` flow only through bound parameters; the only `format!`-interpolated SQL values are the compile-time constant index name and the validated `u32` `index_lists`.
- **Error handling & types.** Validation/coverage use the new `StorageError::DenseRebuild`; PG errors map to `PostgresClient`. `i32`→`integer`, `count(*)`→`i64` bindings are correct. Compiles cleanly (`cargo check -p jurisearch-storage --tests`).
- **Tests.** The integration test exercises both the coverage-failure path and the happy path, then independently verifies the index exists in `pg_indexes` and asserts the manifest contents — not just the returned report. Gated via `discover_pg_config`, consistent with the crate's established PG-integration pattern.

## Non-blocking suggestions
1. **Manifest `normalize` is re-parsed from the fingerprint string** (`spec.embedding_fingerprint.contains(":normalize:true")`). This happens to be correct given the canonical format `model:dim:normalize:bool` (`EmbeddingFingerprint::storage_embedding_fingerprint`), but it's fragile. When the endpoint loop wires `EmbeddingConfig` into this finalizer, prefer sourcing `normalize` (and `model`/`dimension`) from the structured config rather than substring-matching.
2. **Manifest divergence from `jurisearch-embed::EmbeddingManifest`.** The embed-crate manifest carries `provisional` + `reembeddable`; this storage manifest hardcodes `reembeddable: true` and omits `provisional`. Fine for now, but the two representations should be reconciled when the embedding loop owns config, so `reembeddable`/`provisional` reflect reality instead of being constants.
3. **No cross-check between `spec.model`/`spec.dimension` and the fingerprint's embedded segments.** A mismatch fails closed (coverage `missing != 0`) but yields a confusing "chunks missing embeddings" error rather than a clear "spec inconsistent" one. A cheap up-front consistency check would improve diagnostics.
4. **Zero-chunk corpora trivially "succeed"** (`missing = 0`), building an empty ivfflat index and writing a manifest that asserts a rebuild. Consider rejecting or at least logging an empty rebuild.
5. **Validation paths are untested.** `validate_dense_spec` (empty fingerprint/model, `dimension != 1024`, `index_lists == 0`) is a pure function — a non-PG unit test would lock in those error paths cheaply, and a coverage variant with a *mismatched* model/dimension (not just an outright-missing embedding) would broaden the integration test.
6. **`dimension != 1024` is a magic constant** duplicated from the schema's `vector(1024)`/`CHECK (dimension = 1024)`. Consider a shared constant so the two can't drift.

None of these block the slice. It proves storage can finalize a full re-embed from reinserted `chunk_embeddings` rows without overclaiming the deferred endpoint loop.
