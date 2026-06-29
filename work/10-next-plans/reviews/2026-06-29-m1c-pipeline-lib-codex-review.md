# Codex Review - M1-C Pipeline Lib

## Findings

### BLOCKER - `embed_documents` exposes zero-valued request fields that can panic or hang

`crates/jurisearch-pipeline/src/embed.rs:51` forwards the public `EmbedRequest` directly into the extracted embedding paths without validating the fields that the CLI still validates before delegation (`crates/jurisearch-cli/src/ingest.rs:205`, `crates/jurisearch-cli/src/ingest.rs:210`, `crates/jurisearch-cli/src/ingest.rs:273`, `crates/jurisearch-cli/src/ingest.rs:278`). The new library API accepts `batch_size: usize` and `pool_concurrency: usize` at `crates/jurisearch-pipeline/src/embed.rs:29` and `crates/jurisearch-pipeline/src/embed.rs:30`, then `embed_and_insert_with_pool` calls `inputs.chunks(batch_size)` at `crates/jurisearch-pipeline/src/embedding/pool.rs:212`, which panics when `batch_size == 0`. It also computes `worker_count = pool_concurrency.min(...)` at `crates/jurisearch-pipeline/src/embedding/pool.rs:218`; with `pool_concurrency == 0`, no workers are spawned, the result channel closes without any inserts, and the no-limit streaming callers in `crates/jurisearch-pipeline/src/embed.rs:149` and `crates/jurisearch-pipeline/src/embed.rs:322` can reload the same still-pending page forever.

This is a new public API hazard: the previous CLI surface rejected those values, but M1-C now advertises `embed_documents(db, cfg, req) -> Result<EmbedReport, EmbedError>` as the reusable in-process seam. A typed library entrypoint should return the same `bad_input` errors for invalid zero values instead of relying on every non-CLI caller to duplicate CLI validation.

Actionable fix: validate `limit != Some(0)`, `batch_size > 0`, and `pool_concurrency > 0` at the start of `embed_documents` (or before each inner call) and return `ErrorObject::bad_input` through `EmbedError`. Add focused pipeline tests that call `embed_documents` or the pool driver with zero `batch_size` / zero `pool_concurrency` and assert an error rather than a panic, hang, or zero-worker success.

### WARN - The fingerprint regression test does not cover the `base_url` half of the stated invariant

The review contract says `request_model` and `base_url` must never affect `storage_embedding_fingerprint()`. The current regression test only varies `request_model` (`crates/jurisearch-pipeline/src/embed.rs:407` through `crates/jurisearch-pipeline/src/embed.rs:440`). The implementation is correct today because `EmbeddingFingerprint::storage_embedding_fingerprint()` only formats `model`, `dimension`, and `normalize` (`crates/jurisearch-embed/src/fingerprint.rs:17` through `crates/jurisearch-embed/src/fingerprint.rs:21`), but this test would false-green if a future change accidentally let `base_url` or `base_url_class` into the storage fingerprint.

There is also a misleading test comment at `crates/jurisearch-pipeline/src/embed.rs:403` that says the storage fingerprint keys on provider / base URL class / pooling, which contradicts the actual implementation. Actionable fix: extend the regression test to clone the same model/dimension/normalize config across materially different `base_url` values, including different base-url classes, and assert equal storage fingerprints; update the comment to list the actual storage-fingerprint fields.

## Notes

I did not find a dependency-direction violation: `jurisearch-pipeline` does not depend on or import `jurisearch-cli` / `jurisearch-query` in source or Cargo metadata. The local `ErrorObject` helper copies match the `jurisearch-query` constructors by source. The S7 package-build changes are signature-only in the reviewed bodies and still obtain independent clients for the main connection and the outbox fence connection.

VERDICT: FIXES_REQUIRED
