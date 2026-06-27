# P8 Writable-App Reference Model + Validator Review

## Findings

### WARN 1. The `jurisearch_app` preservation test does not snapshot enough state

The source implementation keeps `jurisearch_app` out of the package reload paths, but the P8 regression test does not yet prove the full invariant it claims. In `crates/jurisearch-package-build/tests/reference_validation.rs:239-252`, the before/after assertion around `apply_incremental` compares only:

`target_kind`, `document_id`, `validation_status`, and `resolved_document_id`.

That can miss a reload regression that mutates other app-reference state, including `reference_id`, `corpus`, `source`, `source_uid`, `version_group`, `as_of_date`, `anchor_json`, `resolved_generation`, `resolved_schema_version`, or `validated_at`. It also only wraps the incremental apply. The re-baseline apply at `crates/jurisearch-package-build/tests/reference_validation.rs:283` has no equivalent before/after assertion, even though the acceptance contract is specifically about preserving writable app state across re-baselines.

The current production code I inspected does not write `jurisearch_app` from the apply path: `jurisearch_app` is outside `REPLICATED_TABLES` (`crates/jurisearch-storage/src/generations.rs:31-51`), and activation writes `jurisearch_control.corpus_state` plus dense metadata/views rather than app tables (`crates/jurisearch-storage/src/generations.rs:1068-1118`). This is therefore a test/proof gap, not an observed app-table mutation. But the brief explicitly asks that the `before == after` snapshot genuinely prove the reload does not mutate `jurisearch_app`; the current partial projection can false-green.

Concrete fix: replace the partial `string_agg` with a helper that snapshots the complete row shape, for example:

```sql
SELECT coalesce(jsonb_agg(to_jsonb(r) ORDER BY reference_id)::text, '[]')
FROM jurisearch_app.app_reference AS r;
```

Assert that full snapshot across both `apply_incremental` and `apply_rebaseline`, before any subsequent `validate_references` call. If P8 later adds more app tables, snapshot all tables in `jurisearch_app` or add table-specific full-row snapshots so the test cannot miss semantic identity or validation-stamp writes.

## Contract Checks

- Migration v24 is present and `CURRENT_SCHEMA_VERSION` is 24 (`crates/jurisearch-storage/src/migrations.rs:24`, `crates/jurisearch-storage/src/migrations.rs:1080-1126`). `jurisearch_app.app_reference` has the requested identity columns, `anchor_json jsonb`, `validation_status` default/check values, target-kind check values, and the four requested indexes. I did not find a hard cross-schema FK.
- `validate_references` reads `active_generation` and `schema_version`, resolves against the active physical generation, and stamps the read generation/schema in the same transaction body (`crates/jurisearch-storage/src/reference.rs:117-177`).
- Missing corpus handling marks that corpus's app references `missing` with no fallback to `public` or `jurisearch_server` views (`crates/jurisearch-storage/src/reference.rs:137-153`).
- The validator write set is narrow: `update_reference` updates only `resolved_document_id`, `resolved_generation`, `resolved_schema_version`, `validated_at`, and `validation_status` (`crates/jurisearch-storage/src/reference.rs:370-392`).
- Pinned `document_id` resolution is always `resolved` when the row exists, independent of generation/schema stamp changes (`crates/jurisearch-storage/src/reference.rs:268-288`).
- Logical resolution uses the required half-open validity window, including `$5::text::date < d.valid_to`, and falls back from `version_group` to `source_uid` (`crates/jurisearch-storage/src/reference.rs:318-349`).
- `changed` is only produced when a prior non-null `resolved_document_id` now differs (`crates/jurisearch-storage/src/reference.rs:352-368`).
- The new test's logical `changed` assertion is exercised by a real supersession: V1 is closed at `2022-01-01`, V2 is inserted, the incremental is built/applied, and the logical reference then resolves to `legi:A1@2022` with `changed` (`crates/jurisearch-package-build/tests/reference_validation.rs:193-260`).
- Pin survival across re-baseline is grounded in package contents, not magic: media packages copy the producer's replicated public tables from a repeatable-read snapshot (`crates/jurisearch-package-build/src/baseline.rs:259-335`), and the test proves the pin resolves from the new `core_g0002` generation (`crates/jurisearch-package-build/tests/reference_validation.rs:285-303`).

## Verification

- Read `/tmp/codex-review-p8-instructions.md` first.
- Inspected the uncommitted P8 diff plus untracked `crates/jurisearch-storage/src/reference.rs`, `crates/jurisearch-package-build/tests/reference_validation.rs`, and `qa/20260627-042419-i-m-about-to-implement-p8-writable-app-r.md`.
- Used CodeGraph for the `validate_references` implementation, apply-path callers, and package-build context.
- `git diff --check` passed.
- I did not rerun the cargo suites; the brief reports `cargo fmt --check`, `cargo clippy`, `cargo test -p jurisearch-storage`, `cargo test -p jurisearch-package-build`, and `cargo test -p jurisearch-cli` already green.

VERDICT: FIXES_REQUIRED
