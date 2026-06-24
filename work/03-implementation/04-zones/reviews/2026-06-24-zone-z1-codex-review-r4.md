# Codex review: zone retrieval Z1 r4

Reviewed `origin/main..HEAD` (`8813cf5`, `c0b6427`, `5399f4a`, `dd86eee`) in `/home/pierre/Work/jurisearch`, focusing on the Z1 storage/migration slice and the r3 follow-up invalidation fix.

## Findings

No blocking findings.

## Previously reported items

- The r3 stale-materialization issue is fixed. `upsert_decision_zones_with_client` now deletes `zone_units` for the refreshed `document_id` whenever the incoming cache row is non-`ok` or hashless (`crates/jurisearch-storage/src/decision_zones.rs:162-174`), and `zone_unit_embeddings` cascade through the v14 foreign key (`crates/jurisearch-storage/src/migrations.rs:532-533`).
- The new regression covers the expected lifecycle: derive one zone unit, insert an embedding, refresh the same decision to `not_found`, then assert both `zone_units` and `zone_unit_embeddings` are empty (`crates/jurisearch-storage/tests/zone_units.rs:403-485`).
- The r1 expired-cache derivation fix remains in place: `load_derivable_decision_zones_json` requires a fresh row with `(z.expires_at IS NULL OR z.expires_at > now())` (`crates/jurisearch-storage/src/zone_units.rs:204-207`).
- The r1 derivation-scope fix remains in place: derivation is limited to `kind = 'decision'`, `source IN ('cass','inca')`, and parser-valid pourvoi rows (`crates/jurisearch-storage/src/zone_units.rs:207-209`).
- The r1 cross-document replacement fix remains in place: `replace_zone_units_for_document` rejects rows whose `document_id` differs from the document being replaced before deleting anything (`crates/jurisearch-storage/src/zone_units.rs:100-124`).

## Verified behavior

- Working tree was clean before writing this review file; the branch was four commits ahead of `origin/main`.
- `CURRENT_SCHEMA_VERSION` is now `15`, with separate `zone_units`, `zone_unit_embeddings`, and `zone_units_bm25_idx` objects (`crates/jurisearch-storage/src/migrations.rs:3`, `:524-580`).
- The Z1 retrieval helper extraction remains narrow: existing chunk retrieval delegates to shared `effective_rrf_weights` / `effective_probes`, and the added unit tests cover defaults, overrides, and numeric formatting.
- The focused zone-unit integration suite covers derivation, embedding insertion, dense finalize coverage, coverage JSON, NULL-hash re-enrichment, expired-row freshness, Cassation-only derivation scope, cross-document replace rejection, and non-derivable refresh invalidation.

## Tests run

- `git diff --check origin/main..HEAD -- crates/jurisearch-storage/src/migrations.rs crates/jurisearch-storage/src/retrieval.rs crates/jurisearch-storage/src/zone_units.rs crates/jurisearch-storage/tests/zone_units.rs crates/jurisearch-storage/src/lib.rs crates/jurisearch-storage/src/decision_zones.rs`
- `CARGO_TARGET_DIR=/tmp/jurisearch-review-target cargo test -p jurisearch-storage --test zone_units`
- `CARGO_TARGET_DIR=/tmp/jurisearch-review-target cargo test -p jurisearch-storage retrieval::tests --lib`
- `CARGO_TARGET_DIR=/tmp/jurisearch-review-target cargo check -p jurisearch-storage`

VERDICT: GO
