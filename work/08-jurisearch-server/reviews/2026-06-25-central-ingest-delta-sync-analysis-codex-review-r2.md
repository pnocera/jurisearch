# Re-review r2

## Prior findings

### WARN - derived rows are deleted/rebuilt: RESOLVED

The revised document now scopes the append-only/mostly-additive claim to the base legal corpus and separately calls out derived retrieval/index-support tables. The cited source ranges support that distinction: `replace_zone_units_for_document` deletes all `zone_units` for a decision before reinserting the current derivation (`crates/jurisearch-storage/src/zone_units.rs:120-145`), `decision_zones` invalidation deletes materialized `zone_units` when a row is not derivable (`crates/jurisearch-storage/src/decision_zones.rs:195-204`), and hierarchy backfill deletes `chunk_embeddings` while clearing fingerprints (`crates/jurisearch-storage/src/projection/hierarchy_backfill.rs:209-229`). The bottom line is consistent with this narrowed framing.

### WARN - existing local `sync`: RESOLVED

The new clarification is accurate: `sync_payload` is local official-source archive delta ingestion over archive plans (`crates/jurisearch-cli/src/ingest.rs:24-89`), `ArchiveSyncFilter` skips the baseline and selects deltas for incremental sync (`crates/jurisearch-cli/src/ingest.rs:330-358`), and the CLI advertises `sync` as a STUB for official-source deltas/transactional histories (`crates/jurisearch-cli/src/args.rs:135-138`). It is not server-to-client corpus replication. Minor wording caveat: the implementation supports jurisprudence archive sources as well as LEGI, so "DILA/LEGI delta archives" is narrower than the code, but the core distinction is correct.

### NIT - IVFFlat/BM25 replication evidence: RESOLVED

The revised mechanism-A constraint and risk item now separate repo-confirmed facts from unverified external claims. The repo confirms `vector` and `pg_search` extensions (`crates/jurisearch-storage/src/migrations.rs:24-25`), BM25 indexes via `USING bm25` (`crates/jurisearch-storage/src/migrations.rs:355-369`, `crates/jurisearch-storage/src/migrations.rs:559-573`), and IVFFlat indexes built locally (`crates/jurisearch-storage/src/dense.rs:151-160`, `crates/jurisearch-storage/src/zone_units.rs:489-498`). The document no longer asserts physical-replication safety and correctly leaves pgvector IVFFlat / pg_search BM25 standby behavior to upstream verification or a smoke test.

## Newly introduced issues

None requiring changes.

VERDICT: GO
