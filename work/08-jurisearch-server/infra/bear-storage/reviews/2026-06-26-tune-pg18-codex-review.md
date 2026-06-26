# Codex Review - tune-pg18.sh

## Findings

### WARN - Persistent `work_mem=512MB` is not safe with the configured parallelism

`tune-pg18.sh:21` and `tune-pg18.sh:55` make `work_mem=512MB` a cluster-wide default while the same script allows 48 total parallel workers and 16 workers per gather (`tune-pg18.sh:57-60`). PostgreSQL applies `work_mem` per sort/hash operation, complex queries can have several such operations at once, and parallel workers apply these limits independently. Hash operations can also use `work_mem * hash_mem_multiplier`, with the default multiplier 2.0. That means one 16-worker parallel query can reach about 8.5 GB per sort node or about 17 GB per hash node before considering multiple nodes; the whole 48-worker pool can multiply that again. On a 192 GB CT with 48 GB shared buffers and a 163 GB corpus, that is too aggressive as a persistent default and can turn a few unlucky large parallel plans into OOM risk.

Concrete fix: lower the persistent default to a conservative value such as `64MB` or `128MB`, and use explicit session-local `SET work_mem = '512MB'` only for a known maintenance/evaluation session that needs it. Also cap autovacuum separately if `maintenance_work_mem=16GB` remains persistent, because PostgreSQL's default `autovacuum_work_mem = -1` inherits `maintenance_work_mem`; add an explicit `ALTER SYSTEM SET autovacuum_work_mem = '1GB';` or similar so multiple autovacuum workers cannot each reserve the 16 GB maintenance ceiling.

### WARN - The index rebuild is not atomic, so a failed `CREATE INDEX` leaves the corpus without the old ANN index

The rebuild block (`tune-pg18.sh:77-84`) executes `DROP INDEX IF EXISTS` and then `CREATE INDEX` as independent statements. `ON_ERROR_STOP` will make the script print the failure sentinel if `CREATE INDEX` fails, but it will not roll back the already-committed `DROP INDEX`. That contradicts the "Fails closed" claim for index availability: a memory error, operator-class error, interruption, or insufficient temporary space after the drop leaves `chunk_embeddings` data intact, but leaves no `chunk_embeddings_embedding_ivfflat_idx` until an operator reruns a successful build.

Concrete fix: make the rebuild block transactional:

```sql
BEGIN;
SET LOCAL maintenance_work_mem = '16GB';
SET LOCAL max_parallel_maintenance_workers = 12;
DROP INDEX IF EXISTS chunk_embeddings_embedding_ivfflat_idx;
CREATE INDEX chunk_embeddings_embedding_ivfflat_idx
ON chunk_embeddings USING ivfflat (embedding vector_l2_ops)
WITH (lists = ...);
ANALYZE chunk_embeddings;
ANALYZE chunks;
COMMIT;
```

That keeps the old index if the replacement build fails. If zero read-side index gap matters later, build a second index name and swap names in a short transaction, at the cost of extra disk during the build. For this not-yet-production box, the temporary no-index window during a successful rebuild is acceptable; the issue is the permanent no-index state after a failed rebuild.

### WARN - The final verification `psql` does not use `ON_ERROR_STOP`

All helper-driven `psql` calls set `ON_ERROR_STOP`, but the final verification call at `tune-pg18.sh:89-97` bypasses the helpers:

```bash
su - postgres -c "psql -d $DB -P pager=off -f -" <<SQL 2>&1 | grep ... | head
```

By default, `psql` continues processing after SQL errors and exits with the script-error code only when `ON_ERROR_STOP` is set. Because this pipeline also filters output through `grep | head`, a verification SQL error can be hidden if any later output matches the filter. The script can then emit `SENTINEL: TUNE-DONE` even though the verification query did not actually prove the ANN plan.

Concrete fix: run the verification through the same fail-closed path, for example `psqlf "$DB"` or `su - postgres -c "psql -v ON_ERROR_STOP=1 -d ... -f -"`, capture the full output/status first, then filter the captured output for display only after `psql` has succeeded.

### NIT - The `su -c` wrappers are safe for the current constants, but not for their advertised environment overrides

`psqlf` and `psqlc` interpolate the database name and SQL into a shell command string (`tune-pg18.sh:33-34`), and the verification call interpolates `$DB` the same way (`tune-pg18.sh:89`). With the script defaults (`DB=jurisearch`) and the fixed SQL strings currently used, this is not a practical failure. However, the script exposes `DB` as an environment override, and an overridden value containing spaces or shell metacharacters would be interpreted by the intermediate shell.

Concrete fix: prefer argv-style execution instead of shell-string execution, e.g. `runuser -u postgres -- psql -v ON_ERROR_STOP=1 -P pager=off -d "$db" -f -`, or at least shell-quote `DB` and avoid passing SQL through nested quotes.

## Checks That Passed

- `shared_buffers=48GB` is sane for this host class: it is exactly 25% of 192 GB, matching PostgreSQL's documented starting point for a dedicated database server. Keeping it below 40% also leaves the OS page cache useful.
- `effective_cache_size=160GB` is coherent as a planner estimate, not a memory allocation. It is plausible for a 192 GB box with a 163 GB database that should mostly live in cache, especially after lowering `work_mem`.
- `max_worker_processes=48` and `max_parallel_workers=48` are internally consistent; `max_parallel_workers` is not higher than the worker-process pool. `max_parallel_workers_per_gather=16` is aggressive but reasonable for an operator-controlled analytical box if `work_mem` is lowered.
- `maintenance_work_mem=16GB` is reasonable for the IVFFlat build itself. PostgreSQL treats `maintenance_work_mem` as the limit for the whole parallel utility command, not per worker. The caveat is its persistent interaction with autovacuum, covered above.
- `max_parallel_maintenance_workers=12` is useful for this operation. pgvector documents parallel IVFFlat index creation and recommends increasing `max_parallel_maintenance_workers` for large tables, with the leader process in addition to the workers.
- `random_page_cost=1.1`, `effective_io_concurrency=256`, `max_wal_size=16GB`, `min_wal_size=2GB`, and `checkpoint_completion_target=0.9` are reasonable for a heavily cached SSD/NVMe-backed dedicated corpus DB. None are data-risky.
- The IVFFlat `lists` heuristic is the pgvector guidance: `rows / 1000` up to 1M rows, `sqrt(rows)` above 1M. For 4.64M rows, the script's awk expression floors `sqrt(4640000)` to `2154`, which is the right integer target. The `LISTS >= 1` guard handles tiny corpora.
- `vector_l2_ops` matches the live jurisearch storage finalizer and manifest (`crates/jurisearch-storage/src/dense.rs:5`, `:151-175`) and the live dense query path uses `<->` L2 distance (`crates/jurisearch-storage/src/retrieval/sql.rs:97-104`, `:195-202`). For normalized bge-m3 vectors, L2 and cosine orderings are monotonic-equivalent, so keeping the existing L2 operator class is the lowest-risk choice. Switching to inner-product or cosine would require changing the query operator and manifest contract too.
- The ordering is broadly right: preconditions, compute lists, `ALTER SYSTEM`, restart for `shared_buffers` and worker-pool changes, wait for connectivity, rebuild index, analyze. `bash -n tune-pg18.sh` passed, and `shellcheck tune-pg18.sh` produced no diagnostics in this checkout.
- The heredoc-through-stdin pattern itself is sound: `su - postgres -c "psql ... -f -"` receives the heredoc on stdin. The problem is shell quoting for future overrides, not the stdin plumbing.
- Corpus data is not modified by the tuning script. The only database changes are persistent configuration and index/statistics maintenance.

## Sources Checked

- `tune-pg18.sh`
- `crates/jurisearch-storage/src/dense.rs`
- `crates/jurisearch-storage/src/retrieval/sql.rs`
- `crates/jurisearch-storage/src/retrieval/types.rs`
- PostgreSQL current documentation for `shared_buffers`, `work_mem`, `hash_mem_multiplier`, `maintenance_work_mem`, `autovacuum_work_mem`, parallel workers, `effective_cache_size`, `random_page_cost`, and `psql` `ON_ERROR_STOP`.
- pgvector README guidance for IVFFlat `lists`, `probes`, distance operators, and parallel index creation.

VERDICT: FIXES_REQUIRED
