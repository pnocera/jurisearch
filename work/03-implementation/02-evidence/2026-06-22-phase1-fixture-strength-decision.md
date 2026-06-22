# Phase 1 Fixture Strength Decision

Date: 2026-06-22

Decision:

- The current Phase 1 release-candidate fixture set is not strong enough to prove a hybrid-over-BM25 quality advantage.
- The fixtures remain useful source-checked smoke coverage for known-article, conceptual statutory, temporal, and citation-rich statutory retrieval.
- They should not be promoted to project-authored release-gating status while named human legal-domain review is unavailable.
- Phase 1 should instead use an external expert-annotated French legal retrieval benchmark gate, with this fixture set retained as internal smoke/regression coverage.

Evidence:

- Real-index eval summary: `work/03-implementation/02-evidence/2026-06-22-phase1-eval-benchmark-summary.md`
- BM25 top-20: 4/4 release candidates passed.
- Hybrid top-20: 4/4 release candidates passed.
- Dense top-20: 2/4 release candidates passed.
- Hybrid plus dev fixtures: 5/6 passed; the failing dev-only fixture is `legi-hierarchy-temporal-sibling-2000`.

Interpretation:

- BM25 and hybrid are saturated on the current four release candidates, so the set cannot demonstrate a hybrid ranking advantage.
- Dense underperforms BM25/hybrid on this small set, which supports keeping hybrid as the default over dense-only retrieval.
- The set does not measure harder ranking distinctions such as close temporal siblings, near-duplicate statutory versions, or conceptually adjacent provisions where lexical matches compete with legal-context matches.

Follow-up requirements:

- External expert-annotated benchmark evidence must be recorded before the Phase 1 claim opens.
- If project-owned release-gating fixtures are added later, named human legal-domain review must sign them off.
- Any future project-owned release-gating set should include discriminating cases where BM25, dense, hybrid, and any future hybrid+rerank can differ meaningfully.
- The existing failing dev fixture `legi-hierarchy-temporal-sibling-2000` should be considered as a seed for discriminating temporal/hierarchy coverage if project-owned legal-domain review becomes available.
- Hierarchy-sensitive expectations should be executed, not only stored as fixture metadata.
- Promotion should use either a tighter automated gate or a documented manual checklist so saturated fixtures cannot be promoted by review count alone.
- The Phase 1 claim should remain blocked until the external benchmark gate passes.

Superseding gate decision:

- `work/03-implementation/02-evidence/2026-06-22-external-expert-benchmark-gate.md`
