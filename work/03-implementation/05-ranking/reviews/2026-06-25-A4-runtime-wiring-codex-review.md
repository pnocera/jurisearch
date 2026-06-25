# A4 Runtime Wiring Review

No BLOCKER findings.

No WARN findings.

No NIT findings.

Verification:

- Inspected `SearchExecution::new`, `run_hybrid_candidates`, and `apply_search_response_envelope` for the authority ON/OFF split, widened limit, gated `publication` projection, pre-truncation `authority_rerank`, and first-page-only pagination.
- Inspected `zone_search_payload` for the mirrored authority weight handling, widened limit, gated projection, shared `authority_rerank` call, and first-page-only pagination override.
- Inspected the added CLI retrieval contract coverage for authority metadata, OFF projection absence, cursor rejection, zero-weight inertness, and session validation.
- Ran `cargo test -p jurisearch-cli --test cli_retrieval_contract search_authority_rerank_wires_projection_pagination_and_metadata -- --nocapture` (pass).
- Ran `cargo test -p jurisearch-cli --test cli_retrieval_contract authority -- --nocapture` (pass: 8 tests).
- Ran `cargo test -p jurisearch-storage authority -- --nocapture` (pass: 10 authority unit tests; unrelated filtered test binaries ran 0 tests).

VERDICT: GO
