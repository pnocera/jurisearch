# BSARD Full Benchmark Result

Date: 2026-06-22

## Artifact

- Path: `work/03-implementation/02-evidence/phase1-external-benchmark-bsard.json`
- Kind: `phase1_external_expert_benchmark`
- Dataset: `maastrichtlawtech/bsard`
- Dataset revision: `f3ca6a396a47c4a3afd26b766b5abc0a56bb4205`
- Split: `test`
- Jurisdiction: Belgium
- License: `cc-by-nc-sa-4.0`
- Usage scope: `eval_only`
- Corpus documents: `22,633`
- Questions: `222`
- Runtime: `2,339.89s`
- Embedding endpoint: OpenRouter-compatible hosted endpoint, `baai/bge-m3`
- Embedding fingerprint: `bge-m3`, `1024` dimensions, normalized

Scope caveat: this artifact is produced by the external BSARD Python harness. Its BM25 and RRF implementations are standalone benchmark code, not the production Rust `pg_search` analyzer or production retrieval pipeline. The shared production-relevant component is the locked `bge-m3` embedding fingerprint.

Command run by the operator:

```bash
python3 external-benchmarks/bsard_benchmark.py \
  --base-url https://openrouter.ai/api/v1 \
  --model baai/bge-m3 \
  --api-key-env OPENROUTER_API_KEY \
  --embed-batch-size 16 \
  --out work/03-implementation/02-evidence/phase1-external-benchmark-bsard.json
```

## Metrics

Status: `failed`

| Mode | Recall@20 | Success@20 | MRR@20 | nDCG@20 | Queries |
|---|---:|---:|---:|---:|---:|
| BM25 | `0.3789` | `0.5270` | `0.2699` | `0.2597` | `222` |
| Dense | `0.4784` | `0.6712` | `0.3843` | `0.3512` | `222` |
| Hybrid RRF | `0.4683` | `0.6261` | `0.3389` | `0.3297` | `222` |

Status-enforced floors:

- Hybrid recall@20 must be at least `0.75`; observed `0.4683`.
- Hybrid nDCG@20 must be at least `0.60`; observed `0.3297`.
- Hybrid MRR@20 must be at least `0.50`; observed `0.3389`.

Dense retrieval outperformed the standalone benchmark harness RRF hybrid on this benchmark. The full run therefore does **not** open the Phase 1 external expert benchmark gate.

## Status Validation

Validated with the completed LEGI index:

```bash
JURISEARCH_INDEX_DIR=/home/pierre/Work/jurisearch/index/phase1-freemium-20250713 \
JURISEARCH_PHASE1_EXTERNAL_BENCHMARK=work/03-implementation/02-evidence/phase1-external-benchmark-bsard.json \
  cargo run -q -p jurisearch-cli -- status
```

Hand-condensed status result. The literal CLI shape is `phase1_gate.checks[]` plus `phase1_gate.external_benchmark.*`; this snippet preserves the decision fields and check statuses:

```json
{
  "phase1_state": "not_ready",
  "claim_allowed": false,
  "external_state": "failed",
  "external_check": "fail",
  "artifact_error": "metrics.hybrid.recall_at_20 must be at least threshold 0.750, got 0.468; metrics.hybrid.ndcg_at_20 must be at least threshold 0.600, got 0.330; metrics.hybrid.mrr_at_20 must be at least threshold 0.500, got 0.339",
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

Interpretation:

- The completed LEGI index is query-ready.
- Ingestion, projection coverage, embedding coverage, replay snapshot, embedding model, and reranker decision checks pass.
- The only blocking Phase 1 gate is the external expert-annotated benchmark quality floor.
- `claim_allowed=false` is correct and must remain so until a valid external benchmark artifact clears the status-enforced thresholds.

## Next Work

Do not lower thresholds to make this artifact pass. The next engineering step is a focused benchmark-quality investigation:

- Verify BSARD harness adaptation choices: article text field, question text field, qrels mapping, ID mapping, and whether title/metadata should be included.
- Compare dense-only, BM25-only, and hybrid variants before changing the gate policy, because dense currently beats the benchmark harness RRF hybrid on BSARD.
- Decide whether the eventual gate should keep using the standalone Python proxy harness or execute the production Rust retrieval pipeline against BSARD.
- Tune RRF/BM25 analyzer/query preprocessing against a development split or separate candidate set, then rerun the locked full test split only for gate evidence.
- Consider the deferred reranker path on BSARD, with latency and fallback evidence, before adopting it.
- Keep LLeQA as a secondary external candidate only after the same artifact/gate discipline is applied.
