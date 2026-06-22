BLOCKER crates/jurisearch-cli/tests/cli_contract.rs:3220 - The requested verification command is not green on the current worktree. `cargo test -p jurisearch-cli` consistently fails in `cite_resolves_local_statutory_citations_and_strict_states`; `RUST_BACKTRACE=1 cargo test -p jurisearch-cli --test cli_contract cite_resolves_local_statutory_citations_and_strict_states -- --nocapture` points to the HTTP-500 error assertion at line 3220. This is outside the Phase D gating diff, and the phase-gate-specific tests passed, but the requested package test command cannot be used as passing release evidence until this is fixed. Fix: update the cite online-500 mock/test to account for current retry behavior, or disable retries for this test path, then rerun `cargo test -p jurisearch-cli`.

WARN crates/jurisearch-cli/tests/cli_contract.rs:24 - `jurisearch_command_without_embedding_env()` clears `JURISEARCH_PHASE1_EXTERNAL_BENCHMARK` but not the new `JURISEARCH_PHASE1_FRANCE_LEGI_BENCHMARK`. The current no-index assertions still pass in this environment, but status contract tests can consume an ambient France-LEGI artifact and become non-hermetic. Fix: add `JURISEARCH_PHASE1_FRANCE_LEGI_BENCHMARK` to the env-removal list.

NIT crates/jurisearch-cli/src/main.rs:3814 - The `check["gating"].as_bool() != Some(false)` filter is intentionally conservative: a missing, null, or non-boolean `gating` field remains gating. That matches the requested default-safe behavior and does not need a code change.

Review notes:
- `claim_allowed` is now exactly "all gating checks pass": `phase1_gate_payload_with` builds `checks`, filters out only `gating: false`, and requires every remaining status to be `pass` at crates/jurisearch-cli/src/main.rs:3811.
- Exactly one runtime check is advisory: `external_expert_annotated_eval` uses `phase1_gate_check_advisory` at crates/jurisearch-cli/src/main.rs:3782, and that helper emits `gating: false` at crates/jurisearch-cli/src/main.rs:4420.
- `france_legi_official_eval` is still gating because it uses `phase1_gate_check` at crates/jurisearch-cli/src/main.rs:3787; that helper emits `gating: true` at crates/jurisearch-cli/src/main.rs:4404.
- The other release checks remain gating: `index_query_ready`, `latest_completed_ingest_run`, `failed_members`, `projection_coverage`, `embedding_coverage`, `replay_snapshot`, `final_embedding_model`, and `reranker_decision` all use `phase1_gate_check` in crates/jurisearch-cli/src/main.rs:3734-3808.
- Policy soundness is correct in code: a passing BSARD alone cannot open the claim because `france_legi_official_eval` remains a gating check and defaults to `pending`; a passing France-LEGI artifact plus all other gating checks can open the claim while a pending advisory BSARD is ignored. The new unit test covers the positive case and the France-LEGI failure re-close at crates/jurisearch-cli/src/main.rs:5312.
- Schema coverage is present: `Phase1GateCheck.gating` is a boolean at crates/jurisearch-core/src/schema.rs:418, and `france_legi_benchmark` is included in `Phase1GateResponse` at crates/jurisearch-core/src/schema.rs:414.

Verification:
- `cargo test -p jurisearch-cli external_benchmark_is_advisory_and_france_legi_gates` passed.
- `cargo test -p jurisearch-cli --test cli_contract status_returns_json_without_index` passed.
- `cargo test -p jurisearch-cli --test cli_contract status_consumes_external_benchmark_artifact_from_env` passed.
- `cargo test -p jurisearch-cli` failed in `cite_resolves_local_statutory_citations_and_strict_states`, outside the Phase D gate logic.

VERDICT: FIXES_REQUIRED
