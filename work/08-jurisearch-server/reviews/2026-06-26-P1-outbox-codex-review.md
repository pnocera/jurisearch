# P1 Outbox Review

## Summary

Phase 1 is mostly present in the working tree: migration v19 adds `package_change_log`; `OutboxContext` is threaded through the archive ingest, embedding, zone-unit, hierarchy-backfill, official API archive, decision-zone, and citation writers; `scopes_changed_for_corpus` reads only global `change_seq` windows; and `index_manifest`, `schema_migrations`, and `ingest_*` remain intentionally hookless.

I do not think this is ready to ship yet. The core same-transaction guarantee is violated in the scheduled enrichment/citation paths because those paths pass a plain auto-commit `postgres::Client` into helpers that perform the data mutation first and the outbox insert afterward. There is also a QA-backstop weakness: several digest signatures omit replicated row content, so the digest can miss real drift.

## BLOCKER

### Auto-commit enrichment/citation writers can commit replicated rows without their outbox rows

The implementation claims same-transaction emission, but the affected helpers are generic over `postgres::GenericClient` and are called with a bare `postgres::Client` in scheduled producer commands. With a bare client, each `execute`/`query_one` is its own auto-commit statement; the later `emit_change` is not rollback-coupled to the preceding mutation.

Concrete paths:

- `crates/jurisearch-storage/src/official_api_archive.rs:71` inserts `official_api_responses`, then `crates/jurisearch-storage/src/official_api_archive.rs:111` emits the outbox row. `archive_exchange` is called by scheduled `ingest enrich-zones` and `ingest enrich-legislation-citations`; CodeGraph shows the production callers are `enrich_decision_from_judilibre_with_client` and `enrich_legislation_citations_payload`.
- `crates/jurisearch-storage/src/decision_zones.rs:152` upserts `decision_zones`, `crates/jurisearch-storage/src/decision_zones.rs:205` may delete `zone_units`, and only then `crates/jurisearch-storage/src/decision_zones.rs:219` emits `decision_zones` and optional `zone_units` `replace_set` rows. The scheduled `ingest enrich-zones` path opens one plain client per worker at `crates/jurisearch-cli/src/ingest/pipeline.rs:152` and passes it through at `crates/jurisearch-cli/src/ingest/pipeline.rs:162`.
- `crates/jurisearch-storage/src/legislation_citations.rs:84`, `crates/jurisearch-storage/src/legislation_citations.rs:155`, and `crates/jurisearch-storage/src/legislation_citations.rs:279` mutate citation occurrence/resolution rows before their outbox emits at `:113`, `:172`, and `:298`. The scheduled commands create a plain client at `crates/jurisearch-cli/src/enrichment/legislation.rs:184` and `:308` and pass it into those helpers at `:237`, `:260`, `:358`, and `:392`.

Failure mode: if an outbox insert fails after the data statement succeeds, the replicated table mutation remains committed but the ledger misses it. For `decision_zones`, a second failure window exists after the zone-unit invalidating delete: the overlay row and delete can commit while either the `decision_zones` or `zone_units` `replace_set` row is absent. That is exactly the silent diff-gap risk P1 is meant to eliminate.

Recommended fix: make outbox-enabled producer mutations execute inside an explicit `postgres::Transaction`. For `enrich-zones`, start a transaction per decision after the HTTP calls are known, pass `&mut tx` into `archive_exchange`, `cache_zone_status_with_client`, and `upsert_decision_zones_with_client`, and commit only after all data mutations and outbox emits for that decision succeed. For citation collection/enrichment, wrap each decision/citation unit similarly, with `official_api_responses` and the dependent resolution update in the same transaction. Consider making the `Some(outbox)` variants accept `&mut Transaction` or adding storage-owned transactional entrypoints so this cannot regress through a bare `GenericClient`. Add rollback tests for the Judilibre and legislation paths that force an error after the data write but before/inside `emit_change` and assert neither data nor ledger rows survive.

## WARN

### The QA digest helper is not a full content backstop for several replicated tables

`corpus_table_digests` is intended to be the section 5.4 row-count plus ordered-hash backstop, but several signatures omit non-volatile replicated columns:

- `documents` have `citation`, `title`, `body`, `source_url`, and `canonical_json` in `crates/jurisearch-storage/src/migrations.rs:27`, but the digest at `crates/jurisearch-storage/src/outbox.rs:345` only includes identity, validity, and `source_payload_hash`.
- `chunks` have `body`, `source_fields`, and embedding state in `crates/jurisearch-storage/src/migrations.rs:46`, but the digest at `crates/jurisearch-storage/src/outbox.rs:354` omits those row contents.
- `chunk_embeddings` and `zone_unit_embeddings` digest only key/fingerprint/model at `crates/jurisearch-storage/src/outbox.rs:363` and `:396`, not the vector or dimension.
- `decision_zones`, `decision_legislation_citations`, `legislation_citation_resolutions`, and `official_api_responses` similarly omit replicated fields such as raw JSON/body/error/status details, response IDs, request fingerprints, occurrence raw fields, schema versions, or provider metadata. Compare the signatures at `crates/jurisearch-storage/src/outbox.rs:403`, `:411`, `:419`, and `:427` with the schemas around `crates/jurisearch-storage/src/migrations.rs:593` and `:650`.

This can produce a false "digest matches" result even when a client-applied staging DB differs from the producer in non-volatile replicated content. The digest is only a backstop, not the primary diff source, so this is not the same severity as the outbox transaction gap, but it weakens the P1 acceptance claim for the QA scaffold.

Recommended fix: compute each table signature from all replicated, non-volatile columns, excluding only columns intentionally machine-local or time-volatile (`created_at`, `updated_at`, `fetched_at` where appropriate). A robust pattern is `md5(to_jsonb(row_without_volatile_columns)::text)` or an explicit `jsonb_build_object` with every replicated column, including stable hashes of vector values. Add tests that mutate one currently omitted column, for example `official_api_responses.error` or a `decision_zones.raw_json` value, and assert the digest changes.

### The digest ordering is not encoded in the aggregate itself

The digest query sorts inside the `scoped` CTE at `crates/jurisearch-storage/src/outbox.rs:263`, then calls `string_agg(sig, '|')` at `:267` without an aggregate `ORDER BY`. PostgreSQL does not make aggregate input order a contract unless the aggregate call orders it, and the whole purpose of this helper is a deterministic ordered digest.

Recommended fix: carry the sort key out of the CTE and use `string_agg(sig, '|' ORDER BY sort_key)` or build each digest query with an aggregate-local `ORDER BY {order_by}`. Add a small test that inserts rows out of PK order and confirms the digest is stable.

## NIT

### The enumerated coverage test does not actually prove hook coverage

The table inventory is useful, but `section_4_2_replicated_set_is_fully_classified` only compares `SECTION_4_2_TABLES` to `REPLICATED_DIGEST_SPECS` (`crates/jurisearch-storage/src/outbox.rs:319` and `:440`). It does not exercise or statically bind any writer to an outbox emit, despite the assertion message saying every replicated data table has an outbox hook. A table can be marked `Hooked` with no actual `emit_change` call and this test still passes.

Recommended fix: keep the inventory, but add an enumerated integration test that drives one owned writer per section 4.2 table/group and asserts the expected outbox rows. Encode the intentional document-scope grouping explicitly, for example `documents` scopes cover `chunks` and `graph_edges`, `zone_units` `replace_set` covers cascaded `zone_unit_embeddings`, and control/operational tables stay hookless.

VERDICT: FIXES_REQUIRED
