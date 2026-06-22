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
- `work/03-implementation/02-evidence/2026-06-22-bsard-full-benchmark-result.md`
- `work/03-implementation/02-evidence/phase1-external-benchmark-bsard.json`
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
- Full-result review verdict: `GO`
  - Applied the non-blocking clarifications that BSARD BM25/RRF metrics are from the standalone Python harness and that the status snippet is hand-condensed from the literal `phase1_gate` JSON shape.

## Full Benchmark Result

The full BSARD benchmark was run without corpus/question limits:

```bash
python3 external-benchmarks/bsard_benchmark.py \
  --base-url https://openrouter.ai/api/v1 \
  --model baai/bge-m3 \
  --api-key-env OPENROUTER_API_KEY \
  --embed-batch-size 16 \
  --out work/03-implementation/02-evidence/phase1-external-benchmark-bsard.json
```

Result artifact:

- `state=failed`
- dataset revision `f3ca6a396a47c4a3afd26b766b5abc0a56bb4205`
- `22,633` corpus documents
- `222` test questions
- runtime `2,339.89s`

Metrics:

| Mode | Recall@20 | Success@20 | MRR@20 | nDCG@20 |
|---|---:|---:|---:|---:|
| BM25 | `0.3789` | `0.5270` | `0.2699` | `0.2597` |
| Dense | `0.4784` | `0.6712` | `0.3843` | `0.3512` |
| Hybrid RRF | `0.4683` | `0.6261` | `0.3389` | `0.3297` |

The artifact misses every status-enforced hybrid floor:

- recall@20 floor `0.75`, observed `0.4683`
- nDCG@20 floor `0.60`, observed `0.3297`
- MRR@20 floor `0.50`, observed `0.3389`

Dense retrieval outperformed the standalone benchmark harness RRF hybrid on BSARD, so the immediate quality work should investigate retrieval/ranking rather than relaxing the gate.

Scope caveat: these BM25 and RRF numbers come from the standalone external Python harness, not the production Rust `pg_search` analyzer or production retrieval pipeline. The shared production-relevant component is the locked `bge-m3` embedding fingerprint.

Status validation against the completed LEGI index:

```bash
JURISEARCH_INDEX_DIR=/home/pierre/Work/jurisearch/index/phase1-freemium-20250713 \
JURISEARCH_PHASE1_EXTERNAL_BENCHMARK=work/03-implementation/02-evidence/phase1-external-benchmark-bsard.json \
  cargo run -q -p jurisearch-cli -- status
```

Relevant result:

- `phase1_gate.state=not_ready`
- `phase1_gate.claim_allowed=false`
- `external_expert_annotated_eval=fail`
- All other Phase 1 checks pass: index query readiness, completed ingest run, zero failed members, projection coverage, embedding coverage, replay snapshot, final embedding model, and reranker decision.

The status summary above is hand-condensed; the literal CLI output stores checks under `phase1_gate.checks[]` and artifact details under `phase1_gate.external_benchmark.*`.

Detailed evidence is in `work/03-implementation/02-evidence/2026-06-22-bsard-full-benchmark-result.md`.

## Next Step

Do not lower thresholds to make this artifact pass. The next step is a focused benchmark-quality investigation:

- verify BSARD text/qrels/ID adaptation and whether title/metadata should be included;
- compare dense-only, BM25-only, and hybrid variants because dense currently beats the benchmark harness RRF hybrid;
- decide whether the eventual gate should keep using the standalone Python proxy harness or execute the production Rust retrieval pipeline against BSARD;
- tune RRF/BM25 analyzer/query preprocessing on a development split or separate candidate set before rerunning the locked full test split;
- measure whether the deferred reranker can clear the external benchmark with acceptable latency and fallback behavior;
- keep LLeQA as a secondary external candidate under the same artifact/gate discipline.

## Open Risks

- Full BSARD run completed on 2026-06-22, but the artifact failed the current quality floors.
- The harness still writes the `.npz` cache only after all document and query embeddings succeed. Retries help, but a hard endpoint failure can require a rerun.
- BSARD is Belgian statutory law. Passing the gate supports only the scoped external French-language statutory benchmark claim, not France-LEGI human-reviewed gold.
- BSARD/LLeQA are CC-BY-NC-SA-4.0; keep use eval-only and do not redistribute dataset content.
