# Codex Review R2: M1-C Pipeline Library

## Findings

No BLOCKER/WARN/NIT findings.

## Verification Notes

The zero-value embed fix is present at the public seam. `embed_documents` calls `validate_embed_request(&req)` before dispatching to either target implementation and before any `db.client()`, runtime readiness, endpoint-pool, or embedder work (`crates/jurisearch-pipeline/src/embed.rs:51-65`). The helper covers both `EmbedTarget::Chunks` and `EmbedTarget::ZoneUnits`, rejects `limit == Some(0)`, `batch_size == 0`, and `pool_concurrency == 0`, and leaves `index_lists == 0` valid (`crates/jurisearch-pipeline/src/embed.rs:68-91`). The error messages match the CLI's retained contract for `embed-chunks` and `embed-zone-units` (`crates/jurisearch-cli/src/ingest.rs:193-215`, `crates/jurisearch-cli/src/ingest.rs:261-283`).

The no-limit streaming panic/hang paths are now unreachable through `embed_documents`: both chunk streaming and zone-unit streaming receive already-validated `batch_size` and `pool_concurrency` (`crates/jurisearch-pipeline/src/embed.rs:179-205`, `crates/jurisearch-pipeline/src/embed.rs:352-378`). The lower-level pool driver also has a defensive `bad_input` guard before `inputs.chunks(batch_size)` and before worker creation (`crates/jurisearch-pipeline/src/embedding/pool.rs:201-230`).

The new zero-value tests are meaningful for the validation contract and the pool guard: they assert the exact `BadInput` messages for both targets and assert that the pool driver returns an error instead of panicking or hanging (`crates/jurisearch-pipeline/src/embed.rs:448-529`). Source review still remains the proof that the helper is invoked before DB work, because the entrypoint-ordering assertion is not exercised through a fake `DbClientSource`.

The fingerprint regression test now covers the intended base-URL invariant. It compares a single expected storage fingerprint across loopback, localhost, OpenRouter, and a second external host while holding model/dimension/normalize constant, and it keeps the `request_model` invariant checks (`crates/jurisearch-pipeline/src/embed.rs:531-597`). The underlying implementation confirms that `storage_embedding_fingerprint()` uses only `model`, `dimension`, and `normalize` (`crates/jurisearch-embed/src/fingerprint.rs:15-22`), while `base_url_class` remains part of the full runtime fingerprint but not the storage string (`crates/jurisearch-embed/src/config.rs:130-149`).

The prior positives still hold after the rebase. Dependency direction remains one-way: `jurisearch-pipeline` depends on core/embed/ingest/official-api/storage and does not depend on `jurisearch-cli` or `jurisearch-query` (`crates/jurisearch-pipeline/Cargo.toml:12-23`). The local `ErrorObject` helpers still match the `jurisearch-query` constructors for `index_unavailable`, `dependency_unavailable`, `no_results`, and `storage_error_object` (`crates/jurisearch-pipeline/src/error.rs:18-55`, `crates/jurisearch-query/src/errors.rs:10-53`). The S7 package-build changes are signature-only over `DbClientSource`, with the independent main/fence connections preserved in `build_incremental` (`crates/jurisearch-package-build/src/incremental.rs:96-125`) and the remote-manifest/cycle signatures updated without introducing a pipeline dependency (`crates/jurisearch-package-build/src/cycle.rs:59-66`, `crates/jurisearch-package-build/src/remote_manifest.rs:63-82`).

I also re-checked the extraction shape by source: the CLI adapters now delegate to `jurisearch_pipeline::ingest_archives`, `enrich_zones`, and `embed_documents` while injecting only CLI-owned context such as `index_dir` (`crates/jurisearch-cli/src/ingest/legi.rs:21-43`, `crates/jurisearch-cli/src/ingest/juri.rs:21-37`, `crates/jurisearch-cli/src/ingest/pipeline.rs:11-45`, `crates/jurisearch-cli/src/ingest/pipeline.rs:171-223`). The pipeline entrypoints operate over `DbClientSource` and return typed reports/errors (`crates/jurisearch-pipeline/src/ingest/mod.rs:74-84`, `crates/jurisearch-pipeline/src/enrich.rs:68-74`, `crates/jurisearch-pipeline/src/embed.rs:51-65`).

Tests run:

```text
cargo test -p jurisearch-pipeline --locked
cargo test -p jurisearch-package-build --locked
```

Both passed. I did not run live-PG/embedder or PISTE/OpenRouter legs beyond the loopback/package tests available in this environment.

VERDICT: GO
