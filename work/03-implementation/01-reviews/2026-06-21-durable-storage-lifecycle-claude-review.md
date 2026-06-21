# Review — Durable Storage Lifecycle (commit `b541fa1`)

Date: 2026-06-21
Reviewer: Claude (Opus 4.8, 1M context)
Scope: local commit `b541fa1` "Add durable storage lifecycle" — `crates/jurisearch-storage/src/runtime.rs` (+`ManagedPostgres::start_durable`, locks, reclaim, bootstrap helpers), new `crates/jurisearch-storage/tests/durable_lifecycle.rs`, workspace `Cargo.toml`/crate `Cargo.toml` (`fs2`, `postgres`), and plan notes in `IMPLEMENTATION_PLAN.md` / `00-setup/PREREQUISITES.md`.
Plan context: `IMPLEMENTATION_PLAN.md §0.3 Embedded Postgres Spike`; `DESIGN §13.3` lifecycle/concurrency/binding acceptance criteria.
Constraint: review-only; no source files modified.

---

## Findings (most important first)

**F1 — The slice does what it claims and is verified end-to-end on the real backend.** `start_durable` (`runtime.rs:157-217`) performs the full durable path: file-lock the index root *before* touching PGDATA (`:166`), create the persistent `pg/{data,sock}` tree, `initdb` only when `PG_VERSION` is absent (`:176`), write runtime config, conservatively reclaim a stale postmaster, `pg_ctl start -w`, bootstrap the `jurisearch` app database, `CREATE EXTENSION vector/pg_search`, then take a session advisory lock keyed to the data dir. The integration test (`tests/durable_lifecycle.rs`) forced to run for real (`JURISEARCH_REQUIRE_PG_EXTENSIONS=1`) passes in 1.38 s: it creates a `docs` table with a `vector(3)` column in one session, drops the handle, restarts against the same root, and asserts the row survives and the nearest-neighbour query returns `responsabilite civile article 1240` (`:63-67`). Persistent-PGDATA restart and durability are genuinely demonstrated, not just "did not error".

**F2 — Single-writer locking is correct and the rejection path is proven.** `StartupLock::acquire` (`runtime.rs:252-272`) opens `<index>/jurisearch-storage.lock` and `try_lock_exclusive()` (`fs2`/`flock`), mapping `WouldBlock` to the actionable `StorageLockBusy { path }` (`:266-268`); the lock is held for process lifetime via `_startup_lock` and released on `Drop` (`:274-278`). Because each `start_durable` opens its own file description, a second in-process owner conflicts as expected — the test asserts exactly `Err(StorageLockBusy { .. })` while the first owner is live (`durable_lifecycle.rs:56-57`). The lock is taken before any PGDATA mutation, so a loser never races `initdb`/`pg_ctl`. This satisfies `DESIGN §13.3` "single-writer locking across simultaneous processes against one index".

**F3 — Orphan reclaim is appropriately conservative.** `reclaim_data_dir` (`runtime.rs:501-514`) runs only under the held startup lock, returns immediately when no `postmaster.pid` exists (the normal clean-restart case, since `stop()` removes the pidfile), otherwise issues a best-effort `pg_ctl stop -m fast -t 20` and removes the pidfile **only** when `postmaster_alive` confirms the PID is dead via `/proc/<pid>/cmdline` containing `postgres` on Linux (`:516-539`). A live foreign/own postmaster is never clobbered — at worst `pg_ctl start` later refuses, which is the safe failure. Off-Linux it assumes alive and declines removal (`:534-538`), erring safe.

**F4 — Binding, privacy, and clean shutdown match the locked design.** `write_runtime_conf` (`runtime.rs:418-443`) idempotently appends `include_if_exists = 'jurisearch.conf'` to `postgresql.conf` and rewrites `jurisearch.conf` each start with `listen_addresses = '127.0.0.1'`, a fresh ephemeral `port`, a private `unix_socket_directories`, and `shared_preload_libraries = 'pg_search'`. `ensure_private_data_dir` chmods the data dir `0700` (`:541-548`). `Drop for ManagedPostgres` calls `stop()` → `pg_ctl -m fast stop` (`:228-245,313-317`). Verified empirically: after two real test runs there are **no** `/tmp/jurisearch-durable-pg.*` leftovers and **no** jurisearch postgres orphans (the only live servers are the unrelated system `/var/lib/pgsql/data` and the `gciauto2` reference app). Matches `DESIGN §13.3` "no public exposure by default" and "clean shutdown… no orphaned processes / stale locks".

**F5 — Bootstrap helpers are idempotent and injection-safe.** `ensure_database` (`runtime.rs:396-416`) checks `pg_database` then `CREATE DATABASE` only if missing; `CREATE EXTENSION IF NOT EXISTS` runs every start (`:205-207`); the config include is appended only once (`:423-426`). Identifiers/literals are escaped through `sql_identifier`/`sql_string_literal` (`runtime.rs:563-569`) with unit coverage for embedded `"` and `'` (`:644-649`). The advisory key is a stable FNV-1a hash of the canonicalized data-dir path with collision/stability unit coverage (`:550-557,632-642`).

**F6 — Quality bar is clean.** `cargo build --workspace`, `cargo clippy -p jurisearch-storage --all-targets`, and `cargo fmt --check` are all warning-free; the 3 storage unit tests pass; the pre-existing temp `extension_smoke` still passes (1.09 s), so the refactor of `execute_sql` → shared `psql(...)` helper (`:337-369`) did not regress the disposable path. The working tree is clean and `target/` is gitignored.

**F7 — The advisory-lock busy branch and orphan-reclaim are shipped untested (coverage gap, not a defect).** The concurrent-owner test trips the **file** lock (`StorageLockBusy`) and therefore never reaches `DataDirLock::acquire`'s failure branch (`runtime.rs:294-301`); `AdvisoryLockBusy` and `postmaster_alive`/`reclaim_data_dir` have no exercised path. For the same-index-root case the advisory lock is genuinely redundant with the file lock (it only adds protection when two distinct index roots canonicalize onto one data dir, e.g. via symlink), so this is defensible defense-in-depth — but the crash-recovery story (`DESIGN §13.3` "crash recovery") rests on `reclaim_data_dir`, which no test drives. The plan already lists crash recovery as remaining work, so this is scope-honest; see R2/R3.

**F8 — Advisory unlock in `Drop` is a post-shutdown no-op (benign ordering quirk).** Fields drop in declaration order *after* the explicit `Drop` runs `stop()`, so by the time `DataDirLock::drop` issues `SELECT pg_advisory_unlock($1)` (`runtime.rs:305-311`) the server is already down and the connection dead; the result is ignored. Harmless — a session advisory lock is released when the instance stops / connection closes, so nothing leaks — but the explicit unlock never actually executes on the happy path. Noted for clarity, not a fix.

**F9 — Scope: this is the lifecycle slice, not all of 0.3.** Correctly reflected in `IMPLEMENTATION_PLAN.md:275` and `PREREQUISITES.md`. Still open per the plan and `DESIGN §13.3`: bundled/downloaded binary policy + offline-install story, schema/extension **migration** mechanics, platform policy, performance tuning (the minimal `jurisearch.conf` sets no `shared_buffers`/`wal`/`fsync`, unlike the `gciauto2` reference), and the warm `< 500 ms` latency check. Flagged only so a green smoke is not read as "0.3 complete".

---

## Required fixes

**None.** No defect blocks proceeding. The slice builds clean, lints clean, the durable restart + concurrent-owner test exercises the real backend and passes, teardown is leak-free, and the plan notes accurately scope what remains. F7–F9 are coverage/scope items, not correctness blockers.

---

## Recommendations (non-blocking)

- **R1 (F7/F8) — Cover the advisory lock directly.** Add a unit/targeted test that calls `DataDirLock::acquire` twice against one running instance with the same key to prove the `AdvisoryLockBusy` branch, since the integration test can never reach it through the file lock. Cheap insurance against future regressions in that branch.
- **R2 (F7) — Add a crash-recovery test before closing 0.3.** Simulate a hard kill (leave a stale `postmaster.pid` with a dead PID) and assert `start_durable` reclaims and restarts; assert it *refuses* when the pidfile names a live `postgres`. This is the one `DESIGN §13.3` lifecycle criterion (crash recovery) whose code path currently has zero coverage.
- **R3 (F9) — When the migration/latency slices land, record which §13.3 criteria remain** (binary acquisition/offline install, schema/extension migrations, platform policy, `< 500 ms` warm latency, tuning) so this lifecycle commit is not mistaken for the full spike. The plan already does this for the current slice — keep that discipline.
- **R4 (F4) — Consider tightening to unix-socket + peer auth.** `--auth=trust` over an ephemeral 127.0.0.1 port lets any local user connect as superuser for the instance's lifetime. It is within the locked design (loopback is permitted) and matches the temp path, but a unix-socket-only binding with peer auth would close the local-trust window for the durable, longer-lived instance.
- **R5 (F8) — Optionally drop the redundant explicit `pg_advisory_unlock`** (or reorder so it runs before `stop()`), since on the happy path it executes against a dead connection. Purely cosmetic.

---

## Verification

All commands run from `/home/pierre/Work/jurisearch`; no source files modified.

| Check | Command | Result |
|---|---|---|
| Workspace builds | `cargo build --workspace` | ✅ Finished, no errors |
| Lint | `cargo clippy -p jurisearch-storage --all-targets` | ✅ Clean, no warnings |
| Format | `cargo fmt -p jurisearch-storage --check` | ✅ Clean (exit 0) |
| Unit tests | `cargo test -p jurisearch-storage --lib` | ✅ 3 passed (lock-key stability, version-key order, SQL quoting) |
| Durable lifecycle (forced real path) | `JURISEARCH_REQUIRE_PG_EXTENSIONS=1 cargo test -p jurisearch-storage --test durable_lifecycle -- --nocapture` | ✅ 1 passed in 1.38 s — persistent PGDATA restart, row survives restart, `StorageLockBusy` on concurrent owner, vector NN query correct |
| Temp smoke (no regression) | `JURISEARCH_REQUIRE_PG_EXTENSIONS=1 cargo test -p jurisearch-storage --test extension_smoke -- --nocapture` | ✅ 1 passed in 1.09 s |
| Teardown leak-free | `ls /tmp/jurisearch-durable-pg.*`; `ls /tmp/jurisearch-pg.*`; real-`postgres` server scan | ✅ No leftover dirs; no jurisearch postgres orphans (only unrelated system + `gciauto2` servers live) |
| Tree clean | `git status --short` | ✅ Empty; `target/` gitignored |

Environment: `pg_search.{so,control}` and `vector.{so,control}` present under the pgrx-managed PG 18 prefix, so the forced real path is the one exercised.

---

VERDICT: GO
