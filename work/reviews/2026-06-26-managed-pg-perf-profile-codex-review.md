## Summary

The change moves the managed Postgres runtime profile from fixed aggressive memory/parallel defaults to conservative defaults, while adding `JURISEARCH_PG_*` environment overrides for the common memory/parallel knobs and `shared_buffers`. Override strings are trimmed, syntactically validated, and then interpolated into the included `jurisearch.conf`; bulk ingest keeps its WAL/checkpoint relaxations and gets a larger default `shared_buffers` than the durable profile.

Validation performed:

- Reviewed the uncommitted diff for `crates/jurisearch-storage/src/runtime.rs`.
- Checked the runtime call surface: durable startup uses `PostgresRuntimeProfile::Durable`; CLI bulk ingest uses `PostgresRuntimeProfile::BulkIngest`.
- Ran `cargo test -p jurisearch-storage pg_` and `cargo test -p jurisearch-storage --lib runtime::tests::`; both passed.
- Parsed generated-style durable and bulk `jurisearch.conf` contents with the local pgrx Postgres 18.4 install; both default profiles were accepted.
- Probed Postgres memory/integer GUC parsing for validator edge cases (`1.5GB`, `65536B`, `0MB`, and oversized numeric strings).

## BLOCKER

None.

## WARN

### `is_pg_mem_literal` / `is_pg_int_literal` do not match the "valid override or fallback" contract

`pg_runtime_setting` only falls back when the validator rejects the env value, but the current validators are a narrow syntax check rather than a Postgres-compatible validity check. That creates two boundary problems in the override path:

- `is_pg_mem_literal` rejects memory values that Postgres accepts, such as fractional sizes (`1.5GB`) and byte-unit values (`65536B`).
- Both validators accept values that Postgres rejects and that will be written into `jurisearch.conf`, such as `0MB` for memory GUCs or an oversized digit string for `max_parallel_workers`.

I verified those cases against the local Postgres parser: `work_mem = '1.5GB'` and `work_mem = '65536B'` parse, while `work_mem = '0MB'` and `max_parallel_workers = '999999999999999999999999999999999999999'` make the config invalid. The latter cases are the safety issue: malformed/hostile env input can still break managed Postgres startup instead of falling back to the conservative default.

Recommended fix: replace the generic digit-only validators with type/range-aware validators for the actual GUCs being overridden. For memory values, parse Postgres's accepted numeric/unit form (`B`, `kB`, `MB`, `GB`, `TB`, including fractional numeric values where Postgres accepts them), reject overflow, and reject values below the minimum for the target setting. For integer values, parse to a bounded integer type and reject overflow/out-of-range values. Add tests proving accepted Postgres literals like `1.5GB`/`65536B` are accepted and known-bad startup breakers like `0MB`, too-small `temp_buffers`, and oversized integers are rejected before writing `jurisearch.conf`.

## NIT

### The env-mutating unit test is not isolated from Rust's parallel test runner

`pg_runtime_setting_prefers_valid_override_else_default` mutates process-wide environment variables with `std::env::set_var`/`remove_var`, but it does not take a shared env lock or restore a prior external value. The variable name is test-specific, so this is unlikely to affect production behavior, but Rust's environment is process-global and other storage tests read environment variables while the test harness can run tests concurrently.

Recommended fix: split `pg_runtime_setting` so the validation/fallback logic can be tested with an `Option<&str>` input without mutating the process environment, or introduce a shared env guard that serializes environment mutation and restores the previous value.

VERDICT: FIXES_REQUIRED
