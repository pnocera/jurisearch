# Phase 1 Fixture Strength Decision

Date: 2026-06-22

Decision:

- The current Phase 1 release-candidate fixture set is not strong enough to prove a hybrid-over-BM25 quality advantage.
- The fixtures remain useful source-checked smoke coverage for known-article, conceptual statutory, temporal, and citation-rich statutory retrieval.
- They should not be promoted to release-gating status until named human legal-domain review adds or approves fixtures that can discriminate ranking quality.

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

Promotion requirements:

- Named human legal-domain review must sign off any release-gating fixtures.
- The release-gating set should include discriminating cases where BM25, dense, hybrid, and any future hybrid+rerank can differ meaningfully.
- The existing failing dev fixture `legi-hierarchy-temporal-sibling-2000` should be considered as a seed for discriminating temporal/hierarchy coverage after legal-domain review.
- Hierarchy-sensitive expectations should be executed, not only stored as fixture metadata.
- Promotion should use either a tighter automated gate or a documented manual checklist so saturated fixtures cannot be promoted by review count alone.
- The Phase 1 claim should remain blocked until those release-gating fixtures exist and pass.
