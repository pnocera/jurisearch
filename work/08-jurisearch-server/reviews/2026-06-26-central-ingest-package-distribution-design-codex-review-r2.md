# Central ingest package-distribution design review r2

## BLOCKER

None.

## WARN

None.

## NIT

### 1. Traceability table points warn-and-reject to the wrong design invariant

The warn-and-reject contract is now summarized in §13 item 9, but the traceability table still points "Warn-and-reject on unmet conditions" to `§13.8` (`work/08-jurisearch-server/2026-06-26-central-ingest-package-distribution-design.md:760`). §13.8 is now the client-build index invariant, so this is just a stale cross-reference introduced by the new invariant numbering.

Recommended fix: change the traceability row from `§6.3, §11, §13.8` to `§6.3, §11, §13.9`.

## R2 verification notes

The r1 blocker is resolved. Generation granularity is now explicit and per corpus: physical schemas are named `jurisearch_server_<corpus>_gNNNN`, `jurisearch_control.corpus_state` is keyed by corpus, incrementals apply into that corpus's active generation, and baselines/re-baselines repoint only the affected corpus's views while leaving other installed corpora visible. I did not find remaining text that implies one global `jurisearch_server_gNNNN` switch or a whole-server generation that would hide other corpora.

The corrected C7 is accurate against the current source. `documents.updated_at` exists and is stamped in the LEGI document upsert (`crates/jurisearch-storage/src/migrations.rs:43`, `crates/jurisearch-storage/src/projection/legi.rs:45-68`); `legi_metadata_roots.updated_at` exists and is stamped on conflict (`migrations.rs:207`, `projection/metadata.rs:43-56`); and `legislation_citation_resolutions.updated_at` exists and is stamped by occurrence finalization and resolution updates (`migrations.rs:689`, `legislation_citations.rs:142-146`, `:206-209`). The diff-relevant tables named in C7 lack a uniform update watermark: `chunks`, `chunk_embeddings`, `graph_edges`, `zone_units`, `zone_unit_embeddings`, `decision_zones`, and `official_api_responses` do not expose a comparable `updated_at`, and the LEGI chunk/graph upserts do not stamp one (`projection/legi.rs:73-105`). The §5.1 outbox rationale therefore remains consistent.

The cursor/index-build ordering warning is resolved. §7.1 now makes index materialisation part of apply/activation; §7.3 says ordinary incrementals rely on PostgreSQL row-level maintenance inside the apply transaction and that a package adding a new index definition builds before cursor advance; §7.4 finalizes baseline/re-baseline indexes before view switch; §9.3 repeats the same rule; §11.1 says the cursor advances only after data, indexes, and postconditions pass; and §13 item 6 summarizes the invariant. That matches the current finalize discipline in `finalize_dense_rebuild` and `finalize_zone_dense_rebuild`, which verify full coverage, drop/recreate the IVFFlat index, analyze tables, and write `index_manifest` before committing (`dense.rs:93-190`, `zone_units.rs:431-524`).

The document remains design-only: it defines contracts, formats, namespaces, and protocols without an implementation plan, phasing, or code. Other touched code-grounded claims also check out: schema version 17 and `SchemaVersionAhead` are current (`migrations.rs:3`, `:704-754`); deterministic LEGI and jurisprudence IDs match source (`crates/jurisearch-ingest/src/legi/canonical.rs:52-56`, `crates/jurisearch-ingest/src/juri/types.rs:180-184`); derived rebuild evidence matches the cited delete/reinsert/cascade paths (`zone_units.rs:120-169`, `decision_zones.rs:195-207`, `projection/hierarchy_backfill.rs:216-229`); BM25 indexes are defined in migrations; and the current `serve` daemon is a single-client sequential JSONL loopback-oriented transport, not a package-management service.

VERDICT: GO
