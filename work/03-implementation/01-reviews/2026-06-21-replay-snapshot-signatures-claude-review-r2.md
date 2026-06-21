# Claude Review: Replay Snapshot Signatures R2

Verdict: GO

## Findings

- None blocking. The R1 blocker is resolved.

  R1 blocker (full replay-snapshot hashing on the per-query readiness path) is
  **fixed** in `92c11bc`. The fix extracts the two coverage COUNT queries into
  `load_readiness_metrics` and exposes a dedicated `load_ingest_readiness`
  entry point (`crates/jurisearch-storage/src/ingest_accounting.rs:552-599`).
  `ensure_query_readiness` now calls `load_ingest_readiness`
  (`crates/jurisearch-cli/src/main.rs:1412`) instead of `load_ingest_health`,
  so `search` (`main.rs:403`) and `fetch` (`main.rs:458`) no longer trigger
  `load_replay_snapshot`. Confirmed by call-graph: `load_replay_snapshot`
  (`ingest_accounting.rs:601`) is invoked only from `load_ingest_health`
  (`ingest_accounting.rs:511`), and `load_ingest_health` is now called only
  from the `status` command (`main.rs:1305`) and the storage test. The
  full-corpus `md5(concat_ws(...))` / `string_agg(... ORDER BY ...)` hashing of
  large `body`/`canonical_json`/`embedding::text` columns no longer runs on the
  hot path. The per-query gate cost is back to the pre-snapshot baseline (the
  same two coverage aggregates that shipped in `a9c7333`).

## Suggestions

- **Coverage source can't diverge — good, and the test locks it in.** Both
  `load_ingest_health` and `load_ingest_readiness` delegate to the shared
  `load_readiness_metrics`, so the `status` report and the query gate can never
  disagree on coverage. The new test assertion
  (`crates/jurisearch-storage/tests/ingest_accounting.rs:227-235`) verifies the
  two entry points agree; it is somewhat redundant given the shared helper but
  is worth keeping as regression protection against a future refactor that
  re-splits the SQL. Non-blocking.

- **`fetch` still pays for the embedding coverage query it never reads.**
  `load_readiness_metrics` always runs both the projection and embedding count
  queries, but the `Fetch` gate only consumes `projection_coverage`
  (`main.rs:1429` gates embedding on `Search` only). A `Fetch` could skip the
  embedding aggregate. Minor; pre-existing pattern, not introduced here.

- **Per-query coverage aggregates still scan full tables.** The readiness gate
  runs `count(DISTINCT d.document_id) ... FROM documents LEFT JOIN chunks` and a
  `count(*) ... FROM chunks LEFT JOIN chunk_embeddings` on every `search`/`fetch`
  (each on a fresh connection). This is the intended gate mechanism per the plan
  (Phase 1.0 acceptance line 488, "Query access is blocked ... until required
  projections pass their gates") and is *not* a regression from this change — but
  at the Phase 1 full-LEGI scale these two aggregates are non-trivial per query.
  A later slice may want a cheaper readiness check (cached/manifest-backed
  coverage, or an `EXISTS`-style incomplete-row probe) once broad canonicalization
  lands. Out of scope for this step.

- **R1 snapshot suggestions remain open (now status-only).** The snapshot
  computation code is unchanged by the fix, so the three R1 suggestions still
  apply but now affect only the `status` path: (1) the five component queries +
  signature run as independent `query_one` calls with no wrapping
  `REPEATABLE READ` transaction (`ingest_accounting.rs:604-664`), so a concurrent
  ingest could yield a combined signature for a state that never existed at one
  instant; (2) `coalesce(col, '')` makes `NULL`↔`''` transitions invisible to the
  signature; (3) the top-level signature does a Postgres round-trip
  (`SELECT md5($1)`, `ingest_accounting.rs:661-664`) that could be hashed in Rust.
  All non-blocking; snapshots are taken post-ingest.

## Verification Notes

- Re-read the fix commit (`git show 92c11bc`, full diff) and the live files
  after the fix:
  - `crates/jurisearch-storage/src/ingest_accounting.rs`: new
    `IngestReadinessReport` struct (146-151 region), `load_ingest_health`
    (450-550) now calling `load_readiness_metrics` (510) + `load_replay_snapshot`
    (511), new `load_ingest_readiness` (552-558), shared `load_readiness_metrics`
    (560-599), unchanged `load_replay_snapshot`/`snapshot_component` (601-693).
  - `crates/jurisearch-cli/src/main.rs`: import block (32-38),
    `ensure_query_readiness` (1408-1438) now using `load_ingest_readiness`,
    `index_not_query_ready` (1440-1460) now typed on `&IngestReadinessReport`,
    `search`/`fetch` gate calls (403, 458), `status` path (1304-1313) still using
    `load_ingest_health`.
- Confirmed the decoupling with a workspace-wide grep for
  `load_ingest_health` / `load_ingest_readiness` / `load_replay_snapshot` /
  `ensure_query_readiness`: `load_replay_snapshot` is reached only via
  `load_ingest_health`; `load_ingest_health` is referenced only at `main.rs:1305`
  (status) and in the storage test; `load_ingest_readiness` is referenced only by
  `ensure_query_readiness` and the storage test. No query-path caller hashes the
  snapshot.
- Determinism unaffected: the fix did not touch `load_replay_snapshot` or
  `snapshot_component`; the R1 determinism analysis (PK-ordered `string_agg`,
  stable empty-table `md5('')`, canonical `::text` casts, no injection surface)
  still holds.
- Status JSON unchanged: `load_ingest_health` still populates
  `replay_snapshot_status` + `replay_snapshot`, and `status` derives
  `query_ready` from projection/embedding coverage (`main.rs:1307-1313`).
- Tests: storage test adds `load_ingest_readiness` assertions cross-checking
  coverage against `load_ingest_health`
  (`tests/ingest_accounting.rs:227-235`); existing replay-snapshot
  count/signature/stability/title-change assertions (236-256) are intact.
- Plan accuracy: `IMPLEMENTATION_PLAN.md` Phase 1.0 (470-499) remains accurate.
  Line 497 (retrieval enforces coverage gates) and line 498 (ingest health
  reports replay snapshot) match the code; the decoupling aligns the
  implementation with acceptance line 488 (gate on projections, not on the
  diagnostic snapshot). No plan edit required.
- I did not re-run the listed `cargo test`/`clippy` commands (the integration
  tests require managed Postgres); I relied on the stated green pre-review runs
  and inspected the code, call graph, and tests directly. The fix is a pure
  refactor of where the snapshot is computed, so the green checks plus the static
  call-graph confirmation are sufficient for the blocking concern.
