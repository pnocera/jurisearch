# Codex Review: jurisearch-cli Phase 4 Status / Runtime / Gates

## Scope

Reviewed the requested Phase 4 diff:

```sh
git diff 4c9b58e..HEAD -- crates/jurisearch-cli
```

The diff moves index runtime/readiness, embedding runtime/config/pool, status/health payloads, and Phase 1/2 gate logic out of `main.rs` into:

- `crates/jurisearch-cli/src/index_runtime.rs`
- `crates/jurisearch-cli/src/embedding_runtime.rs`
- `crates/jurisearch-cli/src/status.rs`
- `crates/jurisearch-cli/src/gates/{mod.rs,support.rs,phase1.rs,phase2.rs}`

## Findings

No BLOCKER, WARN, or NIT findings.

## Review Notes

The high-risk gate, status, and embedding-pool paths remain behavior-preserving in this diff. I compared the moved function bodies against `4c9b58e:crates/jurisearch-cli/src/main.rs`; the critical bodies are unchanged apart from module relocation and visibility/import changes required by the split.

Verified moved status and index functions include:

- `status_payload`, `status_index_and_ingest_health`, `zone_retrieval_status_block`, `doctor_payload`, `setup_payload`, `model_fetch_payload`, `stats_payload`, `inspect_payload`, `versions_payload`, and `diff_payload`
- `require_existing_index_dir`, `require_configured_index_dir`, `configured_index_dir`, `open_index`, `open_index_for_bulk_ingest`, `coverage_complete`, and `ensure_query_readiness`

Verified moved Phase 1/2 gate functions include:

- Phase 1 claim derivation, replay snapshot check, locked embedding-model check, external benchmark validation, France-LEGI benchmark validation, advisory semantic category handling, and routing-backend accounting
- Phase 2 corpus-source checks, honest zone provenance, fail-closed benchmark payload loading, per-category floor checks, production provenance requirements, and per-identifier ECLI/pourvoi/CETATEXT citation verification

Verified moved embedding runtime functions include:

- endpoint pool config/dedupe/fingerprint matching
- least-outstanding endpoint selection and release accounting
- batch retry handling
- chunk and zone-unit insert wrappers
- endpoint-stat merging
- env/TOML config loading and precedence
- secret redaction for in-process providers
- model-cache and loopback endpoint status probing

The new module wiring follows the stated hub pattern: `main.rs` imports the module surfaces at crate root, and the moved modules use `use crate::*` to resolve the same symbols. The diff keeps the command dispatch surface pointed at the same payload builders, and the gate logic continues to re-derive pass/fail from artifact contents rather than trusting self-reported states.

## Validation

Static/source validation performed:

- `git status --short --branch`
- `git diff --stat 4c9b58e..HEAD -- crates/jurisearch-cli`
- `git diff --name-status 4c9b58e..HEAD -- crates/jurisearch-cli`
- targeted CodeGraph context/explore for `status_payload`, `ensure_query_readiness`, `embed_and_insert_with_pool`, `loaded_embedding_config`, `phase1_gate_payload_with`, and `phase2_benchmark_artifact_errors`
- targeted exact-body comparison of moved functions against `4c9b58e:crates/jurisearch-cli/src/main.rs`

I did not rerun `cargo build` or `cargo test` because this review request explicitly constrained file modifications, and those commands can write build artifacts. The provided validation record says `cargo build -p jurisearch-cli` and `cargo test -p jurisearch-cli` were clean.

VERDICT: GO
