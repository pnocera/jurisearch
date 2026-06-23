## Findings

### Low: regression test proves the invariant but not the fixed standalone backfill entry point

The new `query_readiness_cache_is_trusted_until_invalidated` test establishes the important behavior around the cache: a fully-ready report is cached, a direct `DELETE FROM chunk_embeddings` remains hidden while the cache is present, and explicit invalidation forces a recompute that observes `covered = 0 / total = 1` (`crates/jurisearch-storage/tests/ingest_accounting.rs:383`, `crates/jurisearch-storage/tests/ingest_accounting.rs:407`, `crates/jurisearch-storage/tests/ingest_accounting.rs:413`). That is a useful invariant test, but it would still pass if the new `invalidate_cached_query_readiness(&postgres)` line were removed from `backfill_legi_hierarchy_payload`, because the test never executes the standalone hierarchy backfill path or any mutation helper that is expected to invalidate internally.

This is not a correctness issue in the current patch, because the source now invalidates before calling `backfill_legi_article_hierarchy_from_metadata` (`crates/jurisearch-cli/src/main.rs:2495`, `crates/jurisearch-cli/src/main.rs:2501`). It is a coverage gap relative to the stated regression-test goal. A future follow-up should add a CLI/payload-level regression that pre-populates `query_readiness`, runs `ingest backfill-legi-hierarchy` on a fixture that invalidates embeddings, and asserts the cache row is absent afterward.

## Correctness Checks

Fix #1 closes the previously identified production invariant hole. The standalone backfill command now invalidates `query_readiness` immediately after opening the index and before the storage backfill that can delete `chunk_embeddings` and clear chunk fingerprints (`crates/jurisearch-cli/src/main.rs:2497`, `crates/jurisearch-cli/src/main.rs:2501`, `crates/jurisearch-cli/src/main.rs:2503`). Structural caller inspection shows the production invalidation set is now: `start_ingest_run_with_client` for ingest runs, `embed_chunks_payload` for embedding/dense rebuild work, and `backfill_legi_hierarchy_payload` for standalone hierarchy backfill. The remaining production writes found by source inspection are under those flows: document/chunk upserts happen during ingest, scoped resume backfill runs inside an ingest command that already invalidated at run start, `insert_chunk_embeddings` and `finalize_dense_rebuild` are reached from `embed_chunks_payload`, and status/replay refresh reads or writes manifest state without mutating coverage.

Fix #2 makes replay snapshot identity independent of the runtime readiness cache. `load_replay_snapshot` now excludes only `replay_snapshot` and `query_readiness` from the `index_manifest` snapshot component (`crates/jurisearch-storage/src/ingest_accounting.rs:1000`, `crates/jurisearch-storage/src/ingest_accounting.rs:1006`). The replay-relevant `schema` and `embedding` manifest keys remain included, so this does not drop the manifest state that describes schema or dense-vector rebuild metadata.

I did not rerun the full validation command during this review; this pass was a source review of `ee3117a` plus structural caller checks and literal SQL write-path inspection.

VERDICT: GO
