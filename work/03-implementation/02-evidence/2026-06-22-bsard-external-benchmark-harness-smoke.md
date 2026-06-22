# BSARD External Benchmark Harness Smoke

Date: 2026-06-22

Scope:

- Verify `external-benchmarks/bsard_benchmark.py` can load BSARD, call the configured OpenAI-compatible bge-m3 endpoint, compute BM25/dense/RRF-hybrid metrics, and emit the JSON artifact shape consumed by `jurisearch status`.
- This was a limited smoke, not a gate artifact.

Command:

```bash
python3 external-benchmarks/bsard_benchmark.py \
  --base-url https://openrouter.ai/api/v1 \
  --model baai/bge-m3 \
  --api-key-env OPENROUTER_API_KEY \
  --limit-corpus 1000 \
  --limit-questions 2 \
  --embed-batch-size 16 \
  --min-hybrid-recall-at-k 0 \
  --min-hybrid-ndcg-at-k 0 \
  --min-hybrid-mrr-at-k 0 \
  --out /tmp/jurisearch-bsard-smoke-r2.json
```

Result:

- Loaded BSARD corpus subset: 1,000 documents.
- Loaded BSARD human-authored `test` questions subset: 2 requested, 1 retained after filtering qrels to the limited corpus.
- Resolved BSARD revision: `f3ca6a396a47c4a3afd26b766b5abc0a56bb4205`.
- Endpoint: OpenRouter `baai/bge-m3`.
- Embedding cache written: `/home/pierre/.cache/jurisearch/benchmarks/bsard-baai-bge-m3-94e4fd8fb731aebe.npz`.
- Artifact written: `/tmp/jurisearch-bsard-smoke-r2.json`.
- Elapsed: 152.010 seconds.

Smoke metrics at top 20:

| Mode | Recall@20 | MRR@20 | nDCG@20 |
|---|---:|---:|---:|
| BM25 | 1.000 | 0.500 | 0.537 |
| Dense | 1.000 | 0.333 | 0.571 |
| Hybrid | 1.000 | 1.000 | 0.832 |

Gate-safety check:

```json
{
  "external_state": "failed",
  "external_check": "fail",
  "artifact_error": "dataset.limit_corpus must be null for a gate artifact; dataset.limit_questions must be null for a gate artifact; dataset.corpus_documents must be at least 22000; dataset.questions must be at least 200; thresholds.hybrid_recall_at_20_min must be at least 0.750, got 0.000; thresholds.hybrid_ndcg_at_20_min must be at least 0.600, got 0.000; thresholds.hybrid_mrr_at_20_min must be at least 0.500, got 0.000",
  "claim_allowed": false
}
```

Interpretation:

- The Python harness works end to end against the hosted bge-m3 endpoint.
- `jurisearch status` correctly rejects limited smoke artifacts and zero-threshold artifacts, even when the smoke artifact's internal state is `passed`.
- The remaining gate task is a full BSARD run with no corpus/question limits and non-zero predeclared thresholds.
