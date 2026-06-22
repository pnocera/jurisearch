# Claude Review Prompt: BSARD Full Benchmark Result Evidence

Repo: `/home/pierre/Work/jurisearch`

Please review the current evidence/documentation update only. Do not edit files.

## User Intent

The operator ran the full BSARD benchmark without corpus/question limits, then validated `jurisearch status`. We need to record the result accurately and avoid any overclaim. The full artifact failed the external expert benchmark floors, so Phase 1 must remain not ready.

## Scope To Review

New/updated files:

- `work/03-implementation/02-evidence/phase1-external-benchmark-bsard.json`
- `work/03-implementation/02-evidence/2026-06-22-bsard-full-benchmark-result.md`
- `work/03-implementation/HANDOFF-2026-06-22-bsard-external-benchmark.md`
- `work/03-implementation/IMPLEMENTATION_PLAN.md`

Relevant prior implementation:

- `external-benchmarks/bsard_benchmark.py`
- `crates/jurisearch-cli/src/main.rs`

## Facts To Check

Artifact summary:

- `state=failed`
- dataset revision `f3ca6a396a47c4a3afd26b766b5abc0a56bb4205`
- `corpus_documents=22633`
- `questions=222`
- `elapsed_seconds=2339.89`
- BM25: recall@20 `0.37887694170588904`, success@20 `0.527027027027027`, MRR@20 `0.26989844029317717`, nDCG@20 `0.25972530871229305`
- Dense: recall@20 `0.4783631033631034`, success@20 `0.6711711711711712`, MRR@20 `0.38425111714585397`, nDCG@20 `0.35119093892460257`
- Hybrid: recall@20 `0.46834998446840553`, success@20 `0.6261261261261262`, MRR@20 `0.338855704838677`, nDCG@20 `0.32968819363118734`
- Hybrid floors: recall@20 `0.75`, nDCG@20 `0.60`, MRR@20 `0.50`

Status validation with the completed index:

```bash
JURISEARCH_INDEX_DIR=/home/pierre/Work/jurisearch/index/phase1-freemium-20250713 \
JURISEARCH_PHASE1_EXTERNAL_BENCHMARK=work/03-implementation/02-evidence/phase1-external-benchmark-bsard.json \
  cargo run -q -p jurisearch-cli -- status
```

Condensed result:

```json
{
  "phase1_state": "not_ready",
  "claim_allowed": false,
  "external_state": "failed",
  "external_check": "fail",
  "checks": {
    "index_query_ready": "pass",
    "latest_completed_ingest_run": "pass",
    "failed_members": "pass",
    "projection_coverage": "pass",
    "embedding_coverage": "pass",
    "replay_snapshot": "pass",
    "external_expert_annotated_eval": "fail",
    "final_embedding_model": "pass",
    "reranker_decision": "pass"
  }
}
```

## Review Requirements

Findings first, ordered by severity. Please verify:

- The new evidence note accurately reflects the artifact and status result.
- The implementation plan and handoff do not claim Phase 1 readiness.
- The next-work recommendation is technically sound and does not suggest tuning on the locked full test split.
- There is no threshold relaxation or accidental conversion of the failed artifact into a passing gate.
- Any missing risk or wording issue that would confuse the next Codex session is identified.

Then include:

- Open questions/risks.
- Verification notes.
- Final verdict as exactly one of:
  - `VERDICT: GO`
  - `VERDICT: FIXES_REQUIRED`
