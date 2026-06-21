# Review — Storage Extension Smoke Runtime (commit `75a70bd`)

Date: 2026-06-21
Reviewer: Claude (Opus 4.8)
Scope: commit `75a70bd` "Add storage extension smoke runtime" — new `jurisearch-storage` crate (`src/runtime.rs`, `tests/extension_smoke.rs`).
Plan context: `IMPLEMENTATION_PLAN.md §3` task **0.3 Embedded Postgres spike**; `00-setup/PREREQUISITES.md §2` (the gating `pg_search`-on-embedded-Postgres unknown) and §16 checklist item "prove `CREATE EXTENSION vector` and `pg_search` in a throwaway data dir".
Constraint: review-only; no source files modified.

---

## Findings (most important first)

**F1 — The step does what it claims, and it is verified end-to-end.** The commit ports `00-setup/smoke-pg-extensions.sh` into a reusable Rust module (`ManagedPostgres` + `PgConfig`) plus an integration test. Forced to actually run (`JURISEARCH_REQUIRE_PG_EXTENSIONS=1`) on this machine it performs a real `initdb` → configure `shared_preload_libraries = 'pg_search'` → `pg_ctl start -w` → `CREATE EXTENSION vector; CREATE EXTENSION pg_search;` → pgvector nearest-neighbour query → `pg_ctl -m fast stop`, and passes in 1.10 s. This directly retires the §16 checklist item and is the concrete core of the §2 "scoped ABI/drop-in spike". The vector assertion (`'[1,0,0]'` → `responsabilite civile article 1240`) is a meaningful behavioural check, not just "did not error".

**F2 — Lifecycle and binding are correct and match the locked design.** Private binding is satisfied: `listen_addresses = '127.0.0.1'` on an ephemeral port obtained via `free_loopback_port()` (`runtime.rs:233-238`), plus a per-run unix socket dir — consistent with DESIGN §13.3 "no public exposure by default". Teardown is correct: `impl Drop for ManagedPostgres` (`runtime.rs:177-181`) calls `stop()`, and because the explicit `Drop` runs before fields are dropped (and `_tmp: TempDir` is the first field), Postgres is stopped *before* the data dir is removed. Verified empirically: after the run there were no leftover `/tmp/jurisearch-pg.*` dirs and no orphaned `postgres` processes.

**F3 — Asset preflight fails fast and actionably.** `start_temp` calls `require_extension_assets("pg_search")` and `("vector")` before touching `initdb` (`runtime.rs:99-100`), and `MissingExtensionAssets` names the extension, `pkglibdir`, and `extension_dir`. This mirrors the bash script's guard and means a missing build is reported clearly rather than as an obscure `CREATE EXTENSION` failure.

**F4 — Default skip makes this gating test report PASS (not SKIP) when the backend is absent.** `tests/extension_smoke.rs:5-25`: when `PgConfig::discover()` returns `MissingPgConfig`, or assets are missing, and `JURISEARCH_REQUIRE_PG_EXTENSIONS` is unset, the test prints `skipping …` to stderr and `return Ok(())` — i.e. a green pass. The escape hatch exists and is the right instinct for portability, but a runner without a built `pg_search` will color the 0.3-gating check green. This mildly conflicts with the project's stated principle (plan 0.2 acceptance) of not "treating unavailable … metrics as passed". Non-blocking here because the asset-owning machine (this one) has both extensions and runs the real path; see Recommended R1.

**F5 — `discover()` hard-codes the PG patch version, diverging from the shell scripts.** `runtime.rs:30` defaults to `~/.pgrx/18.4/pgrx-install/bin/pg_config`, a fixed `18.4`. By contrast `smoke-pg-extensions.sh:13-20` and `build-pg-search.sh:40-52` discover the version dynamically (`find … | grep -E "/(18|18[.][0-9]+)/" | sort -V | tail -1`). The Rust default will silently miss the binaries the moment pgrx moves to 18.5, while the shell path keeps working. Mitigated today by the `JURISEARCH_PG_CONFIG` / `PG_CONFIG` env overrides (`runtime.rs:23-28`) and by 18.4 being current. Robustness/consistency gap, not a correctness bug; see Recommended R2.

**F6 — `serde` derive on `PgConfig` is currently dead weight.** `runtime.rs:12` derives `Serialize, Deserialize` and `Cargo.toml` adds `serde`, but nothing in the crate or test serializes `PgConfig`. Harmless (it compiles, clippy-clean), but it is an unused dependency surface. Keep only if `PgConfig` is intended to be recorded into the manifest later; otherwise drop. See Recommended R3.

**F7 — Scope clarity, so the 0.3 gate is not prematurely closed.** This step covers only the *extension* slice of 0.3. It does **not** implement single-writer locking, crash-recovery/orphan reclaim, schema/migration mechanics, or the < 500 ms warm-query latency target — all of which 0.3 / PREREQUISITES §2 still require (the `gciauto2` reuse covers most of these and is not yet wired in here). This is expected for an incremental commit; flagged only so reviewers do not read a passing smoke as "0.3 complete".

---

## Required fixes

**None.** No defect blocks proceeding. The crate builds, the smoke test exercises the real backend and passes, clippy and fmt are clean, and teardown is leak-free. F4–F6 are hardening items, not blockers, and F7 is scope clarification rather than a defect.

---

## Recommended (non-blocking) improvements

- **R1 (F4):** Have the CI/owner machine that defends the 0.3 gate set `JURISEARCH_REQUIRE_PG_EXTENSIONS=1` so an absent/broken `pg_search` build turns the gate red, not green. Optionally surface the skip as a clearly distinct signal (e.g. log a structured `SKIPPED` marker) rather than a silent `ok`.
- **R2 (F5):** Make `discover()`'s default fallback glob `~/.pgrx/*/pgrx-install/bin/pg_config` (newest by version) to match the shell scripts, instead of pinning `18.4` — keeps Rust and bash paths from drifting on the next pgrx patch bump.
- **R3 (F6):** Drop the `serde` derive + dependency unless `PgConfig` is about to be persisted into the manifest; if it is, add a round-trip test so the derive is actually covered.
- **R4 (F7):** In the next 0.3 commit, note in the plan/runbook which 0.3 acceptance criteria remain (single-writer lock, crash recovery, migrations, latency) so this smoke is not mistaken for the full spike.

---

## Verification

All commands run from `/home/pierre/Work/jurisearch`; no source files modified.

| Check | Command | Result |
|---|---|---|
| Crate builds | `cargo build -p jurisearch-storage` | ✅ Finished, no errors |
| Workspace builds | `cargo build --workspace` | ✅ Finished, no errors |
| Lint | `cargo clippy -p jurisearch-storage --all-targets` | ✅ Clean, no warnings |
| Format | `cargo fmt -p jurisearch-storage --check` | ✅ Clean |
| Extension assets present | inspected pgrx prefix | ✅ `pg_search.{so,control}` and `vector.{so,control}` present under PG 18.4 prefix |
| Smoke test (forced real path) | `JURISEARCH_REQUIRE_PG_EXTENSIONS=1 cargo test -p jurisearch-storage --test extension_smoke -- --nocapture` | ✅ `1 passed; 0 failed` in 1.10 s — real `initdb`/`pg_ctl`/`CREATE EXTENSION`/vector NN query/clean stop |
| Teardown is leak-free | `ls /tmp/jurisearch-pg.*`; `ps aux \| grep [p]ostgres` | ✅ No leftover temp dirs; no orphaned postgres processes |

Environment confirmed: `pg_config --version` → PostgreSQL 18.4 at the discover() default path `~/.pgrx/18.4/pgrx-install/bin/pg_config`, so the default code path is the one exercised.

---

VERDICT: GO
