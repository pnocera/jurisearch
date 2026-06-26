# Codex review — build-pg-search.sh

## Scope
`/home/pierre/bear-storage/build-pg-search.sh`

Runs INSIDE a Debian 13 LXC (CT 110) as root. It builds + installs ParadeDB `pg_search` for the
**system** PostgreSQL 17, from the local fork `pnocera/paradedb` (cloned at /root/paradedb, tag 0.24.1),
via cargo-pgrx, then enables it and verifies `CREATE EXTENSION`. The build is long (~20-40 min), so this
is a pre-execution gate: correctness of the build incantation matters most (a wrong flag wastes 30 min).

## Ground truth (verified 2026-06-26 inside CT 110)
- Debian 13 trixie; `postgresql-17` (17.10) + `postgresql-17-pgvector` (0.8.0) + `postgresql-server-dev-17`
  installed; cluster `17/main` online, UTF8 / C.UTF-8, data on a 1 TB mount.
- Rust 1.96.0 via rustup at /root/.cargo (matches the fork's rust-toolchain.toml channel = "1.96.0").
- Build toolchain present: build-essential, clang 19, libclang-dev, pkg-config, libssl-dev, git, curl.
- Fork facts (read from the clone):
  - root `Cargo.toml`: `pgrx = "=0.18.1"` (and pgrx-tests "=0.18.1").
  - `pg_search/Cargo.toml`: crate name `pg_search`, `crate-type=["cdylib"]`,
    `[features] default = ["pg18","deferred_wal"]`, `pg17 = ["pgrx/pg17","pgrx-tests/pg17"]`,
    `deferred_wal = []`.
  - Repo `Makefile`: `cargo pgrx install --package pg_search --release --pg-config "$(PG_CONFIG)"`
    (defaults to pg18 features). Our script overrides to PG17 with
    `--no-default-features --features pg17,deferred_wal`.
- system PG17 pg_config: `/usr/lib/postgresql/17/bin/pg_config`.

## What to verify (contract correctness, not style)
1. **The build command.** Is
   `cargo pgrx install --package pg_search --release --no-default-features --features pg17,deferred_wal
    --pg-config /usr/lib/postgresql/17/bin/pg_config`
   the correct way to build pg_search for a *system* PG17 with cargo-pgrx 0.18.x? Specifically:
   - Does `cargo pgrx install` work against a system Postgres given ONLY `--pg-config`, or does pgrx
     0.18 first require `cargo pgrx init --pg17 <pg_config>` to register that pg_config? If init is
     required, the script will fail at step 3 — call that out as a BLOCKER with the exact fix.
   - Is `--no-default-features --features pg17,deferred_wal` the right feature set (default is
     pg18+deferred_wal; we must drop pg18 and keep deferred_wal)? Is `deferred_wal` needed, or
     harmless to include?
   - Will `cargo pgrx install` (not `package`) place the `.so`/`.control`/`.sql` into the system PG17
     pkglibdir/sharedir, which are root-writable (we run as root)?
2. **shared_preload_libraries.** pg_search requires `shared_preload_libraries='pg_search'` and a
   restart BEFORE `CREATE EXTENSION pg_search`. The script does `ALTER SYSTEM SET ...` then
   `pg_ctlcluster 17 main restart` then verifies, then `CREATE EXTENSION`. Confirm this ordering is
   correct and that ALTER SYSTEM (postgresql.auto.conf) is the right idempotent mechanism. Does
   pgvector also need preload (it should NOT)?
3. **pgrx version extraction.** `grep -m1 -E '^pgrx[[:space:]]*=' Cargo.toml | sed -E 's/.*"=?([0-9][0-9.]*)".*/\1/'`
   against `pgrx = "=0.18.1"` should yield `0.18.1`. Confirm.
4. **Fail-closed + psql/su.** `set -Eeuo pipefail` + ERR trap + PGSEARCH-FAILED sentinel; the
   `su - postgres -c "psql ..."` calls and the temp-file SQL approach for the smoke. Any quoting or
   stdin pitfalls? Is the restart-readiness loop adequate?
5. **Anything that would silently produce a broken/incompatible pg_search** (e.g. ABI mismatch,
   building for the wrong PG major, missing feature) -> BLOCKER.

## Output
For each finding: severity (BLOCKER / WARN / NIT), exact location, the problem, and a concrete fix —
for every severity. Note what you checked and found correct. End with a final line that is exactly
`VERDICT: GO` or `VERDICT: FIXES_REQUIRED`.
