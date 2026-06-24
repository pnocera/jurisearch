# Code Review: `/tmp/juri-merge.sh`

## Findings

### BLOCKER: The script can leave the target clone half-mutated on any late failure

The target writes are split across many independent autocommit `psql` sessions. Step 1 deletes readiness/replay caches and drops the BM25/vector/graph indexes before any data copy (`/tmp/juri-merge.sh:18-23`). Each `copy_slice` invocation then opens a new source `psql` and a new target `psql`, so each target `COPY` commits separately (`/tmp/juri-merge.sh:26-29`, then callers at `:33-73`). The final rebuild block is also not wrapped in `BEGIN`; for example `UPDATE chunks SET embedding_fingerprint = '$FP'` commits before the missing-embedding check can fail (`/tmp/juri-merge.sh:76-157`).

That violates the brief's "must not corrupt the clone" requirement. A failure in a later step, such as `ingest_run`/`legi_metadata_roots` collision, interrupted `COPY`, missing embedding detected by the DO block, or `CREATE INDEX` failure, would leave the clone with some LEGI rows inserted, some indexes dropped, deleted `query_readiness`/`replay_snapshot`, and possibly rewritten chunk fingerprints. The repo's own comparable paths avoid this: `finalize_dense_rebuild` starts a transaction before counting, validating, updating fingerprints, rebuilding the dense index, and writing the manifest, then commits only at the end (`crates/jurisearch-storage/src/dense.rs:98-100`, `:122-161`, `:182-192`); migrations are likewise wrapped in `BEGIN ... COMMIT` (`crates/jurisearch-storage/src/migrations.rs:436-442`).

Actionable fix: make target mutation atomic or make the clone replaceable only after a successful merge. One practical shell-level fix is to first export all source slices to temporary binary files using source-only `COPY TO`, then run a single destination `psql` script with `ON_ERROR_STOP=1`, `BEGIN`, the index drops, `\copy ... FROM '<temp-file>' WITH (FORMAT binary)` for every table, validation, index/manifest rebuild, and `COMMIT`; run `ANALYZE` after commit if needed. If using one destination transaction is too awkward, run the entire merge against a fresh throwaway clone path and atomically promote that path only after post-merge validation succeeds.

### WARN: Default `status` will lose replay-snapshot availability until a deep refresh runs

The script deletes `index_manifest` entries for both `query_readiness` and `replay_snapshot` at the start and again at the end (`/tmp/juri-merge.sh:18-23`, `:156`). Invalidating `query_readiness` is correct: production query readiness recomputes live projection/embedding coverage and caches only a fully ready report (`crates/jurisearch-storage/src/ingest_accounting.rs:862-897`). `replay_snapshot` is different: default `status` uses cached replay snapshots, and only `status --deep` refreshes them (`crates/jurisearch-cli/src/main.rs:492-495`, `:7005-7010`). With the key deleted, `load_ingest_health_with_replay_snapshot_mode(... Cached)` reports the replay snapshot as missing (`crates/jurisearch-storage/src/ingest_accounting.rs:692-706`), which makes the Phase 1 `replay_snapshot` check pending (`crates/jurisearch-cli/src/main.rs:7366-7423`).

This does not appear to block `phase2_gate` directly, because `phase2_gate` checks query readiness, cass/capp/inca+jade corpus-source presence, honest `zone_accurate=false`, and the Phase 2 benchmark (`crates/jurisearch-cli/src/main.rs:8071-8124`). It does mean the merged clone will not have a complete default status/gate picture until a replay snapshot is refreshed.

Actionable fix: after the atomic merge commits, run the supported deep status/refresh path against the clone (`jurisearch status --deep`, with the same Phase 2 benchmark env that will be used for release validation) or call the existing replay-snapshot refresh path, and require it to succeed before declaring the clone ready.

### WARN: The script prints counts but does not enforce the gates it says the result must pass

The final section prints source/count summaries only (`/tmp/juri-merge.sh:159-164`). It does not fail the script if production query readiness or `phase2_gate.claim_allowed` is false. The relevant code computes query readiness from full live projection and embedding coverage (`crates/jurisearch-cli/src/main.rs:8478-8499`; `crates/jurisearch-storage/src/ingest_accounting.rs:905-953`) and computes `phase2_gate` from query readiness, jurisprudence corpus-source coverage, honest zone provenance, and the Phase 2 benchmark (`crates/jurisearch-cli/src/main.rs:8065-8133`).

Actionable fix: add explicit post-merge validation after the atomic data/index transaction, using the actual CLI/env the release gate will use. At minimum, fail unless `index.query_ready == true` and `phase2_gate.claim_allowed == true`, and include a read-only smoke query that exercises the merged corpus.

## Verification Notes

- The BM25 DDL in the script matches migration v9 (`crates/jurisearch-storage/src/migrations.rs:350-376`), and the reverse-citation graph index matches migration v10 (`crates/jurisearch-storage/src/migrations.rs:378-397`).
- The hand-written embedding manifest shape and dense index parameters match `finalize_dense_rebuild` (`crates/jurisearch-storage/src/dense.rs:151-180`). The current target manifest already uses `bge-m3:1024:normalize:true`, `model=bge-m3`, `dimension=1024`, `normalize=true`, `provisional=true`, `reembeddable=true`.
- Live read-only checks found no `ingest_run.run_id` collision between the source LEGI run and target jurisprudence runs, no existing LEGI documents or metadata roots in the target, no LEGI failed members in the source, and no LEGI graph edges pointing at missing documents.
- Live read-only coverage checks found source LEGI has `1,736,165` documents, `1,852,745` chunks, `1,852,745` matching embeddings, zero projection misses, and zero embedding misses. The target clone has `1,144,796` documents, `2,848,609` chunks, `2,848,609` matching embeddings, zero projection misses, and zero embedding misses. Combined coverage should therefore remain complete if the merge commits atomically.
- The existing target jurisprudence `corpus_sources` inputs are present for `cass`, `capp`, `inca`, and `jade`, all with `zone_accurate=false`; `phase2_gate` intentionally considers only those jurisprudence sources and ignores LEGI for that check (`crates/jurisearch-cli/src/main.rs:8074-8090`).

VERDICT: FIXES_REQUIRED
