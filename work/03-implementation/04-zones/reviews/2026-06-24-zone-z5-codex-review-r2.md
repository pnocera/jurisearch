# Code Review - Zone Retrieval Z5 R2

Reviewed commit `9b66d30068a7a97c04673409f401bb0ae40b2690` (`Zone retrieval Z5 r1 fixes (codex)`) against the prior R1 review findings and the Z5 binding constraints.

## Findings

No blocking issues found in the reviewed patch.

## R1 Resolution

- The zone-gold SQL now strips Cassation pourvoi-shaped identifiers in addition to ECLI/JURITEXT/CETATEXT. The expression in `crates/jurisearch-storage/src/france_juris.rs:243` correctly uses doubled braces for the Rust `format!` string, covers plain forms such as `12-34567`, and covers dotted forms such as `12-34.567` as a whole before whitespace compaction. The updated storage test in `crates/jurisearch-storage/tests/zone_units.rs:258-323` seeds both forms, asserts both are absent from the emitted query, and asserts semantic zone text still survives.
- The `phase2_zone_benchmark` artifact no longer hardcodes `bge-m3:1024:normalize:true`. `eval_france_juris_zones_payload` computes `needs_dense` from `RetrievalMode::uses_dense()`, passes the same `expected_fingerprint` to `ensure_zone_retrieval_readiness`, and records that value in the artifact; BM25 passes `None`, producing JSON `null`. `PreparedQueryEmbedder::from_env()` derives the query-time storage fingerprint from the same `embedding_config_from_env().storage_embedding_fingerprint()` path, so dense/hybrid provenance matches the readiness check and the actual dense filter. The CLI unit test at `crates/jurisearch-cli/src/main.rs:11454-11492` covers BM25 null/`uses_dense:false` and hybrid non-null/`uses_dense:true`.

## Binding Checks

- The ordinary default/chunk search path and the `phase2_gate` path are not touched by the R2 code changes. The full-juridic `phase2_france_juris_benchmark` artifact remains separate from the measured-only `phase2_zone_benchmark` artifact.
- The zone artifact still uses distinct `kind: "phase2_zone_benchmark"`, `state: "measured"`, and `gate_input: false`, so it cannot satisfy or inflate the full-juridic Phase 2 claim.
- The resolver denominator remains outside the search hot path; this commit does not change the status-only denominator work or route it into zone candidate retrieval.
- `git diff --check b87ac8c..9b66d30` reported no whitespace errors.

## Verification

- `git status --short --branch`
- `git show --find-renames --format=fuller --stat --patch 9b66d30`
- `git diff --check b87ac8c..9b66d30`
- CodeGraph context/explore for `zone_retrieval_sql`, `eval_france_juris_zones_payload`, `zone_benchmark_artifact`, and `ensure_zone_retrieval_readiness`
- `cargo test -p jurisearch-cli zone_benchmark_artifact_records_actual_fingerprint_and_never_gates`
- `cargo test -p jurisearch-storage --test zone_units zone_gold_strips_identifiers_dedupes_and_honors_caps`

VERDICT: GO
