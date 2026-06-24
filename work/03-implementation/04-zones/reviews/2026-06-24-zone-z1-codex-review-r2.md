# Codex review: zone retrieval Z1 r2

Reviewed `origin/main..HEAD` (`8813cf5` + `c0b6427`) in `/home/pierre/Work/jurisearch`, focusing on the new zone-unit storage path, schema migrations v13-v15, retrieval helper extraction, and the follow-up fixes from the first saved review.

## Findings

### WARN: The changed Rust files are not rustfmt-clean

The behavioral fixes in `c0b6427` compile and the focused tests pass, but a direct formatter check over the changed Rust files still fails. `rustfmt --edition 2024 --check crates/jurisearch-storage/src/zone_units.rs crates/jurisearch-storage/tests/zone_units.rs crates/jurisearch-storage/src/retrieval.rs crates/jurisearch-storage/src/migrations.rs crates/jurisearch-storage/src/lib.rs` reports diffs in the new/touched files, including `crates/jurisearch-storage/src/zone_units.rs:46`, `crates/jurisearch-storage/src/zone_units.rs:455`, `crates/jurisearch-storage/tests/zone_units.rs:28`, and `crates/jurisearch-storage/tests/zone_units.rs:148`.

Impact: if formatting is a required CI/review gate for this repository, this branch will fail despite the storage tests being green. Even if workspace-wide formatting currently also reports pre-existing files, the newly added `zone_units` module and test are part of the formatter failure.

Actionable fix: format the changed Rust files before merging, preferably with the repository's normal formatter command. If avoiding unrelated churn is important, apply rustfmt only to the changed files listed above and re-run the same `rustfmt --edition 2024 --check ...` command.

## Previously reported blockers

- The expired-cache derivation issue is fixed: `load_derivable_decision_zones_json` now requires `(z.expires_at IS NULL OR z.expires_at > now())`, and `expired_ok_rows_are_refresh_candidates_not_derivable` covers the refresh-vs-derive split.
- The cross-document replace issue is fixed: `replace_zone_units_for_document` rejects rows whose `ZoneUnitRow.document_id` differs from the top-level `document_id`, and `replace_zone_units_rejects_foreign_document_rows` covers it.
- The derivation scope issue is fixed: derivation now requires `d.kind = 'decision'`, `d.source IN ('cass','inca')`, and the parser-valid pourvoi predicate, with `derivation_enforces_cassation_scope` covering a foreign-source row.

## Verified behavior

- Working tree was clean before writing this review file; the branch was two commits ahead of `origin/main`.
- `CURRENT_SCHEMA_VERSION` is now `15`, and migrations v13-v15 add isolated `zone_units`, `zone_unit_embeddings`, and `zone_units_bm25_idx` objects without changing the existing chunk tables.
- The T1.3 retrieval helper extraction is narrow: the chunk path now delegates to `effective_rrf_weights` / `effective_probes`, and the added tests cover default and override behavior.
- The new zone-unit integration tests exercise derivation, stale builder-version selection, embedding input loading, dense finalize coverage, coverage JSON, NULL-hash re-enrichment, expired-row freshness, Cassation-only derivation scope, and cross-document replace rejection.

## Tests run

- `git diff --check origin/main..HEAD -- crates/jurisearch-storage/src/migrations.rs crates/jurisearch-storage/src/retrieval.rs crates/jurisearch-storage/src/zone_units.rs crates/jurisearch-storage/tests/zone_units.rs`
- `CARGO_TARGET_DIR=/tmp/jurisearch-review-target cargo check -p jurisearch-storage`
- `CARGO_TARGET_DIR=/tmp/jurisearch-review-target cargo test -p jurisearch-storage retrieval::tests --lib`
- `CARGO_TARGET_DIR=/tmp/jurisearch-review-target cargo test -p jurisearch-storage --test zone_units`
- `rustfmt --edition 2024 --check crates/jurisearch-storage/src/zone_units.rs crates/jurisearch-storage/tests/zone_units.rs crates/jurisearch-storage/src/retrieval.rs crates/jurisearch-storage/src/migrations.rs crates/jurisearch-storage/src/lib.rs` (fails with formatting diffs in changed Rust files)
- `CARGO_TARGET_DIR=/tmp/jurisearch-review-target cargo fmt --check` (also fails; this broader check reports many unrelated pre-existing files as well as the changed files)

VERDICT: FIXES_REQUIRED
