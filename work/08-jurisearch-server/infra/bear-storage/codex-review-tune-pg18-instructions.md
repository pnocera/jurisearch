# Codex review — tune-pg18.sh

## Scope
`work/08-jurisearch-server/infra/bear-storage/tune-pg18.sh` (run INSIDE CT 110 as root).

Tunes the standalone PG18 for performance and rebuilds the `chunk_embeddings` IVFFlat index at a
corpus-sized `lists`. The corpus DB (`jurisearch`, 163 GB, 4.64 M chunk embeddings) is loaded and live.

## Ground truth (CT 110)
- Now **48 cores + 192 GB RAM** (just bumped); host has 64t/251 GB.
- DB ~163 GB → fits in cache with 192 GB RAM. `/dev/shm` = 126 GB (parallel DSM fine).
- PG18.4 + pgvector 0.8.3 + pg_search 0.24.1; cluster `18/main`, db `jurisearch`.
- Was running on Debian defaults; the IVFFlat had `lists=32` (→ ~13 s dense queries).
- The jurisearch code creates this exact index as `ivfflat (embedding vector_l2_ops) WITH (lists=…)`.

## What to verify
1. **Config values for 48c/192 GB, dedicated DB box.** shared_buffers=48 GB (25%),
   effective_cache_size=160 GB, work_mem=512 MB, maintenance_work_mem=16 GB, max_worker_processes /
   max_parallel_workers=48, max_parallel_workers_per_gather=16, max_parallel_maintenance_workers=12,
   random_page_cost=1.1, effective_io_concurrency=256, max_wal_size=16 GB. Are these sane and safe?
   In particular: is `work_mem=512 MB` safe given up to 48 parallel workers (per-node × per-worker
   memory) — could a big parallel hash/sort OOM 192 GB? Is shared_buffers=48 GB + 160 GB
   effective_cache_size coherent? Does `max_worker_processes` need to be ≥ max_parallel_workers (it's
   equal here — ok)? Flag any value that should change.
2. **The `lists` heuristic.** `rows<=1e6 ? rows/1000 : sqrt(rows)` → 2154 for 4.64 M. Is that the right
   pgvector guidance, and is the awk sqrt + integer floor correct? Is rebuilding with `vector_l2_ops`
   (matching the existing index + the jurisearch code) correct for normalized bge-m3 vectors?
3. **Sequence + restart.** ALTER SYSTEM → `pg_ctlcluster restart` (needed for shared_buffers) → wait for
   connections → DROP/CREATE the index with session maintenance_work_mem + parallel workers → ANALYZE.
   Is the ordering right and fail-closed? Does pgvector 0.8.x build IVFFlat in parallel via
   max_parallel_maintenance_workers (so setting it helps)?
4. **Safety.** `set -Eeuo pipefail` + ERR trap + TUNE-FAILED sentinel; ON_ERROR_STOP on all psql. The
   DROP INDEX leaves a window with no ANN index until CREATE completes — acceptable on this
   not-yet-production box? Any risk to the corpus data (there should be none — config + index only)?
5. The nested `psqlf`/`psqlc` helpers and heredocs through `su - postgres -c "psql -f -"` — any quoting
   or stdin pitfall?

## Output
Severity-tagged findings (BLOCKER/WARN/NIT) with concrete fixes for every severity; note what you
checked and found correct. End with exactly `VERDICT: GO` or `VERDICT: FIXES_REQUIRED`.
