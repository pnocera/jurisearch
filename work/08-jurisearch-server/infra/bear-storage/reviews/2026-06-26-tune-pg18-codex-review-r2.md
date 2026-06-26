# Codex Re-review - tune-pg18.sh r2

## R2 Finding Status

1. RESOLVED - Persistent memory settings are no longer the OOM/autovacuum hazard from r1. `work_mem` is now `128MB`, persistent `maintenance_work_mem` is `2GB`, `autovacuum_work_mem` is explicitly capped at `1GB`, and the `16GB` ceiling is only applied by `SET LOCAL maintenance_work_mem` for the IVFFlat build transaction.
2. RESOLVED - The rebuild is now atomic for index availability. `BEGIN; SET LOCAL ...; DROP INDEX IF EXISTS ...; CREATE INDEX ...; COMMIT;` means a failed non-concurrent `CREATE INDEX` aborts the transaction and rolls back the drop, preserving the old `chunk_embeddings_embedding_ivfflat_idx`; moving `ANALYZE` after commit is correct.
3. RESOLVED - Verification now uses `psqlf "$DB"` with `ON_ERROR_STOP=1`, captures the full output into `verify_out`, and only filters that already-successful output afterward. A SQL or `psql` failure makes the assignment return non-zero under `set -e` before the success sentinel can print.
4. RESOLVED - The `runuser -u postgres -- psql ...` helpers are argv-style and avoid shell parsing of the database name or SQL. The `-f -` form still receives the caller's heredoc stdin through `runuser`, so the rewrite fixes the quoting concern without breaking stdin.

## New Issues

None found.

## Checks That Passed

- `shared_buffers=48GB` remains sane for a dedicated 192 GB CT, and `effective_cache_size=160GB` remains coherent as a planner cache estimate rather than an allocation.
- `max_worker_processes=48`, `max_parallel_workers=48`, `max_parallel_workers_per_gather=16`, and `max_parallel_maintenance_workers=12` remain internally consistent for this operator-controlled producer profile.
- The `lists` heuristic is still the pgvector guidance: `rows / 1000` up to 1M rows and integer-floor `sqrt(rows)` above that. For the stated 4.64M corpus this gives `2154`, with a guard to keep tiny corpora at least `1`.
- The rebuilt index still uses `ivfflat (embedding vector_l2_ops)` with the established `chunk_embeddings_embedding_ivfflat_idx` name, matching the storage finalizer and the live dense query path's `<->` L2 operator.
- The ordering remains correct: preconditions, row count and `lists`, `ALTER SYSTEM`, restart for postmaster/shared-buffer settings, connectivity wait, transactional index rebuild with local build memory, then post-commit `ANALYZE`.
- `bash -n tune-pg18.sh` passed.
- `shellcheck tune-pg18.sh` produced no diagnostics.

## Sources Checked

- `tune-pg18.sh`
- `codex-review-tune-pg18-instructions.md`
- `reviews/2026-06-26-tune-pg18-codex-review.md`
- `crates/jurisearch-storage/src/dense.rs`
- `crates/jurisearch-storage/src/retrieval/sql.rs`

VERDICT: GO
