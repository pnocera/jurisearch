# Code Review: A5 Contract Tests

## Findings

### WARN - Explicit `--authority-weight 0.0` is not regression-tested on a real successful search path

The A5 plan requires `--authority-weight 0.0` to produce the same output as unset for the same query, including no `publication`, no `authority` block, and legacy pagination. The current zero-weight coverage at `crates/jurisearch-cli/tests/cli_retrieval_contract.rs:1547` only proves that `0.0` bypasses pre-index authority validation and reaches `index_unavailable` with `--kind code`. It would not catch a regression where a real successful `--kind decision --authority-weight 0.0` search projects `publication`, disables pagination, changes `next_cursor`, or emits `authority` metadata.

Concrete fix: extend `search_authority_rerank_wires_projection_pagination_and_metadata` or add a neighboring test using the same seeded decision index to run the exact same query once with no authority flag and once with `--authority-weight 0.0`, then assert the parsed JSON responses are identical after any inherently variable fields are accounted for. At minimum, assert the zero-weight response has no response/candidate authority block, no `publication`, and `pagination.cursor_supported == true` with the same `next_cursor` behavior as unset.

### WARN - The Phase 2 gate isolation tests cover only the CLI flag, not artifact/gate consumer isolation

`crates/jurisearch-cli/tests/cli_eval_contract.rs:80` verifies that `eval france-juris` rejects `--authority-weight`, which is useful, but A5 also calls out the gate/consumer contract: the Phase 2 artifact validator should still accept the unchanged `phase2_france_juris_benchmark` artifact, and gate re-derivation should ignore/reject any separate authority benchmark artifact because it is not a gate input. Existing tests cover the normal France-juris artifact shape in `crates/jurisearch-cli/src/tests.rs`, but this patch does not add the requested regression that a future `phase2_authority_benchmark` artifact cannot accidentally satisfy `JURISEARCH_PHASE2_BENCHMARK`.

Concrete fix: add a focused gate test next to `phase2_benchmark_re_derives_pass_and_rejects_bad_artifacts` that writes a plausible `{"kind":"phase2_authority_benchmark","gate_input":false,...}` artifact to a temp path, passes it through `phase2_benchmark_payload_with_path`, and asserts the resulting benchmark status remains failed/not accepted for the Phase 2 full-juridic gate. Keep the existing `eval france-juris` knob-free CLI test.

## Verification

- `cargo test -p jurisearch-storage`
- `cargo test -p jurisearch-cli --test cli_retrieval_contract -- --nocapture`
- `cargo test -p jurisearch-cli --test cli_eval_contract`
- `cargo test -p jurisearch-cli --test cli_session_contract`

VERDICT: FIXES_REQUIRED
