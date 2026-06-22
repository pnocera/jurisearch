# `jurisearch` — Embeddings Endpoint (verified)

Date: 2026-06-21
Status: bge-m3 **locked as v1** (DECISIONS D21); endpoint + throughput verified; build-time projection pool measured 2026-06-22
Refs: `DESIGN §11.2` (fingerprint), `DESIGN §14` (config), `RESEARCH §2`, `IMPLEMENTATION_PLAN` 0.4 / W5 / 1.7

The dense leg uses an OpenAI-compatible `/v1/embeddings` endpoint (D5). The **locked v1** model is **`bge-m3`** (DECISIONS D21; multilingual incl. strong French; 1024-d dense; native 8192 context; dense + learned-sparse + ColBERT for the hybrid/Phase-3 roadmap). Served via `llama.cpp`.

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

## Client contract check

The Rust endpoint contract is codified in `jurisearch-embed`. With the local `llama.cpp` endpoint running:

```bash
cargo test -p jurisearch-embed --test live_endpoint -- --ignored --nocapture
```

This sends an OpenAI-compatible embedding request through the production client, verifies the configured Phase 0 fingerprint, and fails if the endpoint does not return a 1024-d normalized `bge-m3` vector.

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

## Embedding throughput & the build-time pool (measured 2026-06-22)

The dense projection over the LEGI corpus is the next gate-blocking step after ingest (`--safe-mode` defers embeddings; the vectors table stays empty until `jurisearch ingest embed-chunks` runs). It is **build-time only** — query-time embeds one tiny query, so **retrieval stays on the single local endpoint**.

Available bge-m3-compatible nodes are volatile. The LAN nodes (`127.0.0.1:8097`, `192.168.1.57:8097`, `192.168.1.27:8097`) share the locked local fingerprint (`gpustack/bge-m3-GGUF:FP16`, 1024-d / cls / normalized; cross-node cosine ~0.99997), but measured throughput changed materially between runs as machine load shifted. OpenRouter's hosted `baai/bge-m3` endpoint measured fingerprint-compatible as well (1024-d, normalized, cosine 0.999972 against local) and was stable.

Measured batched throughput on real `contextualized_body` chunk payloads:

| node | latest texts/s | earlier texts/s | note |
|---|---:|---:|---|
| localhost `:8097` | ~222 | ~58 | local load changed materially |
| `192.168.1.57:8097` | ~6 | ~194 | currently contended |
| `192.168.1.27:8097` | ~4 | ~146 | currently contended |
| OpenRouter `baai/bge-m3`, C=16 | **~292** | n/a | stable; C=8 ~149, C=32 ~188 |

Projected dense projection over the corpus (~1.85 M chunks at measurement):

| strategy | wall-clock |
|---|---|
| localhost-only at old ~58 t/s | ~8.9 h |
| OpenRouter-only at C=16 | ~1.8 h |
| OpenRouter + any uncongested LAN node | faster, if the LAN node is actually free |

**Build-time projection recommendation (W4/W5):**
- Use OpenRouter as the reliable build-time backbone for LEGI dense projection: `https://openrouter.ai/api/v1`, request model `baai/bge-m3`, `Authorization: Bearer $OPENROUTER_API_KEY`, concurrency cap **16**.
- Keep the canonical jurisearch model/fingerprint as `bge-m3:1024:normalize:true`. The OpenRouter slug is a request alias only; stored rows and query compatibility still use locked `bge-m3`.
- Do **not** set `JURISEARCH_EMBED_MODEL=baai/bge-m3`; that would change the canonical storage fingerprint. Keep `model = "bge-m3"` and put `baai/bge-m3` only in the pool request-model slot.
- Use the explicit pool spec when endpoints need different request models or secrets:

```bash
JURISEARCH_EMBED_BASE_URL="http://127.0.0.1:8097/v1" \
JURISEARCH_EMBED_POOL="https://openrouter.ai/api/v1|baai/bge-m3|OPENROUTER_API_KEY" \
jurisearch ingest embed-chunks --batch-size 32 --pool-concurrency 16
```

- `JURISEARCH_EMBED_BASE_URLS` remains suitable only for same-model/same-auth local pools. Use `JURISEARCH_EMBED_POOL` for mixed local/hosted pools so hosted secrets are not sent to LAN endpoints. When `JURISEARCH_EMBED_POOL` is set it supersedes `JURISEARCH_EMBED_BASE_URLS` for `ingest embed-chunks`; `status` reports this as `pool_overrides_base_urls: true`.
- Batched + **resumable** via the projection-coverage accounting: completed batches are durable, failed batches abort the run, and a rerun skips chunks that already have matching embeddings.
- OpenRouter can return an error-shaped JSON body even on a successful HTTP status (observed with context-length/provider errors). The client treats this as an endpoint error, retries transient endpoint/invalid-response failures, and includes a bounded body excerpt on final failure.
- A small number of LEGI chunks exceed bge-m3's context budget (26 chunks >24k chars in the 2025-07-13 freemium index; max observed 72,124 chars). `embed-chunks` truncates only the outbound embedding request text to the tighter of `max_input_chars` and the estimated token budget expressed as chars, then reports `embedding_inputs_truncated`; stored chunk text, lexical search, chunk IDs, and the canonical embedding fingerprint remain unchanged. The default Phase 0 request char budget is **20,000**: 24,000 chars still exceeded OpenRouter's 8192-token limit on the longest chunks, while all 26 over-24k chunks succeeded as one 20k-truncated OpenRouter batch. If an exact tokenizer is configured, keep the char budget conservative enough that tokenizer-counted inputs still clear the endpoint token limit.
- LEGI is public official text, so OpenRouter egress is acceptable for Phase 1. Revisit this for Phase 2 Judilibre: even pseudonymized decisions may warrant local-only embedding.

## Why bge-m3 (and why not `nomic-embed-text-v1.5`)

- jurisearch is **French legal**. `nomic-embed-text-v1.5` is **English-optimized** (quality drops on multilingual text); `bge-m3` is strongly multilingual incl. French (top open model on MTEB-French). For this corpus that's decisive.
- bge-m3 also emits dense + learned-sparse + ColBERT from one model, matching the hybrid retrieval and the Phase-3 SPLADE/ColBERT upgrade path; nomic is dense-only.

**Locked (2026-06-22).** bge-m3 was validated against the French specialists `sentence-camembert-large` and `Solon` on a French-legal retrieval set and found **statistically indistinguishable** → locked as v1 (DECISIONS **D21**; evidence: `work/03-implementation/02-evidence/2026-06-22-bge-m3-vs-french-embeddings.md`). The 1.7 comparative bake-off is skipped; the re-embed + vector-index migration path is retained for any future change (a model swap is a full re-embed, since dimension/pooling are part of the fingerprint).

## Cross-refs
- `DESIGN §14` config example updated to `pooling = "cls"` (was `"mean"`, a nomic-default slip).
- `work/03-implementation/00-setup/PREREQUISITES.md §3` (embeddings infrastructure).
