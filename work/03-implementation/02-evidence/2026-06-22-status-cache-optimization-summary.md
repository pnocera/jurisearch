# Status Replay Snapshot Cache Evidence

Date: 2026-06-22

Scope:

- Default `jurisearch status` reads a cached replay snapshot from `index_manifest['replay_snapshot']` instead of recomputing ordered full-corpus hashes.
- `jurisearch status --deep` explicitly recomputes the replay snapshot and refreshes the manifest cache.
- Successful LEGI archive ingestion, hierarchy backfill, and dense embedding finalization refresh the cache at command boundaries.

Full-index evidence path:

- Index: `/home/pierre/Work/jurisearch/index/phase1-freemium-20250713`

Measured runs:

| Run | Source | Replay status | Elapsed |
|---|---:|---:|---:|
| `status` before cache | `missing` | `missing` | 3.00s |
| `status --deep` | `refreshed` | `available` | 589.34s |
| `status` after cache | `cached` | `available` | 2.87s |

Deep replay counts:

- Documents: 1,736,165
- Chunks: 1,852,745
- Publisher edges: 12,949,444
- Embeddings: 1,852,745
- Manifests: 2
- Signature: `430af44453662d6107a46e7baedde246`

Evidence files:

- `2026-06-22-status-cache-default-before.json`
- `2026-06-22-status-cache-default-before.time.json`
- `2026-06-22-status-cache-deep-refresh.json`
- `2026-06-22-status-cache-deep-refresh.time.json`
- `2026-06-22-status-cache-default-after.json`
- `2026-06-22-status-cache-default-after.time.json`
