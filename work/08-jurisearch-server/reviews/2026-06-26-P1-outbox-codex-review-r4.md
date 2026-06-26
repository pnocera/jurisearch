# P1 Outbox Re-review r4

No findings.

The r3 blocker is resolved. `finalize_dense_rebuild` and `finalize_zone_dense_rebuild` now accept an `OutboxContext`, update only parent rows whose `embedding_fingerprint` actually changes via `IS DISTINCT FROM ... RETURNING document_id`, and emit document-scoped `chunks` / `zone_units` upserts in the same finalizer transaction. The CLI `embed-chunks` and `embed-zone-units` paths pass the command outbox context into both the embedding insert phase and the dense finalizer, so the stale/null parent-fingerprint case is represented in the ledger. The new regression test covers both parent tables and verifies the no-op re-finalize emits nothing.

I also checked the broader current working tree for new outbox gaps. The production producer paths thread an outbox context through LEGI/JURI projection, metadata roots, zone enrichment, zone-unit derivation, chunk and zone-unit embeddings, citation collection/enrichment, official API archiving, and hierarchy backfill. Storage writers that own their transaction emit inside it; `GenericClient` writers are called from the existing ingest batch transaction or through `in_outbox_txn` on the enrichment paths. The replicated-table inventory and digest backstop include the design §4.2 data tables, and the P1 document-scoped projection hook matches the implementation plan's deferred P4 materialization of child set semantics.

Validation run: `cargo check -q -p jurisearch-cli -p jurisearch-storage`.

VERDICT: GO
