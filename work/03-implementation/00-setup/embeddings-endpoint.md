# `jurisearch` — Embeddings Endpoint (verified)

Date: 2026-06-21
Status: provisional default verified locally; final model is eval-gated at plan task 1.7
Refs: `DESIGN §11.2` (fingerprint), `DESIGN §14` (config), `RESEARCH §2`, `IMPLEMENTATION_PLAN` 0.4 / W5 / 1.7

The dense leg uses an OpenAI-compatible `/v1/embeddings` endpoint (D5). The provisional benchmark-default model is **`bge-m3`** (multilingual incl. strong French; 1024-d dense; native 8192 context; dense + learned-sparse + ColBERT for the hybrid/Phase-3 roadmap). Served locally via `llama.cpp`.

## Verified launch command (llama.cpp `llama-server`)

```bash
./build/bin/llama-server \
    -hf gpustack/bge-m3-GGUF:FP16 \
    --embeddings \
    --pooling cls \
    --parallel 8 \
    -ngl 99 \
    -t 8 \
    -c 32768 -b 8192 -ub 8192 \
    --host 127.0.0.1 --port 8097
```

- **Model repo:** `gpustack/bge-m3-GGUF` (HF). Quants: `FP16` (best), `Q8_0` (635 MB, near-identical) — avoid `Q4` and below for retrieval. `-hf …:FP16` auto-downloads/caches.
- **`--pooling cls`** — bge-m3 is trained with **CLS-token** pooling; `mean` degrades quality.
- **No RoPE/YaRN flags** — bge-m3 (XLM-RoBERTa, absolute positions) is natively 8192; the `--rope-scaling yarn --rope-freq-scale 0.75` used for `nomic-embed-text-v1.5` would distort bge-m3 and must be omitted.
- **Context vs throughput:** `llama-server` splits `-c` across `--parallel` slots, so `-c 32768 --parallel 8` → ~4096 tokens/slot. Use `-c 65536` for the full 8192/slot, or lower `--parallel`. No GPU → drop `-ngl 99` (model is ~568 M, CPU-fine).

## Verified behaviour (2026-06-21)

Tested against `http://127.0.0.1:8097/v1/embeddings`:

| Check | Result |
|---|---|
| Endpoint | responds (OpenAI-compatible) |
| Dimension | **1024** (correct for bge-m3) |
| L2 norm | **1.0** (normalized → cosine-ready) |
| cos(FR legal query, relevant statute text) | **0.748** |
| cos(FR legal query, irrelevant pie recipe) | **0.276** |
| Semantic discrimination | strong, correct (relevant ≫ irrelevant) — also validates `pooling=cls` |

## jurisearch config / index fingerprint (`DESIGN §11.2`)

```toml
[embedding]
provider  = "openai_compatible"
base_url  = "http://127.0.0.1:8097/v1"
model     = "bge-m3"
api_key   = "no-key"            # local llama.cpp; or via JURISEARCH_EMBED_API_KEY
dimension = 1024                # hard-checked against the index; mismatch = error
normalize = true
pooling   = "cls"
```

Record `provider, base_url-class, model, dimension(1024), normalize(true), pooling(cls)` in the index manifest. Document and query embeddings **must** share this fingerprint; a mismatch is a hard error (no degraded results). A local `127.0.0.1` endpoint is still treated as a "remote provider" by the CLI.

## Why bge-m3 (and why not `nomic-embed-text-v1.5`)

- jurisearch is **French legal**. `nomic-embed-text-v1.5` is **English-optimized** (quality drops on multilingual text); `bge-m3` is strongly multilingual incl. French (top open model on MTEB-French). For this corpus that's decisive.
- bge-m3 also emits dense + learned-sparse + ColBERT from one model, matching the hybrid retrieval and the Phase-3 SPLADE/ColBERT upgrade path; nomic is dense-only.

**This remains provisional.** Plan task **1.7** benchmarks `bge-m3` vs French specialists (`sentence-camembert-large`, `Solon`) and ≥1 strong hosted multilingual model (and may consider current SOTA such as Qwen3-Embedding), choosing the winner on **French legal retrieval metrics after fusion**. If the winner differs, run the explicit re-embed + vector-index migration (manifest fingerprint bump) — note bge-m3's 1024-d vs nomic's 768-d means any model swap is a full re-embed, not a config flip.

## Cross-refs
- `DESIGN §14` config example updated to `pooling = "cls"` (was `"mean"`, a nomic-default slip).
- `work/03-implementation/00-setup/PREREQUISITES.md §3` (embeddings infrastructure).
