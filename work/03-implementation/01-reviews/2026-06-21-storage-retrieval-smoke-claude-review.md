# Review — Storage Retrieval Smoke (commit `1bfa246`)

Date: 2026-06-21
Reviewer: Claude (Opus 4.8, 1M context)
Scope: local commit `1bfa246` "Add storage retrieval smoke" — migration v2 in `crates/jurisearch-storage/src/migrations.rs` (`chunk_bm25_index`, `CURRENT_SCHEMA_VERSION` bump 1→2), new `crates/jurisearch-storage/tests/retrieval_smoke.rs`, updated two-step-chain expectations in `crates/jurisearch-storage/tests/schema_migrations.rs`, and the §0.3 status note in `work/03-implementation/IMPLEMENTATION_PLAN.md`.
Plan context: `IMPLEMENTATION_PLAN.md §0.3 Embedded Postgres Spike` ("BM25 + vector retrieval over the target spike corpus"); `DESIGN §13.3` "Upgrades: index / schema / extension migration story". Builds directly on the prior slice reviewed in `2026-06-21-storage-schema-migrations-claude-review.md`.
Constraint: review-only; no source files modified.

---

## Findings (most important first)

**F1 — The slice does exactly what it claims and is verified end-to-end on the real backend.** Migration v2 (`migrations.rs:99-113`) adds a `pg_search` BM25 index over `chunks(chunk_id, body)` keyed on `chunk_id` (`:103-105`) and bumps the manifest to `schema_version = 2` (`:107-111`); `CURRENT_SCHEMA_VERSION` is now `2` (`:3`). The new `retrieval_smoke` (`retrieval_smoke.rs:37-100`) seeds two documents/chunks/embeddings and proves both retrieval modes: a BM25 lexical query (`:81-87`) and a pgvector nearest-neighbour query (`:90-96`). Forced onto the real path (`JURISEARCH_REQUIRE_PG_EXTENSIONS=1`) it passes in 1.17 s. This is genuine retrieval, not "did not error".

**F2 — Both retrieval assertions are real two-candidate discriminations, not trivial round-trips — this closes the prior review's R3.** The BM25 query `body @@@ 'responsabilite faute dommage'` is run against two chunks: `chunk:1240:0` body `responsabilite civile faute reparation dommage article 1240` (matches three terms) and `chunk:recipe:0` body `recette tarte pommes cannelle dessert` (matches none), so `ORDER BY paradedb.score(chunk_id) DESC LIMIT 1` selecting `chunk:1240:0` (`:88`) proves discrimination. The vector test stores two *distinct* unit vectors — `legal_vector = vector_literal(0)` on `chunk:1240:0` and `unrelated_vector = vector_literal(1)` on `chunk:recipe:0` (`:46-47,73-79`) — and queries with `legal_vector`, so `<->` returns `chunk:1240:0` at distance 0 vs `chunk:recipe:0` at distance √2 (`:97`). Both are now non-trivial nearest-of-two selections, directly addressing the zero-distance/single-row gap flagged as R3 in the schema-migrations review.

**F3 — The BM25 query genuinely exercises migration v2's index, and fails closed if it were missing.** The `@@@` operator and `paradedb.score()` require a `bm25` index on the column; absent the index, the query errors rather than silently sequential-scanning. So a passing `retrieval_smoke` is positive proof that migration v2's `chunks_bm25_idx` was created and is in use. Note the test never calls `run_migrations()` explicitly — it relies on `start_durable` running migrations under the storage locks (`runtime.rs:213`), which is the correct production path and means the smoke also covers auto-migration on a fresh durable root.

**F4 — Migration v2 applies atomically, under the single-writer locks, in one transaction.** The runner wraps each pending migration's DDL plus its `schema_migrations` insert in a single `BEGIN; … COMMIT;` string (`migrations.rs:151-157`) — this is the R1 fix landed in `d332669` and it now protects v2. `CREATE INDEX … USING bm25` is non-concurrent, so it is transaction-safe inside that block (confirmed empirically: both real-path tests pass). `validate_migration_list()` (`:169-193`) still enforces contiguous-from-1 versions and `CURRENT_SCHEMA_VERSION == latest migration`, so the v2 addition can't silently desynchronise the constant from the slice.

**F5 — Idempotency is preserved across the new migration.** Three layers as before: the runner skips any already-applied version (`migrations.rs:147-149`), v2's DDL is `CREATE INDEX IF NOT EXISTS` (`:103`), and its manifest seed is `INSERT … ON CONFLICT (key) DO UPDATE` (`:107-111`). The `schema_migrations` smoke restarts against the populated root and asserts `count(*) FROM schema_migrations == CURRENT_SCHEMA_VERSION` i.e. `2` (`schema_migrations.rs:105-108`), proving v1+v2 each applied exactly once with no duplicate re-application.

**F6 — Schema-migration expectations are correctly upgraded to a two-step chain.** `report.applied` is now asserted to be `(1..=CURRENT_SCHEMA_VERSION)` = `[1, 2]` (`schema_migrations.rs:56-59`); the run records both `canonical_documents_chunks_vectors` and `chunk_bm25_index` (`:66-70`); the migration count and manifest `schema_version` both track `CURRENT_SCHEMA_VERSION` (`:108,123`). Every expectation is derived from the constant rather than a literal `1`, so the test moves with the schema. The manifest correctly ends at `2` because v1 seeds `1` and v2 overwrites with `2` in the same ordered run.

**F7 — The two name-position assertions are coupled to `CURRENT_SCHEMA_VERSION` by arithmetic and will misfire when v3 lands (latent test fragility, benign today).** `schema_migrations.rs:66-69` checks for `"{CURRENT_SCHEMA_VERSION - 1}:canonical_documents_chunks_vectors"` and `:70` for `"{CURRENT_SCHEMA_VERSION}:chunk_bm25_index"`. This is correct *only* while there are exactly two migrations. When a v3 is authored and the constant becomes `3`, the first assertion will look for `canonical_documents_chunks_vectors` at version `2` and `chunk_bm25_index` will no longer be at `CURRENT_SCHEMA_VERSION` — both `assert!(... .contains(...))` checks would then fail (or, worse, pass against the wrong row if a future name collides). The names are pinned to *fixed* versions (1 and 2), so the assertions should reference those literals, not offsets from `CURRENT`. Not a defect in this commit; see **R1**.

**F8 — Helper duplication between the two integration tests (minor).** `discover_pg_config` and `vector_literal` in `retrieval_smoke.rs:3-35` are near-verbatim copies of the same helpers in `schema_migrations.rs:6-38` (differing only in the skip-message text). Harmless, but a future change to the discovery/skip contract now has to be made in two places. See **R2**.

**F9 — Coverage overlap with the schema-migrations smoke (acceptable).** `retrieval_smoke`'s vector portion overlaps the `<->` join already exercised by `schema_migrations.rs:110-118`. The net new value here is real and worth the overlap: (a) the BM25 path, which nothing else covers, and (b) a two-distinct-vector discrimination that the schema smoke's zero-distance single-row query did not provide. Co-locating both retrieval modes in one "retrieval smoke" is a reasonable home for the 0.3 retrieval mechanic.

**F10 — The plan note is accurate and does not overstate completion.** `IMPLEMENTATION_PLAN.md` adds a "Done" bullet for the retrieval smoke and re-scopes "Remaining before 0.3" by dropping "BM25 + vector retrieval" and keeping "retrieval over the target spike corpus" plus the latency check. This is faithful: the slice proves the BM25 + pgvector *mechanic* over synthetic chunks, not over the 50k+10k spike corpus and not against the `< 500 ms` warm-latency target. A green retrieval smoke must not be read as "0.3 complete"; the note correctly avoids that claim.

**F11 — Quality bar is clean and nothing regressed.** `cargo fmt --check` and `cargo clippy --all-targets` for `jurisearch-storage` are warning-free. The 5 lib unit tests pass; `retrieval_smoke` passes (1.17 s) and `schema_migrations` passes (1.44 s) on the forced real path; and the pre-existing `durable_lifecycle` (1.43 s) and `extension_smoke` (1.10 s) tests still pass — so the v2 addition did not regress the durable or disposable paths. Teardown is leak-free (no `/tmp/jurisearch-*-pg.*` leftovers, no jurisearch-managed postgres orphans) and the working tree is clean.

---

## Required fixes

**None.** No defect blocks proceeding. Migration v2 installs atomically and idempotently under the single-writer locks, `CURRENT_SCHEMA_VERSION`/migration-list invariants hold, and the retrieval smoke proves both BM25 lexical and pgvector nearest-neighbour selection as genuine two-candidate discriminations on the real backend; build/clippy/fmt are clean and the durable/temp paths do not regress. F7–F9 are forward-looking test-maintenance items, not defects in the shipped slice.

---

## Recommendations (non-blocking)

- **R1 (F7) — Pin the migration-name assertions to literal versions.** Before authoring v3, change `schema_migrations.rs:66-70` to assert `"1:canonical_documents_chunks_vectors"` and `"2:chunk_bm25_index"` (or index into `MIGRATIONS`), so the checks verify the actual recorded history rather than a position relative to `CURRENT_SCHEMA_VERSION` that breaks the moment a third migration lands.
- **R2 (F8) — Extract `discover_pg_config` / `vector_literal` into a shared `tests/common` module.** The three integration tests (`retrieval_smoke`, `schema_migrations`, and the lifecycle/extension smokes) repeat the same discovery/skip and vector-literal logic; one shared helper keeps the `JURISEARCH_REQUIRE_PG_EXTENSIONS` skip contract consistent.
- **R3 (F3) — Consider one explicit index-presence assertion in the BM25 smoke** (e.g. assert `chunks_bm25_idx` appears in `pg_indexes`, or `EXPLAIN` shows the bm25 scan). The current test already fails closed without the index, but an explicit assertion would turn an opaque operator error into a self-describing failure and document the dependency on migration v2.
- **R4 (F2/F10) — When the spike-corpus retrieval slice lands, exercise accent/stemming behaviour and ranking depth.** The synthetic bodies use unaccented French (`responsabilite`) matched against an unaccented query, so the smoke does not depend on the tokenizer's accent-folding. Real `legi`/`jurisprudence` text will, so the corpus-scale slice should confirm the BM25 analyzer config and assert ordering across more than two candidates, alongside the `< 500 ms` warm-latency check still listed as remaining.

---

## Verification

All commands run from `/home/pierre/Work/jurisearch`; no source files modified.

| Check | Command | Result |
|---|---|---|
| Format | `cargo fmt -p jurisearch-storage --check` | ✅ Clean (exit 0) |
| Lint | `cargo clippy -p jurisearch-storage --all-targets` | ✅ Clean, no warnings |
| Unit tests | `cargo test -p jurisearch-storage --lib` | ✅ 5 passed (lock-key stability, version-key order, SQL quoting, advisory-lock reject, stale-pid reclaim) |
| Retrieval smoke (forced real path) | `JURISEARCH_REQUIRE_PG_EXTENSIONS=1 cargo test -p jurisearch-storage --test retrieval_smoke -- --nocapture` | ✅ 1 passed in 1.17 s — BM25 `@@@`/`paradedb.score` selects the legal chunk, `<->` selects the nearer of two distinct vectors |
| Schema migration smoke (two-step chain) | `JURISEARCH_REQUIRE_PG_EXTENSIONS=1 cargo test -p jurisearch-storage --test schema_migrations -- --nocapture` | ✅ 1 passed in 1.44 s — `applied == [1,2]`, both migration names recorded, `count == 2`, manifest `schema_version == 2` |
| Regression — durable lifecycle | `JURISEARCH_REQUIRE_PG_EXTENSIONS=1 cargo test -p jurisearch-storage --test durable_lifecycle` | ✅ 1 passed in 1.43 s (no regression from v2) |
| Regression — temp extension smoke | `JURISEARCH_REQUIRE_PG_EXTENSIONS=1 cargo test -p jurisearch-storage --test extension_smoke` | ✅ 1 passed in 1.10 s |
| Teardown leak-free | `ls /tmp/jurisearch-retrieval-pg.* /tmp/jurisearch-schema-pg.* …`; jurisearch postgres scan | ✅ No leftover temp dirs; no jurisearch-managed postgres orphans (only an unrelated system/`gciauto2` instance) |
| Tree clean | `git status --short` | ✅ Empty; `target/` gitignored |

Environment: `pg_search.so` and `vector.so` present under `~/.pgrx/18.4/pgrx-install/lib/postgresql/`, so the forced real path (not the skip path) is the one exercised.

---

VERDICT: GO
