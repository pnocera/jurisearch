# Code Review: A5 Contract Tests R3

## Findings

No BLOCKER, WARN, or NIT findings.

The current revision addresses the r2 issues:

- `phase2_benchmark_artifact_errors` now rejects artifacts whose `kind` is not `phase2_france_juris_benchmark`, and the regression starts from an otherwise-passing Phase 2 artifact before flipping only the kind.
- The successful CLI `--authority-weight 0.0` regression now compares raw stdout bytes against the unset path before parsing.
- The JSONL session path now has a successful indexed `authority_weight: 0.0` versus omitted parity regression with no authority/publication leakage and legacy cursor support.

## Verification

- `cargo test -p jurisearch-cli phase2_gate_rejects_a_separate_authority_benchmark_artifact`
- `cargo test -p jurisearch-cli eval_france_juris_stays_knob_free_and_rejects_authority_weight --test cli_eval_contract -- --exact`
- `cargo test -p jurisearch-cli search_authority_rerank_wires_projection_pagination_and_metadata --test cli_retrieval_contract -- --exact`
- `cargo test -p jurisearch-cli session_search_authority_weight_zero_matches_unset_on_a_real_search --test cli_retrieval_contract -- --exact`
- `cargo test -p jurisearch-cli zone_search_positive_authority_weight_disables_paging_and_carries_metadata --test cli_retrieval_contract -- --exact`
- `cargo test -p jurisearch-storage decisions_project_search_and_fetch --test decision_projection -- --exact`
- `cargo test -p jurisearch-storage zone_candidates_json_scopes_to_zone_with_official_provenance --test zone_units -- --exact`

VERDICT: GO
