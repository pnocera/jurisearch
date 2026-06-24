# Codex review: zone retrieval Z1

Reviewed commit `8813cf5` against `5ef9969`, focusing on migrations v13-v15, `zone_units.rs`, and the T1.3 retrieval helper extraction.

## Findings

### WARN: `load_derivable_decision_zones_json` derives expired `ok` cache rows

`crates/jurisearch-storage/src/zone_units.rs:181` selects derivation candidates with only `z.status = 'ok'` and `z.text_hash IS NOT NULL`; it does not exclude rows whose `decision_zones.expires_at <= now()`. That conflicts with the function's own "fresh `ok`" contract and with the enrichment predicate, which correctly treats expired rows as refresh candidates at `crates/jurisearch-storage/src/zone_units.rs:60`.

Impact: an expired but hash-bearing `ok` row with no/current-missing `zone_units` can be materialized into zone units before the refresh pass runs. Once Z2 retrieval starts reading `zone_units`, those rows would be served as `zone_accurate=true` official zones even though the cache already knows they are expired. A later refresh may repair them via `text_hash`, but the stale-zone exposure exists between derive and refresh.

Actionable fix: add a freshness predicate to the derivable query, for example `AND (z.expires_at IS NULL OR z.expires_at > now())`, or explicitly document and implement an ordering/locking contract that prevents derivation from running before refresh for expired rows. Add a regression test with an expired `ok` row with non-NULL `text_hash` and no `zone_units`; it should be returned by `enrich_zone_candidates_json` but not by `load_derivable_decision_zones_json`.

### WARN: `replace_zone_units_for_document` can write rows for a different document than the document it clears

`crates/jurisearch-storage/src/zone_units.rs:95` accepts both `document_id` and `rows`, deletes units for the top-level `document_id`, then inserts each row using `row.document_id` at `crates/jurisearch-storage/src/zone_units.rs:116`. There is no preflight check that every `ZoneUnitRow.document_id` matches the document being replaced.

Impact: a caller bug can clear document A's units and insert document B's units in the same successful transaction if B has no conflicting `(document_id, zone, fragment_index)` rows. That violates the helper's "replace all of a decision's zone_units" contract and makes the write path less defensive than the dense embedding guard added later in the same module.

Actionable fix: before opening the transaction, reject any row whose `document_id != document_id` with a `StorageError::Projection` message naming the first offending row. Add a unit/integration test that tries to replace `doc-a` with a row for `doc-b` and asserts that neither document's units change.

### WARN: derivation does not enforce the Cassation-only source/kind scope

The Z1 schema and module comments state the subsystem is Cour de cassation only (`cass` + `inca`), and `enrich_zone_candidates_json` gates candidates through `d.kind = 'decision'`, a caller-supplied source, and parser-valid pourvoi checks at `crates/jurisearch-storage/src/zone_units.rs:55`. The derivation query at `crates/jurisearch-storage/src/zone_units.rs:181` only joins `documents` for `d.source`; it does not require `d.kind = 'decision'`, `d.source IN ('cass','inca')`, or a parser-valid pourvoi.

Impact: any manually inserted or future-produced `decision_zones` row with `status='ok'` and a hash can be converted into `zone_units`, including unsupported sources or even non-decision documents. That is not a default retrieval isolation leak today, but it weakens the Option B boundary the migration comments describe.

Actionable fix: mirror the enrichment reachability gate in `load_derivable_decision_zones_json`: require `d.kind = 'decision'`, `d.source IN ('cass','inca')`, and the same parser-valid pourvoi predicate, or enforce equivalent constraints when writing `decision_zones`. Add a negative test for a non-`cass`/`inca` or non-decision row.

## Verified behavior

- `CURRENT_SCHEMA_VERSION` is `15`, migrations are contiguous through v15, and `validate_migration_list` still checks latest migration equality.
- The v15 `zone_units_bm25_idx` analyzer mirrors the v9 chunk BM25 analyzer shape: `ascii_folding`, French stemmer, and French stopwords over the analyzed text field.
- The T1.3 retrieval helper extraction is behavior-preserving for the chunk path: `HybridCandidateQuery` delegates to value-identical `effective_rrf_weights` and `effective_probes`; the generated `hybrid_candidates_json` SQL body is otherwise unchanged in the diff.
- `dense.rs` and `projection.rs` were not changed by this commit.
- The reviewed SQL string interpolation paths use either `sql_string_literal` for dynamic literals or postgres parameters for writes; I did not find an injection issue in the reviewed helpers.
- `insert_zone_unit_embeddings` has the intended missing/conflicting-unit guard, and `finalize_zone_dense_rebuild` has the intended empty-corpus and missing-embedding guards.

## Tests run

- `CARGO_TARGET_DIR=/tmp/jurisearch-review-target cargo test -p jurisearch-storage retrieval::tests --lib`
- `CARGO_TARGET_DIR=/tmp/jurisearch-review-target cargo test -p jurisearch-storage --test zone_units`
- `git diff --check 5ef9969..8813cf5 -- crates/jurisearch-storage/src/migrations.rs crates/jurisearch-storage/src/retrieval.rs crates/jurisearch-storage/src/zone_units.rs crates/jurisearch-storage/tests/zone_units.rs`

VERDICT: FIXES_REQUIRED
