# Review: managed Postgres perf profile r3

## Summary

The r3 diff makes the managed PostgreSQL profile conservative by default and env-tunable through private validators before values are written to `jurisearch.conf`. The r2 fixes are present and complete: memory overrides are now bounded on both sides, the floor comment no longer claims to be the largest per-GUC minimum, and worker overrides reject signed strings such as `+1` before parsing.

I reviewed the scoped working-tree diff in `crates/jurisearch-storage/src/runtime.rs`, the helper call path around `write_runtime_conf`, and the PostgreSQL 18.4 GUC definitions/parser. For the local managed target (`/home/pierre/.pgrx/18.4/pgrx-install/bin/pg_config --version` reports PostgreSQL 18.4 on a 64-bit host), I did not find an override string that passes validation but would break startup through malformed syntax, injection shape, too-small values, too-large memory values, or out-of-range worker counts.

PostgreSQL 18.4 source checks used:

- `src/backend/utils/misc/guc_tables.c` at tag `REL_18_4`: `work_mem` and `maintenance_work_mem` use `GUC_UNIT_KB`, min `64`, max `MAX_KILOBYTES`; `shared_buffers` uses `GUC_UNIT_BLOCKS`, min `16`, max `INT_MAX / 2`; `temp_buffers` uses `GUC_UNIT_BLOCKS`, min `100`, max `INT_MAX / 2`; `effective_cache_size` uses `GUC_UNIT_BLOCKS`, min `1`, max `INT_MAX`; the three overridden parallel-worker GUCs all max at `MAX_PARALLEL_WORKER_LIMIT`.
- `src/include/utils/guc.h` at tag `REL_18_4`: `MAX_KILOBYTES` is `INT_MAX` on builds where `SIZEOF_SIZE_T > 4`.
- `src/include/postmaster/bgworker_internals.h` at tag `REL_18_4`: `MAX_PARALLEL_WORKER_LIMIT` is `1024`.
- `src/backend/utils/misc/guc.c` at tag `REL_18_4`: integer GUCs parse numeric values plus recognized units, convert to the GUC base unit, round, and then enforce each GUC's min/max.

The `MAX_PG_MEM_BYTES = INT_MAX * 1024` ceiling is the tight bound for the overridden kB-based GUCs on this 64-bit PostgreSQL 18.4 build. With the default 8 kB block size, the block-based GUC maxima are larger: `shared_buffers`/`temp_buffers` allow roughly `INT_MAX / 2 * 8192` bytes, and `effective_cache_size` allows roughly `INT_MAX * 8192` bytes. The boundary tests are exact: `2047GB` is `2,146,435,072 kB`, below `INT_MAX`; `2048GB` and `2TB` are `2,147,483,648 kB`, one kB above `INT_MAX`, so rejection is correct.

`is_pg_int_literal` now matches the intended contract for the overridden worker GUCs: only ASCII digits are accepted, `parse::<u32>` rejects overflow, and the explicit `<= 1024` bound matches PostgreSQL 18.4 for `max_parallel_workers_per_gather`, `max_parallel_workers`, and `max_parallel_maintenance_workers`. The tests cover the corrected `+1` rejection, the `1024`/`1025` boundary, and overflow fallback.

`git diff --check -- crates/jurisearch-storage/src/runtime.rs` is clean. I did not run Cargo tests because that would create or update build artifacts under the repository, and the review request only authorized writing this review file.

## Findings

### BLOCKER

None.

### WARN

None.

### NIT

None.

VERDICT: GO
