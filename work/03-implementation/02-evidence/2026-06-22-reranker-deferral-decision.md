# Phase 1 Reranker Deferral Decision

Date: 2026-06-22

Decision:

- Phase 1 does not adopt a reranker by default.
- The Phase 1 reranker provider state is `disabled` / `deferred`.
- Retrieval keeps the current hybrid fusion order as the final order.
- HTTP or local reranking remains a future gated enhancement, not a Phase 1 release dependency.

Evidence:

- Feasibility spike: `work/03-implementation/02-evidence/2026-06-21-reranker-feasibility.md`
- Real-index Phase 1 eval summary: `work/03-implementation/02-evidence/2026-06-22-phase1-eval-benchmark-summary.md`

Rationale:

- The current release-candidate fixture set cannot measure a material rerank gain: BM25 and hybrid both pass 4/4 at top 20, while dense alone passes 2/4.
- The current release-candidate fixtures are still candidates, not release-gating fixtures, because named legal-domain review is pending.
- No reranker provider is packaged in `jurisearch` yet.
- The feasibility spike identifies `bge-reranker-v2-m3` as the first candidate and TEI/HTTP as the first practical provider, but it also records unresolved latency, tokenizer/pair-contract, runtime packaging, and model-cache checks.
- A cross-encoder reranker would add operational complexity and likely non-trivial latency; adopting it without measured legal-quality gain would weaken the Phase 1 claim rather than strengthen it.

Gate implication:

- The `reranker_decision` Phase 1 gate check may pass because the non-adoption decision is recorded.
- This does not open the Phase 1 claim by itself. Release-gating fixture promotion still requires named human legal-domain review.

Future adoption criteria:

- Hybrid+rerank must show a material legal-retrieval quality gain on release-gating fixtures.
- Reranker latency and failure behavior must be measured for bounded candidate sets.
- Reranker failures must degrade to the existing hybrid order.
- Provider/model/config must be visible in status and reproducibility manifests before adoption.
