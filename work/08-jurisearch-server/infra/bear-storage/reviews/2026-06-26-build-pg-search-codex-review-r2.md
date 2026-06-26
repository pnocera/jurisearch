# Code Review: build-pg-search.sh r2

## R1 Findings

1. RESOLVED - `cargo pgrx init "--pg${PGVER}=${PG_CONFIG}"` now runs after the pinned `cargo-pgrx` install and before `cargo pgrx install`; local `cargo-pgrx 0.18.1` help and source confirm `--pg17=<path>` is accepted, records an existing `pg_config`, and `install` still calls `Pgrx::from_config()`.
2. RESOLVED - The smoke now runs `psql -v ON_ERROR_STOP=1 -d ext_smoke -f /tmp/ext_smoke.sql`, so a failed `CREATE EXTENSION pg_search` returns nonzero and trips the script's ERR trap instead of reaching `PGSEARCH-DONE`.
3. RESOLVED - The `shared_preload_libraries` merge preserves an existing value, appends `pg_search` only when absent, sets `pg_search` when empty, and is idempotent when `pg_search` is already present.
4. RESOLVED - The `cargo-pgrx` version check now uses `grep -qx "cargo-pgrx $PGRX_VERSION"`, so `cargo-pgrx 0.18.10` no longer satisfies a `0.18.1` pin.

## New Issues

None found.

## Non-Regression Checks

- The build command still uses `cargo pgrx install --package pg_search --release --no-default-features --features pg17,deferred_wal --pg-config "$PG_CONFIG"`, which remains the right PG17 feature selection for the stated fork facts.
- `cargo-pgrx 0.18.1` source confirms `install` copies the control file and generated SQL to `pg_config.extension_dir()` and the shared library to `pg_config.pkglibdir()`, so the install-into-system-PG mechanics remain correct when run as root.
- The pgrx version extraction still yields `0.18.1` for the stated `pgrx = "=0.18.1"` line.
- The preload, restart, readiness loop, then `CREATE EXTENSION` ordering remains correct for `pg_search`; `pgvector` still does not need preload.
- `bash -n /home/pierre/bear-storage/build-pg-search.sh` passes, and `shellcheck /home/pierre/bear-storage/build-pg-search.sh` produced no findings in this environment.

VERDICT: GO
