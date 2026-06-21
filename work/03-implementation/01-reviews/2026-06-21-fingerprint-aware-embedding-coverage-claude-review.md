# Claude Review: Fingerprint-Aware Embedding Coverage

Verdict: GO

## Findings
- None blocking. The change is correct, minimal, and matches the plan note.

## Suggestions
- `crates/jurisearch-storage/src/ingest_accounting.rs:614-618` — The two
  predicates `c.embedding_fingerprint IS NOT NULL` and `ce.chunk_id IS NOT NULL`
  are logically subsumed by the equality `ce.embedding_fingerprint =
  c.embedding_fingerprint`: under SQL NULL semantics, if either operand is NULL
  (no embedding row → `ce.embedding_fingerprint` NULL; unfingerprinted chunk →
  `c.embedding_fingerprint` NULL) the comparison evaluates to NULL, not TRUE, and
  the row is excluded. They are harmless and document intent, so no change is
  required; a one-line comment noting they are intentional clarity-only guards
  would help future readers. Non-blocking.
- Scope/semantics note for Phase 1.7: this metric checks *internal* consistency
  between `chunks.embedding_fingerprint` (the chunk's finalized/target
  fingerprint) and `chunk_embeddings.embedding_fingerprint` (the stored vector's
  fingerprint). It does **not** compare either against the currently-configured
  embedding model fingerprint. So a corpus where both the chunk target and the
  stored embedding still carry an *old* fingerprint while the operator's config
  has moved to a new model would still report `embedding_coverage` complete and
  `query_ready: true`. The query-time safety net is `retrieval.rs:67`
  (`WHERE ce.embedding_fingerprint = {embedding_fingerprint}` filters dense
  candidates by the configured fingerprint, so a stale corpus yields zero dense
  hits). When the Phase 1.7 migration mechanism (manifest fingerprint change +
  full re-embed + index rebuild) lands, ensure it bumps
  `chunks.embedding_fingerprint` to the new target up front (via re-projection or
  a dedicated migration step) so this coverage gate correctly flips to
  not-ready until the re-embed completes. Worth recording in the 1.7 plan.
  Non-blocking — the actual migration mechanism is explicitly Phase 1.7 future
  work, and the 1.0 note here does not over-claim.
- Consider a round-trip test: after the stale-fingerprint mutation, re-insert a
  matching embedding (or reset the fingerprint) and assert `covered` returns to
  `total`. The current tests verify the gate closes on staleness but not that it
  re-opens after re-embedding. Non-blocking.

## Verification Notes
- Confirmed live repo matches the commit under review: `git log --oneline -1` is
  `1618704`, working tree clean except untracked `.codegraph/` (correctly out of
  scope per instructions). `git diff HEAD` on all four touched files is empty.
- Inspected the SQL change at `ingest_accounting.rs:611-623`. The `FILTER` now
  counts a chunk as covered only when `c.embedding_fingerprint IS NOT NULL AND
  ce.chunk_id IS NOT NULL AND ce.embedding_fingerprint = c.embedding_fingerprint`.
  `total = count(*)` over `chunks` is unchanged; only `covered` becomes stricter.
  Semantics are sound under NULL-safe comparison (verified each predicate's effect).
- Traced the data model to confirm the fingerprint roles: schema
  (`migrations.rs:46-67`) — `chunks.embedding_fingerprint` is nullable (the
  finalized/target fingerprint), `chunk_embeddings.embedding_fingerprint` is
  NOT NULL (the stored vector's fingerprint). `projection.rs:182-212`
  (`insert_chunk_embeddings`) sets the chunk fingerprint on embed and refuses to
  insert an embedding whose fingerprint diverges from a non-NULL chunk
  fingerprint; `dense.rs:111-138` finalization verifies full matching coverage
  before stamping all chunks to the spec fingerprint. The new coverage query is
  consistent with these write paths.
- Confirmed both readiness consumers share one code path: status
  (`load_ingest_health` → `load_readiness_metrics` → `load_embedding_coverage`,
  `ingest_accounting.rs:511,577-583`) and search/fetch
  (`ensure_query_readiness` → `load_ingest_embedding_coverage`,
  `main.rs:1429-1431`). `query_ready` = projection complete AND embedding
  complete, with `coverage_complete = total > 0 && covered == total`
  (`main.rs:1307-1313,1389-1391`). Stale embeddings drop `covered` below `total`,
  so both `status.query_ready` and the search/fetch gate close. Grepped for other
  embedding-coverage computations — only this one exists; no stale duplicate of
  the old semantics remains.
- Reviewed both new tests:
  - `crates/jurisearch-storage/tests/ingest_accounting.rs:262-269` — 2-chunk
    fixture (`insert_projection_fixture`, both chunks + embeddings on
    `bge-m3:1024:normalize:true`); corrupting `chunk:1240:1`'s embedding
    fingerprint yields `covered == 1`, `total == 2`. Matches expected.
  - `crates/jurisearch-cli/tests/cli_contract.rs:279-301` — single-chunk fixture;
    corrupting `chunk:1240:0`'s embedding fingerprint yields
    `query_ready == false`, `embedding_coverage.covered == 0`, `total == 1`. The
    `pg_config.clone()` change at line 148-151 is required because `pg_config` is
    reused for the second `start_durable` at line 281; minimal and correct.
  - `execute_sql` used by the tests exists at `runtime.rs:222`.
- Plan accuracy (`IMPLEMENTATION_PLAN.md`): the removed line
  ("...make embedding coverage fingerprint-aware before Phase 1.7...") is
  correctly replaced with a Done entry, and the remaining 1.0 items (safe-mode
  write/backfill, full-corpus gate thresholds) are unchanged. The Done wording
  ("a chunk counts as embedded only when its finalized chunk fingerprint is
  present and matches the corresponding `chunk_embeddings.embedding_fingerprint`")
  precisely describes the implemented behavior and does not over-claim model-level
  migration handling, which remains Phase 1.7 (lines 619-637). Accurate.
- Did not re-run the provided verification suite (cargo test for
  `ingest_accounting` / `cli_contract`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `cargo test --workspace`, `git diff --check`), which the task
  states already passed; the change is a SQL-string edit plus test additions, so
  clippy/test coverage already exercises it. The Postgres-backed tests no-op when
  `discover_pg_config` finds no managed Postgres, so a green run requires the live
  PG fixture to have been present.
