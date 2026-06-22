# Handoff: BSARD External Benchmark Gate

Date: 2026-06-22

## Current State

- Phase 1 no longer depends on unavailable local human legal-domain review.
- `jurisearch status` exposes `phase1_gate.external_benchmark` and reads a durable artifact from `JURISEARCH_PHASE1_EXTERNAL_BENCHMARK`.
- The gate stays fail-closed unless the artifact is a full BSARD `test` run with valid metadata, non-empty evidence, the locked bge-m3 fingerprint, and status-enforced quality floors.
- The external benchmark runner is Python, outside the Rust CLI: `external-benchmarks/bsard_benchmark.py`.

## New Files

- `external-benchmarks/README.md`
- `external-benchmarks/bsard_benchmark.py`
- `work/03-implementation/02-evidence/2026-06-22-bsard-external-benchmark-harness-smoke.md`
- Claude review artifacts:
  - `work/03-implementation/01-reviews/2026-06-22-bsard-external-benchmark-harness-claude-review.md`
  - `work/03-implementation/01-reviews/2026-06-22-bsard-external-benchmark-harness-claude-review-r2.md`

## Gate Policy

Required artifact environment variable:

```bash
export JURISEARCH_PHASE1_EXTERNAL_BENCHMARK=/path/to/phase1-external-benchmark-bsard.json
```

Status rejects artifacts unless all of these are true:

- `kind == "phase1_external_expert_benchmark"`
- `schema_version == 1`
- `dataset.id == "maastrichtlawtech/bsard"`
- `dataset.revision` is pinned and not `unknown`
- `dataset.question_split == "test"`
- `dataset.jurisdiction == "belgium"`
- `dataset.usage_scope == "eval_only"`
- `dataset.license == "cc-by-nc-sa-4.0"`
- `dataset.limit_corpus == null` and `dataset.limit_questions == null`
- `dataset.corpus_documents >= 22000`
- `dataset.questions >= 200`
- `embedding.fingerprint_model == "bge-m3"`
- `embedding.dimension == 1024`
- `embedding.normalize == true`
- `evidence` is non-empty
- hybrid `recall_at_20 >= threshold >= 0.75`
- hybrid `ndcg_at_20 >= threshold >= 0.60`
- hybrid `mrr_at_20 >= threshold >= 0.50`

The gate re-derives pass from metrics and thresholds; it does not trust artifact `state` alone.

## Commands Already Run

```bash
python3 -m py_compile external-benchmarks/bsard_benchmark.py
cargo fmt --all
cargo test -p jurisearch-cli external_benchmark
cargo test -p jurisearch-cli --test cli_contract
```

Result:

- `external_benchmark`: 5/5 passed.
- `cli_contract`: 45 passed, 2 ignored live-endpoint tests.

Limited smoke against OpenRouter:

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

Status correctly rejected the limited smoke artifact:

```json
{
  "external_state": "failed",
  "external_check": "fail",
  "claim_allowed": false
}
```

Detailed smoke evidence is in `work/03-implementation/02-evidence/2026-06-22-bsard-external-benchmark-harness-smoke.md`.

## Claude Review

- R1 verdict: `FIXES_REQUIRED`
  - Fixed threshold floors, metric-vs-threshold re-derivation, locked embedding checks, revision pinning, true recall, artifact identity/size checks, retries, cache-key hardening, deterministic tie-breaks.
- R2 verdict: `GO`
  - Applied the non-blocking README note that gate artifacts must use default `--k 20`.

## Next Step

Run the full BSARD benchmark without limits:

```bash
python3 external-benchmarks/bsard_benchmark.py \
  --base-url https://openrouter.ai/api/v1 \
  --model baai/bge-m3 \
  --api-key-env OPENROUTER_API_KEY \
  --embed-batch-size 16 \
  --out work/03-implementation/02-evidence/phase1-external-benchmark-bsard.json
```

Then validate status:

```bash
JURISEARCH_PHASE1_EXTERNAL_BENCHMARK=work/03-implementation/02-evidence/phase1-external-benchmark-bsard.json \
  cargo run -q -p jurisearch-cli -- status
```

Expected:

- If metrics clear floors, `external_expert_annotated_eval` passes.
- `claim_allowed` may still depend on other Phase 1 checks in the live index.

## Open Risks

- Full BSARD run will embed about 22.6k documents plus 222 test questions; OpenRouter throughput may vary.
- The harness still writes the `.npz` cache only after all document and query embeddings succeed. Retries help, but a hard endpoint failure can require a rerun.
- BSARD is Belgian statutory law. Passing the gate supports only the scoped external French-language statutory benchmark claim, not France-LEGI human-reviewed gold.
- BSARD/LLeQA are CC-BY-NC-SA-4.0; keep use eval-only and do not redistribute dataset content.
