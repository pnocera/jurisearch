I have everything needed. The diff is small and self-contained; I reviewed the code, schema, tests, the three evidence artifacts, the status JSON, and confirmed no reranker implementation exists anywhere in `crates/`.

---

## Phase 1 Reranker Deferral Gate — Review

### 1. Findings (by severity)

**[Low / forward-looking] The `reranker_decision` payload is fully static and unverified against runtime — `main.rs:3792-3807`**
`phase1_reranker_decision_payload()` hardcodes `provider:"disabled"`, `adopted:false`, `state:"deferred"`, and the gate check at `main.rs:3772-3776` is a constant `"pass"`. I confirmed via grep that no reranker provider/config exists in `crates/` today, so this is *accurate for Phase 1 as shipped*. The risk is stale-by-construction: when the provider seam from the feasibility spike (`disabled|http|local_onnx`) eventually lands, this payload will keep reporting `disabled`/`adopted:false` and the gate will keep passing regardless of actual runtime config. Recommend a guard/TODO so that whoever wires the provider in must derive this payload from real config rather than the literal. Not a blocker now.

**[Low] The gate passes without verifying the recorded evidence exists — `main.rs:3772-3776`, `3799-3803`**
The check's contract is "a deferral/adoption decision is recorded," but it passes unconditionally and never checks that the three referenced evidence files are present. All three exist today (verified), but if any is later renamed/deleted, the gate still passes and `status` emits dangling evidence paths. For a release-gating artifact, consider at least an existence assertion, or document that these paths are advisory.

**[Low] Schema entry is looser than codebase convention — `schema.rs:413`, `cli_contract.rs:171-176`**
`reranker_decision` is declared `{"type": "object"}`, while siblings use named sub-schemas (`EvalFixtureSummary`, `Phase1GateCheck` via `$ref`). The CLI actually emits eight structured fields (`state`, `provider`, `adopted`, `decision_date`, `model_candidate`, `evidence`, `reason`, `future_adoption_gate`) and the unit test asserts three of them, but the compiled schema documents none and the contract test only checks `type == "object"`. A `RerankerDecision` sub-schema would restore parity and let the contract test catch field drift.

**[Nit] Model-name inconsistency** — payload `model_candidate` is `"BAAI/bge-reranker-v2-m3"` (`main.rs:3798`) while the deferral-decision artifact prose writes `bge-reranker-v2-m3`. Same model; harmless.

**[Observation, not a defect] Evidence framing is slightly stronger than the data supports**
BM25 and hybrid both pass 4/4 at top-20 (dense 2/4) — the release-candidate fixtures are *saturated*, so they cannot distinguish any ranking refinement, meaning they prove neither a need for reranking nor its absence. The artifact's bullet "fixtures do not show a quality need for reranking" (`...deferral-decision.md:19`) reads as the latter; the gate message's framing — "until legal eval proves a material rerank gain" — is the correct, conservative one. The conclusion (defer) is the safe, status-quo direction and matches the constraint "do not adopt without measured legal-quality gain," so the decision is justified even though one rationale line slightly overstates.

### 2. Open questions / residual risks
- When the reranker provider seam is implemented, who owns updating this now-static payload + gate so it reflects real config? (see Finding 1)
- The dev fixture `legi-hierarchy-temporal-sibling-2000` still fails under hybrid top-20 — unrelated to this diff, but the deferral leans on "hybrid is good enough," and hierarchy-sensitive recall is exactly where a reranker would plausibly help. Tracked as a W2/W5 follow-up in the benchmark summary; worth keeping visible so deferral isn't read as "reranking proven unnecessary."

### 3. Recommendations
- Add a `RerankerDecision` named schema and assert its key fields in the contract test (parity + drift protection).
- When the provider lands, replace the literal `"pass"`/`provider:"disabled"` with config-derived values.
- Optional: soften the artifact's "no quality need" wording to "current fixtures cannot measure a rerank gain (BM25≈hybrid, saturated)," matching the gate message.

### 4. Verification notes
- Reviewed the uncommitted diff only; **no files edited**.
- **Fail-closed preserved.** `claim_allowed = checks.all(status=="pass")` (`main.rs:3778-3780`). `release_gating_eval_fixtures` stays `pending` because `eval_summary.release_gating == 0` (requires named human review); confirmed `release_gating: 0` in the captured status JSON. So `claim_allowed:false` / `state:"not_ready"`. The reranker check flipping `pending→pass` removes a non-human blocker but does **not** open the claim — exactly the intended behavior and consistent with the constraint that named legal-domain review remains human-blocked.
- **Coherence confirmed** across code → schema → both tests → status JSON → plan. `eval_fixtures` counts (BM25/hybrid 4/4, dense 2/4) match `2026-06-22-phase1-eval-benchmark-summary.md`; decision date `2026-06-22` consistent; the plan correctly moved the reranker item out of "Remaining" and into a "Done" line.
- Confirmed via `grep` that no reranker implementation exists in `crates/`, so `provider:"disabled"` is truthful today.
- Did not re-run `cargo`; trusting the stated fmt/test/clippy green run. The modified unit test (`main.rs:4792-4827`) and contract assertion (`cli_contract.rs:171-176`) are internally consistent with the emitted payload I read. Captured time evidence (2.95s) matches the request.

VERDICT: GO
