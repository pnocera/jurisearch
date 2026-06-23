# Phase 1 Code Review r2

## Findings

No blocking findings.

## Verification

- The qrels relevance universe issue is resolved. `eval_run_payload` now builds the metric universe from the retrieved pool union all label keys before calling `compute_eval_metric`, so relevant qrel documents that no retriever returned still count in recall and IDCG (`crates/jurisearch-cli/src/main.rs:1358`).
- The BM25-only embedding dependency is resolved. `eval_run_payload` computes `needs_dense`, uses `QueryReadinessGate::SearchLexical` when no requested mode uses dense retrieval, constructs `PreparedQueryEmbedder` only under `needs_dense`, and only embeds questions when that optional embedder exists (`crates/jurisearch-cli/src/main.rs:1197`, `crates/jurisearch-cli/src/main.rs:1202`, `crates/jurisearch-cli/src/main.rs:1210`, `crates/jurisearch-cli/src/main.rs:1227`).
- Precision@k now uses the standard denominator. `compute_eval_metric` divides hits by `spec.k`, so missing ranks in a short returned list count as non-relevant instead of inflating precision (`crates/jurisearch-cli/src/main.rs:1045`).
- `doctor` now reports the previously omitted checks without opening the index. It emits an `embedding_fingerprint` check from the loaded config and an explicit `index_schema_and_readiness` warn that points users to `status --deep` for schema, migration, replay, query-readiness, and index compatibility checks that require opening the index (`crates/jurisearch-cli/src/main.rs:5528`, `crates/jurisearch-cli/src/main.rs:5543`).
- The command contract/help drift is resolved. `SearchRequest` now includes `group_by` with `chunk`/`document` enum values and default `chunk`, and the `related` clap help no longer advertises the command as a stub (`crates/jurisearch-core/src/schema.rs:84`, `crates/jurisearch-cli/src/main.rs:151`).

## Notes

- I reviewed the HEAD diff and the affected source paths directly. I did not rerun the test suite because the request restricted filesystem changes to this review file, and a Cargo test run may update build artifacts.
- Residual test coverage gap: the existing `cli_contract` schema test still asserts `mode`, `format`, and `cursor`, but does not pin the new `SearchRequest.group_by` field. I do not consider this a blocker for r2 because the schema source itself now exposes the field and the prior functional drift is fixed.

VERDICT: GO
