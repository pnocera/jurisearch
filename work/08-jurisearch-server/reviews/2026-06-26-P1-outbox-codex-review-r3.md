# P1 Outbox Re-review r3

## Summary

The two r2 findings are addressed in the main producer paths I checked. `finalize_citation_occurrence_counts` now updates only changed counts, emits `legislation_citation_resolutions` outbox rows in the same transaction, and `collect_legislation_citations_payload` passes an `OutboxContext`. The embedding insert writers also now emit paired parent-table scopes for `chunks` and `zone_units`.

I found one remaining outbox gap in the dense finalize paths. These commands are part of the P1 acceptance surface (`ingest embed-chunks`, `ingest embed-zone-units`) and still contain replicated parent-table updates with no outbox context.

## BLOCKER

### Dense rebuild finalizers can still stamp parent fingerprints without any outbox row

`embed_chunks_payload` threads an `OutboxContext` through the page-level embedding inserts, but then calls `finalize_dense_rebuild(&postgres, ...)` at `crates/jurisearch-cli/src/ingest/pipeline.rs:582` without passing that context. The finalizer runs its own transaction and executes `UPDATE chunks SET embedding_fingerprint = $1` at `crates/jurisearch-storage/src/dense.rs:180` with no `package_change_log` emit. `chunks.embedding_fingerprint` is a replicated, digested column (`crates/jurisearch-storage/src/migrations.rs:55`).

The zone-unit path has the same gap: `embed_zone_units_payload` calls `finalize_zone_dense_rebuild(&postgres, ...)` at `crates/jurisearch-cli/src/ingest/pipeline.rs:433`, and that finalizer executes `UPDATE zone_units SET embedding_fingerprint = $1` at `crates/jurisearch-storage/src/zone_units.rs:559` with no outbox row. `zone_units.embedding_fingerprint` is also replicated (`crates/jurisearch-storage/src/migrations.rs:509`).

In the common happy path the insert writers may already have stamped the rows and emitted the paired parent scopes, but the finalizers are still independent replicated-table writers. Any stale/null parent fingerprint with an already-correct child embedding, or any repair/upgrade path that reaches the finalizer, can change authoritative `chunks` / `zone_units` rows while the outbox window contains no corresponding parent scope. A package built from the outbox can therefore miss the parent fingerprint update.

Fix: make both dense finalizers outbox-aware. Thread the command `OutboxContext` into `finalize_dense_rebuild` and `finalize_zone_dense_rebuild`; change the parent updates to `WHERE embedding_fingerprint IS DISTINCT FROM $1 RETURNING ...`; emit one document-scoped `chunks` or `zone_units` `upsert` per affected document before committing. Add regression tests that seed a matching child embedding with a stale/null parent fingerprint, run each finalizer with an outbox context, assert the parent fingerprint changes and the outbox row is present, then re-run and assert the no-op finalizer emits nothing.

## Notes

I did not run the test suite during this re-review; this was a static review of the current uncommitted and untracked working tree.

VERDICT: FIXES_REQUIRED
