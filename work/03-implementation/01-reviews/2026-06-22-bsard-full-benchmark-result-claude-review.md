Live run confirms the documented result **exactly** — `state=not_ready`, `claim_allowed=false`, `external_benchmark.state=failed`, the `artifact_error` string matches, and all nine checks carry precisely the documented statuses (only `external_expert_annotated_eval=fail`). The review is complete.

---

# Review: BSARD Full Benchmark Result Evidence

## Findings (by severity)

### 1. [Medium — clarity] Evidence does not flag the proxy-vs-production retrieval gap
The BSARD harness (`external-benchmarks/bsard_benchmark.py`) implements its **own** self-contained BM25 (`k1=1.5, b=0.75`, generic `tokenize`, lines 111–146) and its **own** numpy RRF (`hybrid_rankings`, line 353). It does **not** exercise jurisearch's production retrieval: the Rust `pg_search` BM25, the Phase 1.3 French legal analyzer (elision/accents/legal stopwords), or the production custom RRF. The only element shared with production is the bge-m3 embedding (via OpenRouter), which matches the locked fingerprint.

Consequence: the statement "Dense retrieval outperformed the current RRF hybrid" and the next-work item "tune RRF/BM25 analyzer/query preprocessing" both refer to the **harness**, not the production stack. A next Codex session could read hybrid `0.4683` as a measurement of jurisearch's real RRF and tune the wrong layer — or assume harness gains transfer to production (or vice-versa). This is conservative for the gate (it does not inflate readiness), so it is a clarity issue, not an overclaim. Recommend one explicit sentence in the evidence note and handoff stating the harness uses a standalone BM25/RRF + shared bge-m3, so numbers bound the embedding/proxy pipeline, not the Rust retrieval pipeline.

### 2. [Low — repro] Condensed status JSON does not match the literal CLI shape
The evidence note (`2026-06-22-bsard-full-benchmark-result.md`, lines 62–81) presents status as flat keys: `phase1_state`, `external_state`, `external_check`, top-level `artifact_error`, and `checks` as a `{name: status}` map. The real output (verified live) is:
- `phase1_gate.state` / `phase1_gate.claim_allowed` (no top-level `phase1_state`),
- `phase1_gate.checks` is an **array** of `{name, status, message}` (not a map),
- `external_state`/`external_check` do not exist as keys — they map to `phase1_gate.external_benchmark.state` and the `external_expert_annotated_eval` check's `status`,
- `artifact_error` is nested under `phase1_gate.external_benchmark.artifact_error`.

The note labels it "Condensed result," and the **handoff's `phase1_gate.*` dotted notation is accurate**, so substance is fine. But a next session grepping live output for `external_state`/`phase1_state` finds nothing. Recommend a one-line "(hand-condensed; real shape is `phase1_gate.checks[]` + `phase1_gate.external_benchmark.*`)" caveat.

### 3. [None — confirmations] Everything else verified clean
- **Numbers**: every metric in the evidence note and handoff matches the artifact JSON to the rounding shown (BM25/Dense/Hybrid recall/success/MRR/nDCG, 22,633 docs, 222 questions, 2,339.89s, revision `f3ca…`).
- **No threshold relaxation**: artifact thresholds (`0.75/0.60/0.50`) equal the code policy floors `PHASE1_EXTERNAL_MIN_HYBRID_{RECALL,NDCG,MRR}_AT_20` (main.rs:90–92). The gate requires both `threshold >= policy_floor` **and** `metric >= threshold` (main.rs:4028–4054), so floors cannot be lowered via the artifact.
- **No failed→pass conversion**: `state=failed` is preserved; the gate re-derives failure from metrics regardless of artifact `state` (main.rs:3843–3849, 4045–4050). `artifact_error` string reproduced exactly by the live run.
- **No Phase 1 readiness claim**: plan diff and handoff both keep "not ready", `claim_allowed=false`, and explicitly say "Do not lower thresholds."
- **Next-work soundness / no test-set tuning**: the recommendation tunes on "a development split or separate candidate set, then rerun the locked full test split only for gate evidence." BSARD ships a train split, so this is legitimate and avoids leakage on the locked `test` split.
- **Metric semantics**: rankings are truncated to `k` before scoring (line 378); recall@20 = `|relevant ∩ top20| / |relevant|` (true recall), nDCG normalized by `ideal_dcg(min(|relevant|, k))`, MRR/success computed within top-k. The dense-beats-hybrid comparison is therefore meaningful.

## Open questions / risks
- **Harness BM25 ≠ production analyzer.** BSARD BM25 (`0.3789`) likely understates what jurisearch's French legal analyzer would yield, and improving the harness analyzer is not the same as improving production. Decide explicitly whether the eventual gate should run the **real** Rust pipeline against BSARD or accept the embedding-proxy harness as the gate of record. (Most important downstream risk.)
- **Belgian-law applicability** is well-documented in the artifact (`claim_scope`/`applicability`) and handoff — keep that caveat attached to any future passing artifact.
- Housekeeping (out of review scope): `…-bsard-full-benchmark-result-claude-review.md.tmp` is left in the working tree per `git status`.

## Verification notes
- Read all four scope files + the gate logic (`phase1_external_benchmark_artifact_errors`, `phase1_validate_external_benchmark_metric`, `phase1_gate_payload`) and the harness metric/ranking functions.
- Cross-checked every documented metric against `phase1-external-benchmark-bsard.json`.
- **Ran the documented status command** against the live index `index/phase1-freemium-20250713` with the prebuilt `target/debug/jurisearch`: output reproduced `state=not_ready`, `claim_allowed=false`, `external_benchmark.state=failed`, the exact `artifact_error`, and all nine check statuses as documented.
- Confirmed policy-floor constants equal artifact thresholds; confirmed rankings truncated to `k` before metric computation.

## VERDICT: GO

The evidence is accurate, fails closed, relaxes nothing, and makes no Phase 1 readiness claim — all verified against the artifact and a live status run. Findings 1 and 2 are recommended clarifications (proxy-vs-production framing; condensed-JSON caveat), not correctness blockers.
