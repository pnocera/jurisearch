Done. Research artifact saved to `work/03-implementation/01-reviews/2026-06-22-external-expert-benchmark-claude-research.md`.

## Summary

**Core finding:** No public dataset is a full substitute for France-LEGI human review. The only datasets with genuine **human-expert** relevance labels — BSARD and LLeQA — are **Belgian law** and **non-commercial** (CC-BY-NC-SA; LLeQA is also gated). Every **France-jurisdiction** qrels dataset is either **LLM-synthetic** (`louisbrulenaudet/tax-retrieval-benchmark`, tax-only, beta) or a **closed/6-row sample** (`mteb-private/FrenchLegal1Retrieval`). `AgentPublic/legi` and COLD are corpus-only (no qrels).

So an external benchmark can become a *real, evidence-backed quality gate*, but it cannot honestly mean "France-LEGI gold, human-reviewed" — different claims, must stay distinct.

**Recommended gate semantics (do not 1:1 rename):**
- Keep internal LEGI fixtures **smoke/dev/release-candidate only**, never promotable by review-count; keep them executing as a France-source regression floor.
- Keep `release_gating_eval_fixtures` as a **non-blocking informational** check (it honestly stays pending — no human reviewers).
- Add a new **fail-closed** `external_expert_annotated_eval` / `phase1_gate.external_benchmark` check, primary dataset **BSARD**, that passes only when a recorded run JSON meets a pre-declared threshold vs a recorded baseline — with `license`, `usage_scope=eval_only`, `jurisdiction=belgium`, and a machine-readable `claim_scope`/`limitations`.
- `claim_allowed` requires that check to pass; the opened claim is *scoped* ("competitive on an external expert-annotated French-language statutory benchmark"), not "France-LEGI gold."

**Top risks flagged:** passing on documentation alone (the existing hardcoded `reranker_decision => "pass"` at `main.rs:3772` is the anti-pattern to avoid); overclaiming Belgian results as France-LEGI; CC-BY-NC-SA compliance (score-only, no redistribution/training); pinning the HF dataset revision for reproducibility.

**Verdict in the artifact: `RESEARCH_COMPLETE`.**

One housekeeping note: there's a leftover empty placeholder `…-claude-research.md.tmp` (1 line) next to the prompt file — I left it untouched since I didn't create it, but you may want to delete it.
