# Codex Review: jurisearch-cli test reorganization

Reviewed diff: `612a8e1..HEAD -- crates/jurisearch-cli`

## Findings

### NIT - Repeated broad imports in each split contract suite

The six new integration suites each keep the full old `cli_contract.rs` import block plus `#![allow(unused_imports)]`, while shared helpers also carry their own imports in `tests/support/mod.rs`. This is behavior-preserving and consistent with the stated copy-first constraint, but it leaves more boilerplate than the split ultimately needs.

Recommended fix: after this behavior-preserving reorganization lands, trim each suite to its actual imports or move the common test-facing imports/helpers behind `support` so the per-suite `allow(unused_imports)` attributes can be removed.

## Completeness Checks

- Contract test inventory is complete. The old `tests/cli_contract.rs` contained 55 unique test functions; the new suites contain the same 55 unique test names with no missing, extra, duplicated, or renamed tests.
- Distribution matches the intended domain split: `cli_eval_contract.rs` has 3 tests, `cli_help_contract.rs` has 3, `cli_ingest_contract.rs` has 18, `cli_retrieval_contract.rs` has 12, `cli_session_contract.rs` has 4, and `cli_status_contract.rs` has 15.
- The two ignored live-embedding tests kept the same names and ignore reason, now split between ingest and retrieval:
  - `ingest_embed_chunks_uses_live_endpoint_and_finalizes_dense_index`
  - `search_returns_results_from_existing_index_with_live_embeddings`
- Shared helper coverage is complete. Comparing old `cli_contract.rs` functions against `tests/support/mod.rs` plus the six new suites found the same 69 function names and no body changes after normalizing only the expected `pub(crate)` visibility added to support helpers.
- Unit-test extraction is complete. The old inline `#[cfg(test)] mod tests` body from `src/main.rs`, with the wrapper removed and one indentation level dedented, is byte-identical to `src/tests.rs` after excluding only the new file-level comment. `src/main.rs` now contains only `#[cfg(test)] mod tests;` at the test-module boundary.
- Cargo discovery matches the expected binaries: `cargo test -p jurisearch-cli -- --list` reports 53 unit tests and 55 contract tests across the six new integration-test binaries.

No blocker or warning findings.

VERDICT: GO
