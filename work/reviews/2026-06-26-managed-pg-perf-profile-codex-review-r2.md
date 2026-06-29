# Codex review r2 - managed Postgres perf profile

## Summary

The r2 split to `resolve_pg_setting(Option<&str>, ...)` fixes the prior env-mutating test-isolation issue, and the current syntax guards reject quote, newline, comment, and semicolon-shaped inputs before values are interpolated into the single-quoted `jurisearch.conf` entries. The worker-count validator matches PostgreSQL 18.4's `MAX_PARALLEL_WORKER_LIMIT` of 1024, and the explicit malformed decimal/exponent parser cases are rejected as intended. However, the memory validator still only enforces a lower bound, so large but syntactically valid memory literals can still pass validation and make PostgreSQL fail during startup/config parsing.

## Findings

### BLOCKER

None.

### WARN

- `crates/jurisearch-storage/src/runtime.rs:500` - `is_pg_mem_literal` accepts any parsed memory literal above 1 MiB, but every memory GUC written at `crates/jurisearch-storage/src/runtime.rs:546` through `crates/jurisearch-storage/src/runtime.rs:590` also has an upper bound in PostgreSQL. This means env overrides such as `JURISEARCH_PG_WORK_MEM=2TB`, `JURISEARCH_PG_SHARED_BUFFERS=8192GB`, or `JURISEARCH_PG_EFFECTIVE_CACHE_SIZE=16TB` pass the current validator, get written into `jurisearch.conf`, and then fail PostgreSQL config parsing. I verified against the local PostgreSQL 18.4 install: `postgres --describe-config` reports `work_mem` max `2147483647` kB, `shared_buffers` max `1073741823` 8kB blocks, and `effective_cache_size` max `2147483647` 8kB blocks; non-mutating `postgres -D /home/pierre/.pgrx/data-18 -C ... -c ...` probes reject `work_mem=2TB`, `shared_buffers=8192GB`, and `effective_cache_size=16TB` with FATAL range/integer errors. Recommended fix: make memory validation range-aware per GUC, preferably by passing min/max/native-unit metadata into the validator used by each env var. If a single shared validator is kept, cap accepted byte counts at the smallest safe upper bound for all exposed memory GUCs, no higher than PostgreSQL's `MAX_KILOBYTES * 1024` on this target. Add tests proving `2TB`, `3TB`, `8192GB`, and `16TB` fall back to defaults instead of reaching `jurisearch.conf`.

### NIT

- `crates/jurisearch-storage/src/runtime.rs:432` - The lower-bound comment says the 1 MiB floor is "the largest per-GUC minimum" and cites `maintenance_work_mem`'s floor as 1 MB, but PostgreSQL 18.4's source/sample config show lower actual minima for these exposed GUCs: `maintenance_work_mem` and `work_mem` 64kB, `temp_buffers` 100 blocks / 800kB, `shared_buffers` 16 blocks / 128kB, and `effective_cache_size` 1 block. The 1 MiB floor is still conservative and startup-safe for every exposed GUC, but the explanation is inaccurate. Recommended fix: reword the comment/test text to say 1 MiB is a deliberately conservative common floor above all exposed minimums, not PostgreSQL's largest exact minimum.
- `crates/jurisearch-storage/src/runtime.rs:504` - The integer helper comment says `parse::<u32>` rejects signs, but Rust accepts a leading `+` (`"+1".parse::<u32>() == Ok(1)`). That is not a startup breaker for PostgreSQL numeric GUCs, but it means the test/comment contract is stricter than the implementation. Recommended fix: either add an ASCII-digit check before parsing if signs should be rejected, or update the comment/tests to explicitly allow a leading plus.

## Verification

- `git diff --check -- crates/jurisearch-storage/src/runtime.rs` passed.
- `cargo test -p jurisearch-storage pg_setting` passed: 2 unit tests ran; the new tests do not cover the upper-bound cases above.
- PostgreSQL 18.4 `--describe-config` and `-C` probes were used for the GUC ranges described in the WARN finding.

VERDICT: FIXES_REQUIRED
