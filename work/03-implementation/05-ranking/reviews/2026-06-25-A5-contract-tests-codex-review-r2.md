# Code Review: A5 Contract Tests R2

## Findings

### BLOCKER - The Phase 2 gate still does not reject wrong-kind artifacts

`phase2_gate_rejects_a_separate_authority_benchmark_artifact` creates an authority artifact with no `categories`, no `provenance`, and no non-empty `evidence` (`crates/jurisearch-cli/src/tests.rs:375`). That test fails for missing required Phase 2 fields, not because the artifact is a separate `kind`. The actual validator never checks `artifact["kind"]`: `phase2_benchmark_artifact_errors` validates jurisdiction, fingerprint, evidence, provenance, and category floors (`crates/jurisearch-cli/src/gates/phase2.rs:193`), then returns success when those fields satisfy the gate (`crates/jurisearch-cli/src/gates/phase2.rs:272`). A future `phase2_authority_benchmark` that accidentally copies the full France-juris categories/provenance shape could still satisfy `JURISEARCH_PHASE2_BENCHMARK`, which is exactly the isolation failure A5 is meant to prevent.

Concrete fix: make the gate consumer require `kind == "phase2_france_juris_benchmark"` inside `phase2_benchmark_artifact_errors`, and strengthen the regression by starting from a passing `france_juris_artifact`, changing only `kind` to `"phase2_authority_benchmark"` (and optionally `gate_input: false`), then asserting `phase2_benchmark_payload_with_path` re-derives `failed` with a kind-specific diagnostic.

### WARN - The successful `--authority-weight 0.0` regression is not a byte-identity test

The new successful-search coverage parses both command outputs into `serde_json::Value` and compares values (`crates/jurisearch-cli/tests/cli_retrieval_contract.rs:1388`, `crates/jurisearch-cli/tests/cli_retrieval_contract.rs:1414`, `crates/jurisearch-cli/tests/cli_retrieval_contract.rs:1436`). That catches field-level differences, but it would not catch byte-level drift in key order, formatting, or newline behavior. The previous review and A5 invariant call for `--authority-weight 0.0` to be byte-identical to unset on a real successful search, so this can still miss a regression in the exact stdout contract.

Concrete fix: capture `stdout.clone()` for the unset and `0.0` commands, assert `assert_eq!(zero_stdout, off_stdout)` before parsing, and then parse the bytes for the existing semantic assertions.

### WARN - The session path still lacks a successful `0.0` versus unset parity test

A5 calls out the JSONL session search mirror and the acceptance text requires unset and explicit `0.0` to be identical for CLI and session search. The current session coverage only checks validation/routing failures for out-of-range and positive-weight non-decision requests (`crates/jurisearch-cli/tests/cli_retrieval_contract.rs:1602`). It does not run a successful indexed JSONL search with omitted `authority_weight` and `authority_weight: 0.0`, so a session-only regression that treats JSON `0.0` as ON, projects `publication`, disables pagination, or emits authority metadata would not be caught.

Concrete fix: add a JSONL session regression using the same seeded decision index as `search_authority_rerank_wires_projection_pagination_and_metadata`: send one `search` request without `authority_weight` and one with `"authority_weight": 0.0`, then assert the two successful `result` payloads are byte-identical if serialized through the same JSONL output path, or at minimum exactly equal parsed results plus no `publication`/`authority` leakage and legacy cursor support.

## Verification

- Static source review only. I did not run the cargo test suite because the requested deliverable was a saved review file and the remaining issues are visible from the test and gate-validator source.

VERDICT: FIXES_REQUIRED
