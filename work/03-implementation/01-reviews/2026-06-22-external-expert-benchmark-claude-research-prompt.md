We are in /home/pierre/Work/jurisearch.

Task: independently research whether existing public or gated datasets can replace our unavailable human legal-domain review step for Phase 1 retrieval gating, and recommend how the code/docs should model that gate.

Context:
- Product: French legal search over LEGI statutory text.
- Current internal eval fixtures are source-checked LEGI release candidates, but not human-reviewed. They are useful smoke/regression cases, not sufficient to prove a best-in-class Phase 1 claim.
- We do not have humans available for review.
- Candidate datasets already identified by Codex:
  - https://huggingface.co/datasets/maastrichtlawtech/bsard
  - https://huggingface.co/datasets/maastrichtlawtech/lleqa
  - https://huggingface.co/datasets/mteb-private/FrenchLegal1Retrieval-sample
  - https://huggingface.co/datasets/louisbrulenaudet/tax-retrieval-benchmark
  - non-gating corpora: https://huggingface.co/datasets/AgentPublic/legi and similar LEGI corpora without qrels
- We need an implementation direction for `jurisearch status.phase1_gate`.

Please do the following:
1. Research available French/French-language legal retrieval or QA datasets on Hugging Face and related primary sources.
2. Decide which datasets are credible substitutes for local human review, and which are only supplemental.
3. Be precise about limitations: Belgian vs French law, statutory vs jurisprudence, gated access, non-commercial licenses, sample-only datasets, lack of qrels.
4. Recommend exact gate semantics:
   - whether to replace `release_gating_eval_fixtures` with `external_expert_annotated_eval`;
   - whether internal LEGI fixtures should remain smoke/dev/release-candidate only;
   - what evidence should be recorded before the Phase 1 claim can open.
5. Review the likely implementation approach:
   - add structured `phase1_gate.external_benchmark` or equivalent status payload;
   - keep fail-closed until a real external benchmark run is recorded;
   - update schema/tests/docs;
   - do not mark pass based on documentation alone.

Relevant local files to inspect:
- crates/jurisearch-cli/src/main.rs
- crates/jurisearch-core/src/eval.rs
- crates/jurisearch-core/src/schema.rs
- crates/jurisearch-cli/tests/cli_contract.rs
- work/03-implementation/IMPLEMENTATION_PLAN.md
- work/03-implementation/02-evidence/2026-06-22-phase1-fixture-strength-decision.md
- work/03-implementation/02-evidence/2026-06-22-phase1-eval-benchmark-summary.md

Do not edit files. Save your answer as a concise research/review artifact with:
- Findings first, ordered by importance.
- Dataset recommendation table.
- Proposed gate semantics.
- Implementation risks.
- Final line exactly one of:
  - VERDICT: RESEARCH_COMPLETE
  - VERDICT: BLOCKED
