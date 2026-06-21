# Review — Storage Schema Migrations (commit `3331482`)

Date: 2026-06-21
Reviewer: Claude (Opus 4.8, 1M context)
Scope: local commit `3331482` "Add storage schema migrations" — new `crates/jurisearch-storage/src/migrations.rs` (runner + migration v1), `crates/jurisearch-storage/src/runtime.rs` (startup wiring at `:213`, `pub(crate)` SQL helpers at `:562/:566`, removal of the standalone `CREATE EXTENSION` call), new `crates/jurisearch-storage/tests/schema_migrations.rs`, `src/lib.rs` module export, and plan status notes in `IMPLEMENTATION_PLAN.md`.
Plan context: `IMPLEMENTATION_PLAN.md §0.3 Embedded Postgres Spike` ("Create minimal documents/chunks/vector schema"; "Record index/schema/extension migration mechanics"); `DESIGN §13.3` "Upgrades: index / schema / extension migration story" and `§13.5` manifest schema version.
Constraint: review-only; no source files modified.

---

## Findings (most important first)

**F1 — The slice does what it claims and is verified end-to-end on the real backend.** `migrations.rs` adds a versioned runner (`run_migrations`, `:98-133`) and migration v1 (`:17-95`) that creates `vector`+`pg_search`, the canonical `documents`/`chunks`/`chunk_embeddings`/`graph_edges`/`index_manifest` tables, six supporting indexes, and seeds `index_manifest` with `schema_version = 1`. `start_durable` runs it once the storage locks are held (`runtime.rs:213`). The integration test forced to run for real (`JURISEARCH_REQUIRE_PG_EXTENSIONS=1`) passes in 1.41 s: it boots a durable instance, asserts `schema_migrations` records `1:canonical_documents_chunks_vectors` (`schema_migrations.rs:60-65`), inserts a real article/chunk/`vector(1024)` embedding (`:67-90`), drops the handle, **restarts against the same root**, asserts `count(*) FROM schema_migrations = 1`, runs a `<->` nearest-neighbour join, and asserts the manifest `schema_version` round-trips (`:95-108`). Persistence + migration application are genuinely demonstrated, not just "did not error".

**F2 — Migration v1 installs atomically, under the single-writer locks.** `run_migrations` is called only from `start_durable` (`runtime.rs:213`), *after* the process-lifetime `StartupLock` (file lock, `:166`) and the `DataDirLock` advisory lock (`:209`) are held — so exactly one writer ever runs migrations against an index root; there is no concurrent-migration race. The v1 body is a single multi-statement string sent through `psql -c` with `ON_ERROR_STOP=1` (`runtime.rs:343-358`), which Postgres wraps in one implicit transaction: the whole DDL set commits or rolls back together, and a mid-migration failure leaves no half-built schema. Empirically confirmed — `CREATE EXTENSION vector`/`pg_search` and all `CREATE TABLE`s execute inside that one transaction without error (test passes).

**F3 — Idempotency is real and proven, not assumed.** Three independent layers: the runner skips any version already present in `schema_migrations` (`migrations.rs:114-117`); every DDL statement is `IF NOT EXISTS` (`:24,43,57,66,76,82-87`); and the manifest seed is `INSERT … ON CONFLICT (key) DO UPDATE` (`:89-93`). The test exercises this directly — a second `start_durable` against the populated root re-enters `run_migrations`, and the assertion `count(*) = 1` (`schema_migrations.rs:96-99`) proves no duplicate application and no duplicate `schema_migrations` row. Because `CREATE EXTENSION` now lives in the once-only migration rather than on every start (the removed `runtime.rs` lines), extensions are created exactly once and persist in the catalog across restarts — a strict improvement over re-issuing them each boot.

**F4 — Bookkeeping is injection-safe and the version-parse path is robust.** The `schema_migrations` insert escapes the migration name through `sql_string_literal` and the table through `sql_identifier` (`migrations.rs:119-124`), with the version interpolated as a plain `i32` — no untrusted string reaches SQL unescaped. Promoting those two helpers to `pub(crate)` (`runtime.rs:562,566`) is the minimal visibility change needed and keeps their existing unit coverage (`runtime.rs:672-677`). Version discovery (`:107-112`) tolerates the empty result set (empty psql output → `lines()` yields nothing → `applied = []`) and silently drops unparseable lines, so a fresh DB and a populated DB both behave correctly.

**F5 — The schema matches the locked design vocabulary.** The five tables map cleanly onto `DESIGN §13.3` ("documents, chunks, metadata, temporal columns, graph edges, manifest") and `§13.5` (manifest carrying the schema version). Temporal modelling is faithful to `§0.5`/`§13.4`: `valid_from`/`valid_to` plus a preserved `valid_to_raw` sentinel (`migrations.rs:33-35`), and a `documents_validity_idx` on `(valid_from, valid_to)` for the planned `as-of` prefilter. Provenance/idempotency hooks (`source_payload_hash`, `canonical_json`, `chunk_builder_version`, `embedding_fingerprint`), `ON DELETE CASCADE` foreign keys, the `kind IN ('article','decision')` check, and `UNIQUE (document_id, chunk_index)` are all sensible and forward-looking. This is a deliberately *minimal* schema slice that lands ahead of the full 0.5 ingestion work — appropriately scoped.

**F6 — The bookkeeping insert is a separate statement from the DDL (latent upgrade hazard, benign today).** `run_migrations` applies `migration.sql` and then records the version in a **second** `execute_sql` call (`migrations.rs:118` then `:119-124`) — two distinct `psql` invocations, hence two transactions. If the process dies between them, the schema is applied but unrecorded, and the next start re-runs the migration. For v1 this is harmless precisely because v1 is fully idempotent (F3), so re-running converges. But the entire point of the runner is the `DESIGN §13.3` "Upgrades" story across versions, and a future non-idempotent v2 (e.g. a destructive `ALTER`/backfill) would corrupt or double-apply under that same crash window. Not a defect in the shipped slice; see **R1**.

**F7 — `MigrationReport` is returned but discarded and untested.** `run_migrations` builds a `MigrationReport { applied, current_version }` (`migrations.rs:129-132`), but the only caller drops it (`runtime.rs:213` is `self.run_migrations()?;` with the value unused) and no test asserts on either field — so the struct, its `Debug/Clone/PartialEq/Eq` derives (`:11`), and the `applied` vector are effectively dead API. Separately, the `applied` field's meaning is ambiguous: it returns *all* versions present in `schema_migrations` (pre-existing + newly applied), not "applied during this call" (`:109-128`), so on a no-op restart it still reports `[1]`. Cosmetic / coverage; see **R2**.

**F8 — The nearest-neighbour assertion is a single-row, zero-distance round-trip.** `vector_literal(0)` is used both as the stored embedding and the query vector (`schema_migrations.rs:67-90,100-105`), so the `<->` distance is exactly 0 and there is only one chunk in the table — the `ORDER BY … LIMIT 1` is satisfied trivially. This correctly proves the `vector(1024)` insert, the `chunk_embeddings`→`chunks` join, and that the pgvector operator is wired, which is what the commit claims. It does **not** prove ranking/discrimination among candidates, nor the operator's behaviour at non-zero distance; see **R3**.

**F9 — The 1024-dim is hardcoded in both the column type and a redundant CHECK (by design, noted).** `embedding vector(1024)` plus `dimension integer … CHECK (dimension = 1024)` (`migrations.rs:60,62`) pin the schema to bge-m3's width; the `dimension` column therefore only ever stores the constant `1024`. This is consistent with the locked design — `DESIGN:474` requires "Re-embedding requires an explicit index migration declared in the manifest", and `§0.4` treats bge-m3 as the provisional Phase 0 default — so a model change is intentionally a v2 migration, not a runtime variable. Flagged only so the hardcoding is a conscious choice on record, not an oversight.

**F10 — The runner has no gap/downgrade detection (irrelevant at one migration, a footgun later).** Application is purely `applied.contains(version)` over a `MIGRATIONS` slice (`migrations.rs:114-117`); it assumes ascending authoring order, does not detect non-contiguous history (DB has v2 but not v1), and silently does nothing when the DB is *ahead* of the binary (`schema_version > CURRENT_SCHEMA_VERSION`), reporting `current_version = 1` regardless. Moot with a single migration; worth hardening before the second one lands. See **R4**.

**F11 — Quality bar is clean and nothing regressed.** `cargo build --workspace`, `cargo clippy -p jurisearch-storage --all-targets`, and `cargo fmt -p jurisearch-storage --check` are all warning-free. The 5 storage unit tests pass; the new `schema_migrations` smoke passes on the real backend; and the pre-existing `durable_lifecycle` (1.42 s) and `extension_smoke` (1.10 s) tests still pass — so moving extension creation into the migration did not regress either the durable or the disposable path. Teardown is leak-free (no `/tmp/jurisearch-*pg.*` leftovers, no jurisearch postgres orphans) and the tree is clean.

**F12 — Scope: this is the first migration/schema slice, not all of 0.3.** Accurately reflected in `IMPLEMENTATION_PLAN.md:276-277`, which marks the slice Done and re-states what remains (embedded-binary policy/offline install, platform policy, BM25 + vector retrieval over the target spike corpus, the `< 500 ms` warm-latency check). The `DESIGN §13.3` "Upgrades" criterion is now *mechanically* established but not yet exercised across an actual version bump (only v1 exists). Flagged so a green migration smoke is not read as "0.3 complete" or "the upgrade story is proven across versions".

---

## Required fixes

**None.** No defect blocks proceeding. The runner is gated by the single-writer locks, migration v1 installs atomically and idempotently, the schema matches the locked design, and the restart/idempotency smoke exercises the real backend (1024-d vector insert + `<->` query) and passes; build/clippy/fmt are clean and the durable/temp paths do not regress. F6–F10 are forward-looking robustness/coverage items that do not manifest as a defect in the shipped v1-only slice.

---

## Recommendations (non-blocking)

- **R1 (F6) — Wrap each migration's DDL and its `schema_migrations` insert in one transaction** before authoring any non-idempotent migration. Either emit `BEGIN; <migration.sql>; INSERT INTO schema_migrations …; COMMIT;` as a single `execute_sql` string, or have `migration.sql` end with its own bookkeeping insert inside the same implicit transaction. This closes the crash window between `migrations.rs:118` and `:119-124` and makes the "Upgrades" guarantee (`DESIGN §13.3`) hold for migrations that aren't self-idempotent. Cheap now; load-bearing for v2.
- **R2 (F7) — Consume or assert `MigrationReport`.** Log the applied/current versions at startup (operationally useful when diagnosing a stale data dir), and/or have the smoke assert `report.current_version == CURRENT_SCHEMA_VERSION` and that a fresh DB returns `applied == [1]`. If the report is intentional future API, a one-line doc comment saying so would stop it reading as dead code. Consider renaming or documenting `applied` to make "all applied versions" vs "applied this run" unambiguous.
- **R3 (F8) — Add a multi-chunk ranking assertion.** Insert ≥2 chunks with distinct `vector(1024)` embeddings and assert the `<->` query returns the *nearer* one (non-zero distance), so the smoke proves nearest-neighbour selection, not just a zero-distance round-trip. This is the assertion that will catch a future indexing/operator regression.
- **R4 (F10) — Harden the runner before v2 lands.** When more than one migration exists, fail fast (or log loudly) if the DB schema version is *ahead* of `CURRENT_SCHEMA_VERSION` (binary older than data dir), and/or assert `MIGRATIONS` is strictly ascending and contiguous. A silent no-op downgrade is a hard-to-diagnose footgun.
- **R5 (F9/F12) — When the embeddings contract (0.4) and the spike-corpus retrieval/latency slices land, record which `§13.3` criteria remain** (binary acquisition/offline install, platform policy, BM25+vector over 50k+10k, `< 500 ms` warm latency) and whether a model-dimension change is still a clean v2 migration. The plan already keeps this discipline (`IMPLEMENTATION_PLAN.md:277`) — maintain it.

---

## Verification

All commands run from `/home/pierre/Work/jurisearch`; no source files modified.

| Check | Command | Result |
|---|---|---|
| Workspace builds | `cargo build --workspace` | ✅ Finished, no errors |
| Lint | `cargo clippy -p jurisearch-storage --all-targets` | ✅ Clean, no warnings |
| Format | `cargo fmt -p jurisearch-storage --check` | ✅ Clean (exit 0) |
| Unit tests | `cargo test -p jurisearch-storage --lib` | ✅ 5 passed (lock-key stability, version-key order, SQL quoting, advisory-lock reject, stale-pid reclaim) |
| Schema migration smoke (forced real path) | `JURISEARCH_REQUIRE_PG_EXTENSIONS=1 cargo test -p jurisearch-storage --test schema_migrations -- --nocapture` | ✅ 1 passed in 1.41 s — v1 recorded, `vector(1024)` insert, `<->` NN join, manifest round-trip, restart idempotency (`count = 1`) |
| Regression — durable lifecycle | `JURISEARCH_REQUIRE_PG_EXTENSIONS=1 cargo test -p jurisearch-storage --test durable_lifecycle -- --nocapture` | ✅ 1 passed in 1.42 s (no regression from migration wiring) |
| Regression — temp extension smoke | `JURISEARCH_REQUIRE_PG_EXTENSIONS=1 cargo test -p jurisearch-storage --test extension_smoke -- --nocapture` | ✅ 1 passed in 1.10 s |
| Teardown leak-free | `ls /tmp/jurisearch-schema-pg.* /tmp/jurisearch-pg.* /tmp/jurisearch-durable-pg.*`; jurisearch postgres scan | ✅ No leftover dirs; no jurisearch postgres orphans |
| Tree clean | `git status --short` | ✅ Empty; `target/` gitignored |

Environment: `pg_search.{so,control}` and `vector.{so,control}` present under the pgrx-managed PG 18 prefix, so the forced real path (not the skip path) is the one exercised.

---

VERDICT: GO
