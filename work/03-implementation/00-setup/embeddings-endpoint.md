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

Three **fingerprint-identical** bge-m3 nodes are available (all `gpustack/bge-m3-GGUF:FP16`, 1024-d / cls / normalized; cross-node cosine 0.99997): `127.0.0.1:8097`, `192.168.1.57:8097`, `192.168.1.27:8097`.

Measured batched throughput — real `contextualized_body` chunk payloads (avg ~30 tokens), batch 32, 4 concurrent:

| node | texts/s | note |
|---|---|---|
| localhost `:8097` | ~58 | slowest — weaker box + contended by the running ingest |
| `192.168.1.57` | ~194 | |
| `192.168.1.27` | ~146 | |
| **3-node pooled** (least-busy queue) | **~288** | Python/GIL-limited client; sum-of-nodes ≈ 398 |

Projected dense projection over the corpus (~1.85 M chunks at measurement):

| strategy | wall-clock |
|---|---|
| localhost-only | **~8.9 h** |
| 3-node pooled | **~1.8 h** (~5×; a Rust pooled client should beat this, ~1.3 h) |

**Build-time projection recommendation (W4/W5):**
- **Pool the N endpoints with a least-outstanding-requests dispatcher** — NOT round-robin (localhost is ~3× slower and would gate every round); the queue auto-balanced (localhost took 7 batches vs 23/17 on the remotes).
- **Deprioritize/exclude localhost for the bulk pass** (slow + ingest-contended); reserve it for query-time.
- Batched + **resumable** via the projection-coverage accounting: completed batches are durable, failed batches abort the run, and a rerun skips chunks that already have matching embeddings. This extends the existing `jurisearch ingest embed-chunks` from one endpoint to a pool.
- All nodes share the locked fingerprint → mixing them is safe for the index (vectors equivalent for retrieval; not bit-identical — irrelevant here).

## Why bge-m3 (and why not `nomic-embed-text-v1.5`)

- jurisearch is **French legal**. `nomic-embed-text-v1.5` is **English-optimized** (quality drops on multilingual text); `bge-m3` is strongly multilingual incl. French (top open model on MTEB-French). For this corpus that's decisive.
- bge-m3 also emits dense + learned-sparse + ColBERT from one model, matching the hybrid retrieval and the Phase-3 SPLADE/ColBERT upgrade path; nomic is dense-only.

**Locked (2026-06-22).** bge-m3 was validated against the French specialists `sentence-camembert-large` and `Solon` on a French-legal retrieval set and found **statistically indistinguishable** → locked as v1 (DECISIONS **D21**; evidence: `work/03-implementation/02-evidence/2026-06-22-bge-m3-vs-french-embeddings.md`). The 1.7 comparative bake-off is skipped; the re-embed + vector-index migration path is retained for any future change (a model swap is a full re-embed, since dimension/pooling are part of the fingerprint).

## Cross-refs
- `DESIGN §14` config example updated to `pooling = "cls"` (was `"mean"`, a nomic-default slip).
- `work/03-implementation/00-setup/PREREQUISITES.md §3` (embeddings infrastructure).
