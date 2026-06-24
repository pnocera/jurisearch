# Codex review: zone retrieval Z1 r3

Reviewed `origin/main..HEAD` (`8813cf5`, `c0b6427`, `5399f4a`) in `/home/pierre/Work/jurisearch`, focusing on the new zone-unit storage/migration path, the follow-up fixes from r1/r2, and the refresh lifecycle between `decision_zones` and materialized `zone_units`.

## Findings

### WARN: Negative or invalid zone refreshes leave stale `zone_units` behind

`crates/jurisearch-storage/src/decision_zones.rs:127` updates an existing `decision_zones` row in place, including `status`, `text_hash`, `zones_json`, and `error`, but it does not clear any already-materialized rows in `zone_units`. The new `zone_units` table only cascades from `documents` (`crates/jurisearch-storage/src/migrations.rs:497`), not from `decision_zones`; and the derivation helper only selects fresh `status = 'ok'` rows (`crates/jurisearch-storage/src/zone_units.rs:204`), so a refresh that changes a previously derived row to `not_found`, `unsupported`, `invalid_offsets`, `upstream_error`, or an `ok` row with `text_hash = NULL` will not trigger a replacement or deletion.

Impact: once Z2 retrieval reads `zone_units`, a decision can continue to serve old `zone_accurate=true` official zone units after the cache has been refreshed to say those zones are no longer valid or unavailable. The previous r1 fixes prevent deriving stale/foreign rows, but they do not handle invalidation after a valid row has already been derived.

Actionable fix: make the `decision_zones` refresh/update path invalidate materialized units when the new row is not derivable. The lowest-risk shape is to perform the `decision_zones` upsert and a `DELETE FROM zone_units WHERE document_id = $1` in the same transaction whenever `status <> 'ok'`, `text_hash IS NULL`, the row is expired/negative, or the source is outside the Cassation derivation scope; embeddings will cascade from `zone_units`. Add a regression test that derives units for an `ok` row, then upserts the same `document_id` with `status='not_found'` or `status='invalid_offsets'`, and asserts both `zone_units` and `zone_unit_embeddings` are gone.

## Previously reported items

- The r1 expired-cache derivation issue remains fixed: `load_derivable_decision_zones_json` now requires `(z.expires_at IS NULL OR z.expires_at > now())`, and `expired_ok_rows_are_refresh_candidates_not_derivable` covers the refresh-vs-derive split.
- The r1 cross-document replace issue remains fixed: `replace_zone_units_for_document` rejects rows whose `ZoneUnitRow.document_id` differs from the top-level `document_id`, and `replace_zone_units_rejects_foreign_document_rows` covers it.
- The r1 derivation scope issue remains fixed: derivation now requires `d.kind = 'decision'`, `d.source IN ('cass','inca')`, and the parser-valid pourvoi predicate, with `derivation_enforces_cassation_scope` covering a foreign-source row.
- The r2 formatting issue is fixed for the new zone files: `rustfmt --edition 2024 --check crates/jurisearch-storage/src/zone_units.rs crates/jurisearch-storage/tests/zone_units.rs` is now green.

## Verified behavior

- Working tree was clean before writing this review file; the branch was three commits ahead of `origin/main`.
- `CURRENT_SCHEMA_VERSION` is `15`, and migrations v13-v15 add isolated `zone_units`, `zone_unit_embeddings`, and `zone_units_bm25_idx` objects without changing existing chunk tables.
- The T1.3 retrieval helper extraction is narrow: the chunk path delegates to `effective_rrf_weights` / `effective_probes`, and the added tests cover default and override behavior.
- The focused zone-unit integration tests cover derivation, stale builder-version selection, embedding input loading, dense finalize coverage, coverage JSON, NULL-hash re-enrichment, expired-row freshness, Cassation-only derivation scope, and cross-document replace rejection.
- The r3 finding is not covered by the current tests: no test derives zone units and then refreshes the same `decision_zones` row to a non-derivable status.

## Tests run

- `git diff --check origin/main..HEAD -- crates/jurisearch-storage/src/migrations.rs crates/jurisearch-storage/src/retrieval.rs crates/jurisearch-storage/src/zone_units.rs crates/jurisearch-storage/tests/zone_units.rs crates/jurisearch-storage/src/lib.rs`
- `rustfmt --edition 2024 --check crates/jurisearch-storage/src/zone_units.rs crates/jurisearch-storage/tests/zone_units.rs`
- `CARGO_TARGET_DIR=/tmp/jurisearch-review-target cargo test -p jurisearch-storage --test zone_units`
- `CARGO_TARGET_DIR=/tmp/jurisearch-review-target cargo test -p jurisearch-storage retrieval::tests --lib`
- `CARGO_TARGET_DIR=/tmp/jurisearch-review-target cargo check -p jurisearch-storage`

VERDICT: FIXES_REQUIRED
