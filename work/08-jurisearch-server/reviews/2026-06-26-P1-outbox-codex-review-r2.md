# P1 Outbox Re-review

## Summary

The r1 same-transaction blocker is mostly fixed for the paths called out there: Judilibre archive/cache/upsert writes now run through `in_outbox_txn`, and the legislation collect/enrich loops wrap the occurrence/resolution and archive/resolution pairs in explicit transactions. The digest fixes also address the prior content-omission and aggregate-order findings.

I still do not think this is ready. One replicated mutation remains outside the outbox entirely, and the embedding writers now expose a second parent-table capture ambiguity that should be closed before relying on the ledger as the primary diff source.

## BLOCKER

### Citation occurrence-count finalization mutates replicated resolution rows without any outbox row

`collect_legislation_citations_payload` now wraps the occurrence insert and the pending-resolution insert in one transaction at `crates/jurisearch-cli/src/enrichment/legislation.rs:239`, which fixes the r1 rollback window for those two writes. But the command then calls `finalize_citation_occurrence_counts(&postgres)` at `crates/jurisearch-cli/src/enrichment/legislation.rs:282`, and that helper runs an auto-commit `UPDATE legislation_citation_resolutions` at `crates/jurisearch-storage/src/legislation_citations.rs:197` without accepting an `OutboxContext` or emitting any `legislation_citation_resolutions` change.

This is not just a bookkeeping update. `occurrence_count` is a replicated, non-volatile column (`crates/jurisearch-storage/src/migrations.rs:681`) and is now included in the whole-row digest. The common failure case is a new decision citing an already-known `citation_key`: `insert_citation_occurrence_with_client` emits a `decision_legislation_citations` row, `upsert_citation_resolution_pending_with_client` hits `ON CONFLICT DO NOTHING` and emits no resolution row (`crates/jurisearch-storage/src/legislation_citations.rs:155`), then the finalizer increments `occurrence_count` with no ledger entry. A package built from the outbox can therefore miss the resolution-row update while the producer's authoritative table has changed.

Fix: make occurrence-count maintenance outbox-aware and rollback-coupled. Either update the affected `(corpus, citation_key)` count inside the same `in_outbox_txn` as each new occurrence and emit a `legislation_citation_resolutions` upsert when the count changes, or replace the finalizer with a transactional `UPDATE ... WHERE occurrence_count IS DISTINCT FROM computed_count RETURNING corpus, citation_key` and emit one resolution upsert per returned row before committing. Add an integration test for adding a second occurrence to an existing citation key and asserting both the count change and its outbox row are present in the same transaction.

## WARN

### Embedding writers update parent table fingerprints but emit only child-table scopes

`insert_chunk_embeddings` updates `chunks.embedding_fingerprint` at `crates/jurisearch-storage/src/projection/embeddings.rs:80`, then emits only `table_name='chunk_embeddings'` at `crates/jurisearch-storage/src/projection/embeddings.rs:148`. The zone-unit equivalent updates `zone_units.embedding_fingerprint` at `crates/jurisearch-storage/src/zone_units.rs:397`, then emits only `table_name='zone_unit_embeddings'` at `crates/jurisearch-storage/src/zone_units.rs:465`. Both parent fingerprint columns are replicated columns (`crates/jurisearch-storage/src/migrations.rs:55` and `crates/jurisearch-storage/src/migrations.rs:509`) and the new whole-row digests include them.

If the package builder treats those events as child-table-only scopes, the client can receive the vector rows while leaving the parent `chunks` / `zone_units` fingerprints stale. That produces a digest mismatch at best, and a silent compatibility/readiness mismatch if the postcondition is not yet wired.

Fix: either emit a paired document-scoped parent-table event in the same transaction (`chunks`/document scope for chunk embeddings, `zone_units`/document scope for zone-unit embeddings), or make the event contract explicitly table-grouped so a `chunk_embeddings` or `zone_unit_embeddings` scope also materializes and applies the parent fingerprint update. Whichever contract is intended should be covered by the hook-coverage test and by a digest-style test that starts with null parent fingerprints, embeds, and verifies the changed parent row is represented by the outbox window.

## Notes

The r1 digest findings appear resolved: `corpus_table_digests` signs `to_jsonb(<alias>)` minus volatile timestamp/TTL keys and orders inside `string_agg` at `crates/jurisearch-storage/src/outbox.rs:272`.

I did not run the test suite during this re-review; the review was static against the current uncommitted and untracked working tree.

VERDICT: FIXES_REQUIRED
