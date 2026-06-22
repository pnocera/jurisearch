# Claude Review Request: Phase 1 Reranker Deferral Gate

Repo: `/home/pierre/Work/jurisearch`

Review scope:

- Current uncommitted diff only.
- Do not edit files.
- Focus on Phase 1 gate safety, whether the reranker deferral decision is justified by the evidence, and whether status/schema/tests remain coherent.

User intent and constraints:

- Continue the implementation plan without blocking on human-only work.
- The plan requires reranker adoption or deferral to be recorded before a Phase 1 claim.
- Named legal-domain review of release-gating fixtures remains human-blocked and must keep `claim_allowed=false`.
- Do not adopt a reranker without measured legal-quality gain and operational evidence.

Changed files/artifacts:

- `crates/jurisearch-cli/src/main.rs`
- `crates/jurisearch-cli/tests/cli_contract.rs`
- `crates/jurisearch-core/src/schema.rs`
- `work/03-implementation/IMPLEMENTATION_PLAN.md`
- `work/03-implementation/02-evidence/2026-06-22-reranker-deferral-decision.md`
- `work/03-implementation/02-evidence/2026-06-22-phase1-status-after-reranker-decision.json`
- `work/03-implementation/02-evidence/2026-06-22-phase1-status-after-reranker-decision.time.json`

Implementation summary:

- Added a structured `phase1_gate.reranker_decision` payload:
  - `state = "deferred"`
  - `provider = "disabled"`
  - `adopted = false`
  - evidence paths for the feasibility spike, real-index eval summary, and deferral decision artifact.
- Changed the `reranker_decision` Phase 1 gate check from `pending` to `pass` because a non-adoption decision is now recorded.
- The overall Phase 1 gate remains fail-closed because `release_gating_eval_fixtures` still requires named human legal-domain review and remains `pending`.
- Added `Phase1GateResponse.reranker_decision` to the compiled schema and asserted it in CLI contract tests.
- Added an evidence artifact explaining why Phase 1 keeps reranking disabled by default:
  - Current release-candidate fixture set does not prove a reranking need: BM25 and hybrid both pass 4/4 at top 20, dense passes 2/4.
  - Fixtures are still candidates, not release-gating fixtures.
  - No reranker provider is packaged.
  - Cross-encoder latency, tokenizer/pair-contract, runtime packaging, and model-cache checks remain unresolved.

Validation already run:

- `cargo fmt --all`
- `cargo test -p jurisearch-cli phase1_gate_payload_maps_ready_inputs_and_failed_members`
- `cargo test -p jurisearch-cli`
- `cargo clippy --workspace --all-targets -- -D warnings`
- Captured full-index cached status evidence:
  - `work/03-implementation/02-evidence/2026-06-22-phase1-status-after-reranker-decision.json`
  - elapsed 2.95s
  - `reranker_decision` check is `pass`
  - `release_gating_eval_fixtures` remains `pending`
  - `claim_allowed` remains `false`

Required output structure:

1. Findings, ordered by severity, with file/line references.
2. Open questions or residual risks.
3. Recommendations or compatible suggestions.
4. Verification notes.
5. Final line exactly one of:
   - `VERDICT: GO`
   - `VERDICT: FIXES_REQUIRED`
