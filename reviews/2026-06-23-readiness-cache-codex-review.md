## Findings

### High: standalone hierarchy backfill leaves a stale query-readiness cache

`backfill_legi_hierarchy_payload` runs as its own `jurisearch ingest backfill-legi-hierarchy` command and does not go through `start_ingest_run_with_client` or `embed_chunks_payload` before mutating readiness-relevant tables. The command opens the index, calls `backfill_legi_article_hierarchy_from_metadata`, and then refreshes the replay snapshot without invalidating `query_readiness` first (`crates/jurisearch-cli/src/main.rs:1672`, `crates/jurisearch-cli/src/main.rs:2495`).

That storage backfill can delete rows from `chunk_embeddings` and clear `chunks.embedding_fingerprint` (`crates/jurisearch-storage/src/projection.rs:482`, `crates/jurisearch-storage/src/projection.rs:489`, `crates/jurisearch-storage/src/projection.rs:499`). Those changes make embedding coverage incomplete. If a previous `search` populated a fully-ready `index_manifest.query_readiness` row, the standalone backfill can leave that row in place after invalidating embeddings. The next `search`/`fetch`/`cite`/`context` hot path calls `load_or_compute_query_readiness`, accepts the manifest row, and skips live coverage checks (`crates/jurisearch-storage/src/ingest_accounting.rs:868`). `ensure_query_readiness` then evaluates the stale report and can return `Ok(())` for `Search` even though live embedding coverage is no longer complete (`crates/jurisearch-cli/src/main.rs:5012`, `crates/jurisearch-cli/src/main.rs:5033`).

This violates the central invariant that a present cache entry means the index is currently query-ready. Fix by invalidating `query_readiness` before the standalone hierarchy backfill mutates documents/chunks/embeddings, preferably before `backfill_legi_article_hierarchy_from_metadata` is called. A regression test should cover: fully ready index, populate cache through `load_or_compute_query_readiness`, run the standalone backfill path or storage backfill that deletes embeddings, then assert the cache is absent and `Search` readiness recomputes/errors until embeddings are rebuilt.

### Medium: `query_readiness` is included in replay snapshot signatures, so a query can change snapshot identity

`load_replay_snapshot` excludes only `replay_snapshot` from the `index_manifest` component (`crates/jurisearch-storage/src/ingest_accounting.rs:1000`). The new `query_readiness` manifest row is therefore part of the replay snapshot. Because `load_or_compute_query_readiness` writes that row as a side effect of a query when the index is fully ready (`crates/jurisearch-storage/src/ingest_accounting.rs:883`), running a retrieval command before the next snapshot refresh can change the replay snapshot manifest count and signature without any ingest, embedding, or source-data change.

The ingest and embed commands currently invalidate the readiness cache before their own snapshot refreshes (`crates/jurisearch-storage/src/ingest_accounting.rs:279`, `crates/jurisearch-cli/src/main.rs:2545`), so their immediate post-write snapshots are not the problematic case. The issue is that `status --replay-snapshot refresh` or the standalone backfill command can snapshot the presence or absence of a purely query-side cache. That makes replay signatures depend on whether someone queried the index, not only on replay-relevant corpus/index state. Exclude `query_readiness` alongside `replay_snapshot` unless the replay contract intentionally treats runtime query caches as part of the deterministic state.

## Correctness Checks

Store-only-when-ready is correct in the production hot path: `load_or_compute_query_readiness` computes both coverage metrics on one `postgres::Client` and writes `query_readiness` only when `coverage_is_complete` passes for both projection and embedding coverage (`crates/jurisearch-storage/src/ingest_accounting.rs:879`, `crates/jurisearch-storage/src/ingest_accounting.rs:883`). `coverage_is_complete` is equivalent to the CLI `coverage_complete` predicate: both require `total > 0 && covered == total` (`crates/jurisearch-storage/src/ingest_accounting.rs:900`, `crates/jurisearch-cli/src/main.rs:4984`).

Gate equivalence looks preserved apart from stale-cache invalidation risk. Projection coverage is still checked for every gate, `Fetch` and `SearchLexical` still return after projection completeness, and `Search` still requires embedding completeness with the same `index_not_query_ready` reasons/messages (`crates/jurisearch-cli/src/main.rs:5017`, `crates/jurisearch-cli/src/main.rs:5026`, `crates/jurisearch-cli/src/main.rs:5033`).

Connection amplification on the hot path is fixed by the refactor. `ensure_query_readiness` now calls `load_or_compute_query_readiness` once; on a cache hit that function uses one connection and one indexed manifest lookup, and on a miss it reuses the same connection for both coverage scans and the optional cache write (`crates/jurisearch-cli/src/main.rs:5012`, `crates/jurisearch-storage/src/ingest_accounting.rs:865`).

I did not rerun the test suite for this review; this pass was source inspection of `d5376e8` and the relevant call/write paths.

VERDICT: FIXES_REQUIRED
