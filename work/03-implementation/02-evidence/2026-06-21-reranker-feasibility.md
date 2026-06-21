# Reranker Feasibility Spike

Date: 2026-06-21
Scope: Phase 0.7 feasibility only; no reranker adoption decision.

## Decision

Phase 1 should implement the reranker as a pluggable provider shape first:

- `disabled` remains the default until eval proves a legal-task gain.
- `http` should be the first shippable provider because TEI exposes a `/rerank` endpoint and keeps heavyweight model/runtime packaging outside `jurisearch`.
- `local_onnx` via Rust `ort` is feasible enough for a follow-up benchmark, but should not be committed as the default path before latency, binary-size, and runtime-provider tests.
- Candle-native local inference is a research fallback, not the first implementation path.

Do not wire reranking into ranking until W2 can measure BM25-only, dense-only, hybrid, and hybrid+rerank ablations on legal fixtures.

## Candidate Model

First candidate: `BAAI/bge-reranker-v2-m3`.

Why it remains the first candidate:

- The upstream model card describes rerankers as cross-encoders that take query and passage together and output a similarity score, not an embedding.
- `bge-reranker-v2-m3` is listed as multilingual and based on `bge-m3`, matching the current provisional embedding family.
- Upstream examples show both FlagEmbedding and Transformers sequence-classification usage.
- A BAAI discussion states the model maximum length is 8192 tokens, but recommends `max_length=1024` because that was the fine-tuning maximum. Use `1024` as the Phase 1 benchmark default unless legal eval proves that longer rerank inputs are worth the cost.

Sources:

- BAAI model card: https://huggingface.co/BAAI/bge-reranker-v2-m3
- BAAI max-length discussion: https://huggingface.co/BAAI/bge-reranker-v2-m3/discussions/9

## Runtime Options

### HTTP Provider

Recommended first implementation target.

TEI supports reranker models as sequence-classification cross-encoders and documents a `/rerank` request shape with `query` plus `texts`. It also provides CPU x86_64 images and local CPU install instructions, which fits `jurisearch` better than embedding Python or model runtimes in the CLI. The HTTP provider can share the existing `JURISEARCH_` secret/env policy and the current "diagnostics on stderr, JSON on stdout" discipline.

Phase 1 provider sketch:

```json
{
  "provider": "http",
  "base_url": "http://127.0.0.1:8080",
  "model": "BAAI/bge-reranker-v2-m3",
  "max_length": 1024,
  "top_n": 50,
  "timeout_ms": 30000
}
```

Source: https://github.com/huggingface/text-embeddings-inference

### Local ONNX Through `ort`

Feasible as the next benchmark spike, not as a default.

Evidence:

- `onnx-community/bge-reranker-v2-m3-ONNX` exists and is explicitly an ONNX conversion of the BAAI model.
- The Rust `ort` crate is the current Rust binding for ONNX Runtime.
- Local workstation has a strong CPU: AMD Ryzen AI MAX+ PRO 395, 16 cores / 32 threads, AVX512 flags present.

Risks:

- ONNX model provenance and exact tokenizer/pair input contract must be verified against the upstream BAAI tokenizer.
- `ort` runtime packaging and execution-provider behavior must be measured in this project before shipping.
- Reproducible model cache/download policy must be integrated with `model fetch` / setup before local inference can be user-facing.

Sources:

- ONNX conversion: https://huggingface.co/onnx-community/bge-reranker-v2-m3-ONNX
- Rust ONNX Runtime binding: https://docs.rs/ort

### Candle Native

Not recommended as the first path.

Candle is a Rust ML framework and includes `candle-transformers` and `candle-onnx`, but this project has no local Candle model implementation yet, no existing tokenizer/model-cache integration, and no confirmed cross-encoder implementation for this exact reranker. Candle remains useful if ONNX packaging fails or if we want a fully Rust local path after HTTP/ORT data is available.

Source: https://github.com/huggingface/candle

### Local GPU

Do not assume GPU acceleration for Phase 1.

Machine facts observed locally:

- `nvidia-smi` exposes no NVIDIA device.
- `rocminfo` sees AMD Radeon 8060S Graphics (`gfx1151`) and ROCm/MIGraphX libraries are installed.
- TEI documents AMD ROCm support as experimental for AMD Instinct GPUs, not as a guaranteed path for this integrated Radeon workstation.

Conclusion: benchmark CPU first. Treat AMD GPU acceleration as optional future investigation, not as a Phase 1 dependency.

## Measurement Plan

Minimum Phase 1 benchmark matrix:

- Providers: `disabled`, `http`, `local_onnx`.
- Candidate counts: rerank top 20, 50, 100 fused candidates.
- Max lengths: 512 and 1024; only test 2048+ if legal eval shows missed context.
- Metrics: latency p50/p95, timeout/failure rate, memory footprint, and legal retrieval quality delta.
- Quality gates: known-article lookup, conceptual statutory retrieval, historical `--as-of`, and statute-to-jurisprudence tasks once decision corpora exist.

Recommended acceptance threshold for adoption:

- Hybrid+rerank must improve legal eval quality enough to justify added latency and local/HTTP operational complexity.
- Reranker failure must degrade to fused hybrid order, not fail the whole `search`.
- Provider choice must be explicit in status and manifests.

## Implementation Follow-Up

Phase 1 code should add only the provider seam first:

- `RerankerConfig { provider: disabled | http | local_onnx, model, base_url, max_length, top_n, timeout }`
- `RerankCandidate { chunk_id, document_id, title, snippet/body }`
- `RerankScore { chunk_id, score, provider, model }`
- `rerank(candidates, query) -> ordered candidates`, with pass-through behavior for `disabled`.

Do not couple reranking to `pg_search` or `pgvector`; run it after RRF over a bounded candidate set.

## Verification Performed

- Source review of the BAAI model card, BAAI max-length discussion, ONNX conversion page, TEI README, `ort` crate docs, and Candle repository.
- Local machine inspection:
  - `lscpu`: AMD Ryzen AI MAX+ PRO 395, 32 logical CPUs, AVX512 flags present.
  - `nvidia-smi`: no NVIDIA device output.
  - `rocminfo`: AMD Radeon 8060S Graphics visible as `gfx1151`.
  - `cargo search ort --limit 3`: current `ort = 2.0.0-rc.12`.
  - Hugging Face cache is effectively empty, so no local reranker weights are already present.
