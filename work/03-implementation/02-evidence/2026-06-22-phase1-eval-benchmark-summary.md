# Phase 1 LEGI Eval Benchmark Summary

Date: 2026-06-22

Index:

- `/home/pierre/Work/jurisearch/index/phase1-freemium-20250713`
- Source: DILA LEGI Freemium baseline `Freemium_legi_global_20250713-140000.tar.gz`
- Embedding fingerprint: `bge-m3:1024:normalize:true`
- Dense projection evidence: `2026-06-22-openrouter-dense-projection-run.log`

Commands:

```bash
JURISEARCH_CONFIG=none \
JURISEARCH_EMBED_BASE_URL=http://127.0.0.1:8097/v1 \
target/debug/jurisearch \
  --index-dir /home/pierre/Work/jurisearch/index/phase1-freemium-20250713 \
  eval phase1 --mode <bm25|dense|hybrid> --top-k 20
```

## Release-Gating Fixture Results

| Mode | Passed | Failed | Elapsed |
|---|---:|---:|---:|
| BM25 | 4 | 0 | 16.62s |
| dense | 2 | 2 | 14.09s |
| hybrid | 4 | 0 | 21.24s |

Evidence files:

- `2026-06-22-phase1-eval-bm25-top20.json`
- `2026-06-22-phase1-eval-bm25-top20.time.json`
- `2026-06-22-phase1-eval-dense-top20.json`
- `2026-06-22-phase1-eval-dense-top20.time.json`
- `2026-06-22-phase1-eval-hybrid-top20.json`
- `2026-06-22-phase1-eval-hybrid-top20.time.json`

Interpretation:

- Hybrid passes all currently executable release-gating statutory fixtures at top 20.
- BM25 also passes all four fixtures, so the current fixture set does not yet prove hybrid beats BM25.
- Dense-only misses two statutory fixtures; dense remains useful but is not sufficient by itself for this release-candidate set.

## Include-Dev Hybrid Run

Command:

```bash
JURISEARCH_CONFIG=none \
JURISEARCH_EMBED_BASE_URL=http://127.0.0.1:8097/v1 \
target/debug/jurisearch \
  --index-dir /home/pierre/Work/jurisearch/index/phase1-freemium-20250713 \
  eval phase1 --include-dev --mode hybrid --top-k 20
```

Result:

- Passed: 5
- Failed: 1
- Elapsed: 35.30s
- Failure: `legi-hierarchy-temporal-sibling-2000`

Evidence files:

- `2026-06-22-phase1-eval-hybrid-include-dev-top20.json`
- `2026-06-22-phase1-eval-hybrid-include-dev-top20.time.json`

The failed dev fixture expects `legi:LEGIARTI000006850361@1999-03-28` for a hierarchy-sensitive temporal sibling query. It is not in the top 20 hybrid candidates. This is non-release-gating evidence, but it should remain a W2/W5 follow-up before promoting hierarchy-sensitive claims.

## Status Gate Evidence

Evidence file:

- `2026-06-22-phase1-status-after-d21-gate-fix.json`

Durable embedding manifest excerpt:

- `embedding_fingerprint`: `bge-m3:1024:normalize:true`
- `model`: `bge-m3`
- `dimension`: `1024`
- `normalize`: `true`
- `coverage`: `1,852,745` chunks / `1,852,745` embeddings
- `vector_index.name`: `chunk_embeddings_embedding_ivfflat_idx`

Relevant checks after the D21 gate fix:

- `index_query_ready`: pass
- `latest_completed_ingest_run`: pass
- `failed_members`: pass
- `projection_coverage`: pass
- `embedding_coverage`: pass
- `replay_snapshot`: pass
- `final_embedding_model`: pass from the stored `ingest_health.embedding_manifest`, not from transient runtime embedding config
- `release_gating_eval_fixtures`: pending
- `reranker_decision`: pending

Remaining blockers:

- Release-gating fixtures still need named human legal-domain review before the Phase 1 claim can open.
- Reranker adoption or deferral still needs a benchmark decision artifact.
- The replay snapshot computation is correct but operationally slow on the full corpus because `status` recomputes ordered hashes over documents, chunks, embeddings, manifests, and about 12.9M publisher edges.
