# Code Review: build-pg-search.sh

## Findings

### BLOCKER - `cargo pgrx install` can fail before the long build because pgrx has not been initialized

Location: `/home/pierre/bear-storage/build-pg-search.sh:46-58`

Problem: The script installs the pinned `cargo-pgrx` and then invokes:

```bash
cargo pgrx install --package pg_search --release \
  --no-default-features --features "$EXT_FEATURES" \
  --pg-config "$PG_CONFIG"
```

For `cargo-pgrx 0.18.1`, `--pg-config` is not enough by itself. The `install` command constructs a `PgConfig` from `--pg-config`, but it still calls `Pgrx::from_config()?` before building so it can normalize feature flags. `Pgrx::from_config()` requires either `PGRX_PG_CONFIG_PATH` or `$HOME/.pgrx/config.toml`; if root has not run `cargo pgrx init`, it errors with the "Have you run `cargo pgrx init` yet?" path. That means this script can fail at step 3 immediately, before spending the 20-40 minute build.

Concrete fix: initialize pgrx for the system PG17 `pg_config` after installing/verifying `cargo-pgrx` and before `cargo pgrx install`, matching the upstream Makefile's `pgrx-init` target:

```bash
cargo pgrx init "--pg${PGVER}=${PG_CONFIG}"
```

Alternatively, export `PGRX_PG_CONFIG_PATH="$PG_CONFIG"` before invoking `cargo pgrx install`, but the durable fix is to register the system `pg_config` with `cargo pgrx init`.

### BLOCKER - Smoke test can report success even if `CREATE EXTENSION pg_search` fails

Location: `/home/pierre/bear-storage/build-pg-search.sh:76-85`

Problem: The smoke test runs a multi-statement SQL file with:

```bash
su - postgres -c "psql -d ext_smoke -f /tmp/ext_smoke.sql"
```

`psql -f` does not fail closed on SQL errors unless `ON_ERROR_STOP` is enabled. If `CREATE EXTENSION pg_search;` fails but the final `SELECT` still runs, the script can continue to `PGSEARCH-DONE` without actually verifying `pg_search`. This is exactly the kind of silent broken install the gate is meant to prevent.

Concrete fix: enable fail-fast SQL execution for the file, and preferably for all `psql` calls:

```bash
su - postgres -c "psql -v ON_ERROR_STOP=1 -d ext_smoke -f /tmp/ext_smoke.sql"
```

or add this as the first line of the smoke file:

```sql
\set ON_ERROR_STOP on
```

Keep the final extension-version `SELECT`, but also rely on `CREATE EXTENSION` itself failing the script.

### WARN - Existing `shared_preload_libraries` entries are overwritten

Location: `/home/pierre/bear-storage/build-pg-search.sh:63`

Problem: The ordering is correct: `ALTER SYSTEM SET shared_preload_libraries = 'pg_search'`, restart, readiness check, then `CREATE EXTENSION pg_search`. `pg_search` requires preload; `pgvector` does not. However, this line replaces the entire `shared_preload_libraries` value with only `pg_search`. In a dedicated fresh CT that may be fine, but it is not generally idempotent if the cluster already preloads something else such as `pg_stat_statements`.

Concrete fix: read the existing value, append `pg_search` only if missing, and write the merged comma-separated list with `ALTER SYSTEM SET`. If CT 110 is guaranteed to have no other preload libraries, document that assumption in the script near this line.

### WARN - The `cargo-pgrx` version check is a prefix match, not an exact match

Location: `/home/pierre/bear-storage/build-pg-search.sh:47`

Problem: `grep -q "cargo-pgrx $PGRX_VERSION"` will accept versions whose text starts with the pinned version, such as `cargo-pgrx 0.18.10` when `$PGRX_VERSION` is `0.18.1`. That defeats the "pinned exact version" check and could run a mismatched `cargo-pgrx` against the `pgrx = "=0.18.1"` crate.

Concrete fix:

```bash
if ! cargo pgrx --version 2>/dev/null | grep -qx "cargo-pgrx ${PGRX_VERSION}"; then
  ...
fi
```

## Checked And Found Correct

- Build feature selection is correct for the stated fork facts. The upstream `pg_search/Cargo.toml` default is `["pg18", "deferred_wal"]`; using `--no-default-features --features pg17,deferred_wal` drops `pg18`, enables the PG17 pgrx bindings, and keeps the upstream default `deferred_wal` behavior.
- `deferred_wal` is not an accidental extra feature. It is part of the upstream default feature set and is used by the index build path to defer WAL handling; keeping it with PG17 is intentional.
- `cargo pgrx install`, once pgrx config/env is available, installs into the target system PostgreSQL selected by `--pg-config`: cargo-pgrx 0.18.1 copies the control file and generated SQL to `pg_config.extension_dir()` and the shared library to `pg_config.pkglibdir()`. Running as root is sufficient for the system directories.
- The pinned pgrx extraction command returns `0.18.1` for `pgrx = "=0.18.1"`.
- The preload ordering is correct: set `shared_preload_libraries`, restart the PG17 cluster, wait for readiness, then run `CREATE EXTENSION pg_search`.
- `pgvector` does not require `shared_preload_libraries`; creating `vector` in the smoke database before `pg_search` is fine.
- The nested `su - postgres -c "psql ..."` quoting is acceptable for the current SQL strings. The temp-file approach avoids stdin being consumed through `su`; the main fail-closed gap is the missing `ON_ERROR_STOP`.

VERDICT: FIXES_REQUIRED
