# Codex Review - Phase 2.6 Evaluation Gate

Reviewed change: `26856a0 Phase 2.6: fail-closed Phase 2 eval gate (full French juridic claim)`

## BLOCKER

1. `phase2_benchmark_artifact_errors` does not enforce the production provenance it says is required.

   Evidence: `crates/jurisearch-cli/src/main.rs:8183` only validates `jurisdiction`, `fingerprint`, non-empty `evidence`, and the boolean flags `provenance.sampled`, `provenance.human_in_gold`, and `provenance.llm_in_gold`. It never requires non-empty `provenance.pipeline`, `provenance.code_version`, or `provenance.index_revision`, even though the gate's own `required_evidence` says "structured provenance: pipeline + code_version + index_revision" and the benchmark check message says the benchmark must run "through the production pipeline".

   Impact: once the corpus and query-readiness checks pass, a hand-written artifact with good-looking aggregate numbers, `state: "passed"`, and only the three boolean provenance flags can open `phase2_gate.claim_allowed`. That is not fail-closed for the "through the production pipeline" part of the full-juridic claim.

   Concrete fix: require `provenance.pipeline` to equal an accepted production-pipeline identifier, require non-empty `provenance.code_version` and `provenance.index_revision`, and preferably compare those values to the current CLI/index manifests exposed by `status`. Add a regression test with an otherwise-valid artifact missing those fields and assert `state == "failed"`.

2. The benchmark validator does not prove the benchmark covers both required jurisprudence families or the claimed citation-verification task.

   Evidence: `crates/jurisearch-cli/src/main.rs:8203` validates only two aggregate category objects, `jurisprudence_retrieval` and `decision_citation`, and `phase2_benchmark_validate_category` at `crates/jurisearch-cli/src/main.rs:8220` checks only `value` and `queries`. There is no required field for metric kind (`recall_at_10` vs another metric), no court-family breakdown proving both Cassation/judicial and administrative retrieval queries are present, and no identifier breakdown proving ECLI/pourvoi/CETATEXT decision-citation verification.

   Impact: the gate can be satisfied by an artifact with 30 retrieval queries from only one court family, or by a `decision_citation` number that is not actually ECLI/pourvoi/CETATEXT accuracy. That can open the "full French juridic search" claim while the artifact is narrower than the policy described in the status payload and evidence note.

   Concrete fix: make the artifact contract explicit and validate it. For example, split retrieval into judicial and administrative subcategories with independent minimum query counts/floors, require `metric` or `metric_name` values such as `recall_at_10` and `decision_citation_accuracy`, and require citation identifier coverage fields for ECLI/pourvoi/CETATEXT or separate validated subcategories. Add negative tests for a judicial-only retrieval artifact and a wrong-metric citation artifact.

## WARN

1. `phase2_benchmark_payload_with_path` still depends on the artifact's self-reported `state`.

   Evidence: after `phase2_benchmark_artifact_errors` returns no errors, `crates/jurisearch-cli/src/main.rs:8142` sets `payload["state"]` to `artifact["state"].as_str().unwrap_or("pending")`. A valid artifact without `state: "passed"` remains pending or failed, even though the surrounding comments say status re-derives pass from per-category metrics and never trusts a self-reported state.

   Impact: this is not an unsafe opening by itself, but it makes the gate semantics depend on a redundant producer-controlled field and contradicts the documented "re-derived" policy.

   Concrete fix: if validation errors are empty, set `payload["state"] = "passed"` directly; if the source artifact includes a state, preserve it under a separate diagnostic field such as `artifact_state`.

2. The public schema does not describe the Phase 2 benchmark contract.

   Evidence: `crates/jurisearch-core/src/schema.rs:482` adds `Phase2GateResponse`, but `benchmark` is only `{ "type": "object" }`.

   Impact: clients can see that a `phase2_gate` exists, but cannot discover the artifact fields needed to satisfy it or the structured response shape returned by `status`.

   Concrete fix: add a `Phase2BenchmarkGate` schema analogous to `ExternalBenchmarkGate` / `FranceLegiGate`, including `state`, `source`, `artifact_path`, `artifact_error`, `jurisdiction`, `fingerprint`, `floors`, `categories`, `provenance`, `evidence`, `reason`, and the stricter provenance/category fields from the blocker fixes.

## NIT

1. The helper names and comments still say "re-derives pass" more strongly than the current implementation does.

   Evidence: `crates/jurisearch-cli/src/main.rs:8181` says `phase2_benchmark_artifact_errors` re-derives whether an artifact passes, but the actual pass decision is split between that validator and the artifact's own `state` at `crates/jurisearch-cli/src/main.rs:8143`.

   Concrete fix: either implement the fully derived state as suggested above, or rename the helper/comment to say it validates artifact eligibility while the artifact state is still required.

## Verification

- `git diff --check HEAD~1 HEAD` passed.
- `cargo test -p jurisearch-cli phase2_` passed: 5 tests.
- `cargo test -p jurisearch-cli --test cli_contract status_returns_json_without_index` passed.
- `cargo test -p jurisearch-core schema` passed.

VERDICT: FIXES_REQUIRED
