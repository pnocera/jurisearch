# P8 Writable-App Reference Model + Validator Review (Round 2)

## Findings

No BLOCKER/WARN/NIT findings.

The round-1 WARN is resolved in the current source. `app_snapshot` now snapshots the complete `jurisearch_app.app_reference` row shape with `to_jsonb(r)` over the table alias, ordered by `reference_id` (`crates/jurisearch-package-build/tests/reference_validation.rs:131-138`). That includes the identity columns, `anchor_json`, the resolved/stamped columns, `validated_at`, and any other current `app_reference` column, not just the four-column projection from r1.

The preservation assertions now bracket both reload paths before the next validator pass. The incremental path snapshots immediately before and after `apply_incremental`, with no `validate_references` call in between (`crates/jurisearch-package-build/tests/reference_validation.rs:248-255`). The re-baseline path does the same around `apply_rebaseline`, and the subsequent `validate_references` call happens only after the byte-identical assertion (`crates/jurisearch-package-build/tests/reference_validation.rs:286-295`). A reload-time mutation to any `app_reference` column would therefore fail the test.

## Contract Checks

- The app reference table shape remains the v24 soft-reference model in `jurisearch_app`, with no hard cross-schema FK and the requested identity, anchor, resolved, timestamp, status, and index fields (`crates/jurisearch-storage/src/migrations.rs:1078-1124`).
- The reload/apply write surface remains outside `jurisearch_app`: `REPLICATED_TABLES` contains only generation/server data tables (`crates/jurisearch-storage/src/generations.rs:39-51`), incremental apply writes those generation tables and then advances only `jurisearch_control.corpus_state` (`crates/jurisearch-syncd/src/apply.rs:991-1036`, `crates/jurisearch-storage/src/incremental.rs:307-333`), and re-baseline activation writes generation registry/cursor metadata rather than app tables (`crates/jurisearch-syncd/src/apply.rs:69-83`, `crates/jurisearch-storage/src/generations.rs:959-1118`).
- The validator still resolves and stamps app references in one transaction after reading the active corpus cursor (`crates/jurisearch-storage/src/reference.rs:117-177`) and its update statement remains limited to `resolved_document_id`, `resolved_generation`, `resolved_schema_version`, `validated_at`, and `validation_status` (`crates/jurisearch-storage/src/reference.rs:370-395`).
- The test still exercises the intended behavior: first validation stamps V1, the incremental supersession drives the logical row to `changed`, the re-baseline applies before validation, and only the post-rebaseline validator stamps the new `core_g0002` generation (`crates/jurisearch-package-build/tests/reference_validation.rs:191-313`).

## Verification

- Read `/tmp/codex-review-p8-r2-instructions.md` first.
- Re-read the r1 review and verified the single WARN against the current source.
- Used CodeGraph for the reference-validation and validator call path, then inspected the exact current line ranges cited above.
- Did not rerun the cargo commands; the r2 brief reports `cargo fmt --check`, `cargo clippy`, and `cargo test -p jurisearch-package-build --test reference_validation` already green.

VERDICT: GO
