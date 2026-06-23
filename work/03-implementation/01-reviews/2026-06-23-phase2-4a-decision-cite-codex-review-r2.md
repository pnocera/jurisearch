# Codex review r2 - Phase 2.4-A: decision cite fixes

Review target: `53bd691` on `main`, diff vs `a253739`.

## BLOCKER

None.

The r1 blocker is closed. `ParsedCitationTarget::DecisionDocumentId` now separates `cass:`/`capp:`/`inca:`/`jade:` decision document IDs from statutory `DocumentId` targets (`crates/jurisearch-cli/src/main.rs:7967-7985`, `crates/jurisearch-cli/src/main.rs:8081-8099`). It still resolves through `CitationLookup::DocumentId`, preserving document-id/source-uid lookup behavior (`crates/jurisearch-cli/src/main.rs:7995-8025`), but `classify_citation_state` handles it in the decision existence-based arm (`crates/jurisearch-cli/src/main.rs:8305-8313`). That arm uses raw match count, so an existing decision cannot become `stale_version` under an `--as-of` date before `decision_date`.

`normalized_value()` includes `DecisionDocumentId`, so the normalized payload remains the original document id (`crates/jurisearch-cli/src/main.rs:8062-8078`), and `input_class()` now reports `decision_document_id` (`crates/jurisearch-cli/src/main.rs:8035-8048`).

## WARN

None.

The r1 online warning is closed. `is_decision()` covers all four decision variants: decision document id, source uid, ECLI, and pourvoi (`crates/jurisearch-cli/src/main.rs:8051-8060`). The `source_unavailable` relabel now excludes decisions (`crates/jurisearch-cli/src/main.rs:2934-2946`), so a missing decision with `--online` remains `not_found`. The only caller of `apply_online_citation_confirmation` is still `cite_payload`, and that call is now behind the malformed and decision branches (`crates/jurisearch-cli/src/main.rs:2966-2984`), so decision targets do not reach the Légifrance statutory probe.

The statutory `--online` path is unchanged for non-malformed, non-decision LEGI targets: empty local matches still prelabel as `source_unavailable`, and non-decision online requests still call `apply_online_citation_confirmation` (`crates/jurisearch-cli/src/main.rs:2936-2944`, `crates/jurisearch-cli/src/main.rs:2983-2984`).

## NIT

None.

The `parse_pourvoi` doc comment now documents the actual 4-6 digit right-hand group and keeps the examples aligned with the parser (`crates/jurisearch-cli/src/main.rs:8323-8340`).

## Checks Performed

- Reviewed `crates/jurisearch-cli/src/main.rs` parser, lookup, input class, normalized value, decision detection, state classification, and online branching for the r2 diff.
- Reviewed `crates/jurisearch-storage/src/citation.rs:31-160` to confirm `CitationLookup::DocumentId` behavior remains the shared document-id/source-uid lookup that `DecisionDocumentId` intentionally reuses.
- Reviewed `crates/jurisearch-cli/tests/cli_contract.rs:3400-3455`; the regression coverage now asserts decision document-id `--as-of` + `--strict` succeeds as `exact`, decision `--online` is a Judilibre no-op, and missing decision `--online` remains `not_found`.
- Ran `cargo test -p jurisearch-cli --test cli_contract cite`: 3 passed.
- Ran `cargo test -p jurisearch-cli --bin jurisearch tests::parse`: 2 passed.
- Ran `git diff --check a253739..53bd691 -- crates/jurisearch-cli/src/main.rs crates/jurisearch-cli/tests/cli_contract.rs`: passed.
- Ran `cargo clippy -p jurisearch-cli`: passed with pre-existing warnings in `jurisearch-official-api` and unrelated existing `jurisearch-cli` sites; no r2-specific warning was observed.

VERDICT: GO
