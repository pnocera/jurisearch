I have everything I need. Here is the review.

## Findings (ordered by severity)

### Gate safety — PASS (no blocker)
The core requirement is met and well-tested. `claim_allowed` is computed as "all checks `pass`" (`main.rs:3780-3782`), and the new `external_expert_annotated_eval` check (`main.rs:3752-3760`) is `pass` only when `external_benchmark["state"] == "passed"` (`main.rs:3692`). Since `phase1_external_benchmark_payload()` hard-codes `"state": "pending"` (`main.rs:3797`), the check is permanently `pending` and **`claim_allowed` cannot open without a code change** — there is no runtime/file input that flips it. The fail-closed property is directly proven by the test at `main.rs:4858-4910`: every other input is set to its passing value, yet `claim_allowed == false` because the benchmark is pending. This is exactly the desired behavior. Field parity is clean — all 8 payload keys match the `ExternalBenchmarkGate` schema (`schema.rs:424-435`).

### Medium

1. **Primary gate is a Belgian-law dataset standing in for a French-LEGI claim** (`main.rs:3801`, evidence doc lines 13-15). BSARD is French-language but Belgian statutory law. It is honestly and repeatedly flagged as a limitation, so it passes the "honest enough for this slice" bar (nothing is promoted — state is `pending`). But the validity transfer gap (Belgian → French LEGI) is substantive: passing BSARD does **not** by itself prove French LEGI retrieval quality. Recommend the threshold/adoption doc make "argue French-LEGI applicability of a Belgian-law result" an explicit promotion precondition, not just a noted caveat, before anyone flips the state to `passed`.

2. **A `failed` benchmark state would be reported as `pending`, not `fail`** (`main.rs:3754`). The check maps only `passed → "pass"`, everything else → `"pending"`, yet the schema enum admits `"failed"` (`schema.rs:426`). This is safe for `claim_allowed` (still closed), but dishonest in reporting: an actually-run-and-failed benchmark would surface as "not yet run." Currently dead (hard-coded pending), but should be fixed to `failed → "fail"` when real metrics are wired, so a real failure isn't masked.

3. **No positive-path / open-path regression test.** Only the `false` branch of the benchmark wiring is exercised. There is no test that a `passed` state actually flips `external_expert_annotated_eval` to `pass` (and, combined with the other inputs, opens `claim_allowed`), nor an invariant that `state == "passed"` requires non-empty `evidence`. A small refactor making the payload state injectable would let you guard the open path against future regressions and prevent a "passed-with-empty-evidence" flip. Not required for this slice (hard-coded pending), but the open path is the higher-risk one and is currently unprotected.

### Low / nits

4. **Dataset-ID inconsistency:** payload uses `mteb-private/FrenchLegal1Retrieval` (`main.rs:3826`) while the evidence doc and its source link use `mteb-private/FrenchLegal1Retrieval-sample` (gate doc lines 25, 52). Reconcile to the canonical HF id for traceability.

5. **License risk surfaced but unresolved:** BSARD/LLeQA are CC-BY-NC-SA-4.0 (non-commercial) (`main.rs:3804,3812`). The required-evidence list does include "dataset access and license recorded," which is good — but if jurisearch is commercial, confirm NC terms permit the intended benchmark/eval use before promotion.

6. **Stale wording (out of scope of this diff):** the reranker's `future_adoption_gate` still measures adoption against "release-gating fixtures" (`main.rs:3871`), which is now slightly inconsistent with the new external-benchmark-as-gate philosophy. Worth aligning in a later pass.

7. **Untyped schema items:** `candidate_datasets` / `non_gating_inputs` use `{"type": "object"}` with no sub-properties (`schema.rs:430-431`). Acceptable for this slice; consider typed sub-schemas later to strengthen the contract.

### "Named human review" check — PASS
The live blocker no longer implies named human review. The check description now reads "Phase 1 requires a passing external expert-annotated French legal retrieval benchmark; internal LEGI fixtures remain smoke/candidate evidence" (`main.rs:3759`). The plan and the fixture-strength decision now scope named-human review to the *hypothetical future case of project-owned release-gating labels*, with the external benchmark as the actual Phase 1 gate (IMPLEMENTATION_PLAN.md "Current status" and Tasks/Acceptance edits; fixture-strength-decision.md "Follow-up requirements"). `grep` confirms no remaining `.rs` references to the old `release_gating_eval_fixtures` check name — only historical review/evidence files retain it, which is correct.

## Open questions / risks
- **Belgian→French transfer:** what numeric threshold on BSARD constitutes "Phase 1 passes," and how will French-LEGI applicability be justified at promotion? (Tied to Medium #1.)
- **Flip mechanism:** the gate doc (line 42) promises "a durable metrics artifact path for `jurisearch status` to consume," but the current code is purely static — there is no consumption path yet. Confirm the eventual flip is data-driven (read + verify metrics) rather than a hand-edited boolean, otherwise the "must not pass from documentation alone" guarantee (gate doc line 37) depends only on reviewer discipline.
- **NC license + commercial use:** see Low #5.

## Verification notes
- `cargo test -p jurisearch-cli phase1_gate_payload_maps_ready_inputs_and_failed_members` → 1 passed.
- `cargo test -p jurisearch-cli --test cli_contract` → 44 passed, 2 ignored (includes `help_schema_json_is_valid_and_lists_commands` and `status_returns_json_without_index`).
- Manually confirmed fail-closed: `main.rs:4910` asserts `claim_allowed == false` with all non-benchmark inputs maximally ready.
- Schema/payload field parity verified: 8/8 keys match `ExternalBenchmarkGate`; dataset/non-gating item shapes consistent across entries.
- No `.rs` references to the retired check name remain.

All findings are non-blocking quality/honesty improvements for the next slice (real benchmark wiring), not defects in this slice's fail-closed safety. The diff correctly and verifiably achieves the stated intent.

VERDICT: GO
