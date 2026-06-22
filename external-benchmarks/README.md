# External Benchmarks

External benchmark runners live outside the Rust CLI but emit JSON artifacts that
`jurisearch status` can consume through `JURISEARCH_PHASE1_EXTERNAL_BENCHMARK`.

## BSARD Phase 1 Gate

BSARD is the current Phase 1 proxy for an expert-annotated French-language
statutory retrieval benchmark. It is Belgian law, not France LEGI, so a passing
run supports only the scoped external-benchmark claim recorded in the artifact.

Example with OpenRouter bge-m3:

```bash
python3 external-benchmarks/bsard_benchmark.py \
  --base-url https://openrouter.ai/api/v1 \
  --model baai/bge-m3 \
  --api-key-env OPENROUTER_API_KEY \
  --out work/03-implementation/02-evidence/phase1-external-benchmark-bsard.json

JURISEARCH_PHASE1_EXTERNAL_BENCHMARK=work/03-implementation/02-evidence/phase1-external-benchmark-bsard.json \
  cargo run -p jurisearch-cli -- status
```

Use `--limit-corpus` and `--limit-questions` only for smoke testing. A gate
artifact must use the full BSARD corpus and human-authored `test` questions.
`jurisearch status` also enforces the current Phase 1 policy floors:
hybrid `recall_at_20 >= 0.75`, `ndcg_at_20 >= 0.60`, and `mrr_at_20 >= 0.50`.
Gate artifacts must use the default `--k 20`; other `k` values fail closed
because the status gate requires the `*_at_20` metric keys.
