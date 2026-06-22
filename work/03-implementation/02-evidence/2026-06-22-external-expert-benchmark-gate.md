# External Expert Benchmark Gate

Date: 2026-06-22

Decision:

- Replace the unavailable local named-human review blocker with an external expert-annotated French legal retrieval benchmark gate for the Phase 1 LEGI/statutory claim.
- Keep the internal LEGI fixtures as source-checked smoke/regression and release-candidate evidence, not as the release gate.
- Keep `claim_allowed=false` until the selected external benchmark is actually imported/run and metrics are recorded.
- Scope the eventual pass claim to an external expert-annotated French-language statutory retrieval benchmark. Do not present it as France-LEGI human-reviewed gold.

Primary candidate:

- `maastrichtlawtech/bsard`
- Why: French-native statutory article retrieval, Hugging Face task `Text Retrieval`, legal tag, 22.6k-row corpus view, CC BY-NC-SA 4.0 license, and primary-source paper/repo describing 1,100+ French legal questions labeled by experienced jurists against 22,600+ Belgian statutory articles.
- Limitation: Belgian statutory law, not French LEGI. This is still the best available proxy for expert-annotated French statutory retrieval when local reviewers are unavailable.
- Usage scope: eval only, with dataset revision/license recorded before any pass decision.

Secondary candidate:

- `maastrichtlawtech/lleqa`
- Why: French-native legal QA/retrieval dataset built on BSARD, with additional legal questions, statutory articles, paragraph-level references, and comprehensive answers written by legal professionals.
- Limitation: gated research access, CC BY-NC-SA 4.0, Belgian statutory law.

Supplemental candidates:

- `mteb-private/FrenchLegal1Retrieval-sample`: useful signal for MTEB-style French legal retrieval, but the public sample/full access status is insufficient for the sole release gate.
- `louisbrulenaudet/tax-retrieval-benchmark`: useful if access is granted, but tax-only scope is narrower than Phase 1 statutory retrieval.

Non-gating inputs:

- Internal LEGI release candidates: source-checked against DILA LEGI and useful for smoke/regression coverage, but not independently expert-annotated.
- `AgentPublic/legi` and similar LEGI corpora: useful corpus context, but no expert retrieval qrels/review labels.

Gate semantics:

- `jurisearch status.phase1_gate.external_benchmark.state` starts as `pending`.
- `phase1_gate.checks[]` uses `external_expert_annotated_eval` as the quality blocker.
- The check must not pass from documentation alone.
- Required evidence before pass:
  - dataset access and license recorded;
  - corpus/questions/qrels imported or adapted with no training leakage;
  - runner may be external to `jurisearch` and written in Python if that keeps dataset loading, scoring, and iteration simpler;
  - durable metrics artifact path recorded for `jurisearch status` to consume before this gate can pass;
  - BM25, dense, and hybrid retrieval metrics recorded with top-k, recall, and nDCG;
  - dataset revision, jurisdiction, usage scope, and non-commercial license implications recorded;
  - Belgian-law to French-LEGI applicability argued explicitly in the adoption decision;
  - the Phase 1 adoption threshold documented before `claim_allowed` can become true.

Sources:

- https://huggingface.co/datasets/maastrichtlawtech/bsard
- https://github.com/maastrichtlawtech/bsard
- https://huggingface.co/datasets/maastrichtlawtech/lleqa
- https://huggingface.co/papers/2309.17050
- https://huggingface.co/datasets/mteb-private/FrenchLegal1Retrieval-sample
- https://huggingface.co/datasets/louisbrulenaudet/tax-retrieval-benchmark
