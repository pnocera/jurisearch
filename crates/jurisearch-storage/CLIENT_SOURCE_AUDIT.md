# M1-B client-source call-graph audit (work/10 §2 seam S1)

Date: 2026-06-29 · Branch `agent/m1b-external-pg` · Scope: `jurisearch-storage` only.

This is the **deliverable M1-C depends on**: the COMPLETE set of `jurisearch-storage` helpers reachable
from the ingest → enrich → embed extraction path that were `&ManagedPostgres`-typed (or `execute_sql`/psql-
based), each now given a client-source `*_with_client` variant so M1-C can run the pipeline against an
**external** PostgreSQL (`DbClientSource`) and **never edit storage**. The `&ManagedPostgres` wrappers are
kept as thin shims, so all existing callers are unchanged.

## Audit method

`codegraph explore` + a name-level call audit from the payload entrypoints — `build_zone_units_payload`,
`embed_zone_units_payload`, `embed_chunks_payload`, `ingest_legi_archives_payload` /
`ingest_juri_archives_payload`, `enrich_zones_payload`, and the legislation-citation collection path
(`cli/src/enrichment/legislation.rs`) — cross-referenced against every storage `pub fn` that takes
`&ManagedPostgres`. The audited set **matches the survey's illustrative §1 sample exactly — nothing
EXCEEDED it** (the survey already absorbed the r3/r4 helper tail). The many other `&ManagedPostgres`
helpers in storage (retrieval/query/status/apply/accounting-runs/generations/outbox/etc.) are **not on the
ingest→enrich→embed path** and are intentionally out of scope.

## The S1 trait (Deliverable 1)

`backend.rs`: `pub trait DbClientSource { fn client(&self) -> Result<postgres::Client, StorageError>; }`
— a client FACTORY (each call yields a fresh, independent client; the build path opens several at once).
Implemented for `ManagedPostgres` (self-managed), and for the external-PG identities `ConnectionConfig`
and `WriterHandle`. Object-safe. Compile-time proof: `backend::tests::
db_client_source_is_implemented_for_managed_and_external_and_is_object_safe` (passes without live PG).

## The audited helper set (Deliverable 2) + generalization status (Deliverable 3)

| File | Helper | Old mechanism | `*_with_client` variant |
|---|---|---|---|
| `projection/hierarchy_backfill.rs` | `backfill_legi_article_hierarchy_from_metadata_scoped` | own client + tx | **added** `…_scoped_with_client` |
| `ingest_accounting/replay_snapshot.rs` | `refresh_replay_snapshot` | own client | **already existed**: `refresh_replay_snapshot_with_client` |
| `ingest_accounting/readiness.rs` | `invalidate_cached_query_readiness` | own client | **already existed**: `invalidate_query_readiness<C: GenericClient>` |
| `zone_units.rs` | `enrich_zone_candidates_json` | `execute_sql` (psql) | **added** `…_with_client` (psql→`simple_query_text`) |
| `zone_units.rs` | `replace_zone_units_for_document` | own client + tx | **added** `…_with_client` |
| `zone_units.rs` | `load_derivable_decision_zones_json` | `execute_sql` (psql) | **added** `…_with_client` (psql→`simple_query_text`) |
| `zone_units.rs` | `load_zone_unit_embedding_inputs` | own client | **added** `…_with_client` |
| `zone_units.rs` | `insert_zone_unit_embeddings` | own client + tx | **added** `…_with_client` |
| `zone_units.rs` | `finalize_zone_dense_rebuild` | own client + tx | **added** `…_with_client` |
| `zone_units.rs` | `zone_retrieval_coverage_json` | read snapshot (`begin_snapshot`) | **added** `zone_retrieval_coverage_with_client` (shared SQL const) |
| `legislation_citations.rs` | `finalize_citation_occurrence_counts` | own client + tx | **added** `…_with_client` |
| `legislation_citations.rs` | `load_archived_decisions_with_visa_json` | `execute_sql` (psql) | **added** `…_with_client` (psql→`simple_query_text`) |
| `legislation_citations.rs` | `load_pending_citation_resolutions_json` | `execute_sql` (psql) | **added** `…_with_client` (psql→`simple_query_text`) |
| `legislation_citations.rs` | `legislation_citations_coverage_json` | `execute_sql` (psql) | **added** `…_with_client` (psql→`simple_query_text`) |
| `dense.rs` | `load_chunk_embedding_inputs` | own client | **added** `…_with_client` |
| `dense.rs` | `finalize_dense_rebuild` | own client + tx | **added** `…_with_client` |
| `projection/embeddings.rs` | `insert_chunk_embeddings` | own client + tx | **added** `…_with_client` |

The citation WRITE helpers on this path (`insert_citation_occurrence_with_client`,
`upsert_citation_resolution_pending_with_client`, `update_citation_resolution_with_client`) were already
`&mut C: GenericClient`-generic — no shim needed.

## `execute_sql` (psql) → client translation

The psql path (`runtime.rs::execute_sql` → `psql -v ON_ERROR_STOP=1 -qAt -c <sql>`) returns the trimmed
stdout. The exact in-process equivalent is `query::simple_query_text` (made `pub(crate)`), which renders a
result set with the SAME `-qAt` semantics (columns joined by `|`, rows by `\n`, SQL NULL → empty string,
whole output trimmed) — it is the documented drop-in already used by `ReadSnapshot::read_text`. Each
translated helper builds the **byte-identical** SQL string and passes it to `simple_query_text(client, …)`
instead of `postgres.execute_sql(…)`. The producer client connects with the default `search_path`
(resolves to `public`, the producer working set), matching psql's default — same as every other
`execute_sql` helper. Parity is asserted in `tests/client_source_parity.rs`.

Only behavioral delta: a SQL error now surfaces as `StorageError::PostgresClient` instead of
`StorageError::Psql` (both are `StorageError`, both map to the same caller error objects). Err-on-error
behavior is preserved.

## Validation

- `cargo build -p jurisearch-storage` / `cargo build --workspace` — pass.
- `cargo fmt --all --check` — clean. `cargo clippy -p jurisearch-storage --all-targets -- -D warnings` — clean.
- `cargo test -p jurisearch-storage` — the no-PG `DbClientSource` type/object-safety test passes; the
  live-PG parity test (`tests/client_source_parity.rs`) **defers** (skips) here because `pgvector`/
  `pg_search` assets are not discoverable via `JURISEARCH_PG_CONFIG` in this environment. Residual risk:
  the run-time byte-for-byte parity of the translated read helpers is asserted only when PG assets are
  present (run with `JURISEARCH_PG_CONFIG` set / `JURISEARCH_REQUIRE_PG_EXTENSIONS=1`).
