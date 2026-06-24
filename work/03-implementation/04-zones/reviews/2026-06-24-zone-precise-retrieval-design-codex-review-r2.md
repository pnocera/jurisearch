# Code Review: Zone-Precise Retrieval Design r2

## Findings

No blocking or warning findings.

## r1 Finding Resolution

- **BLOCKER: `text_hash` was treated as already populated. Resolved.** The design now says the current helper writes `text_hash=NULL` (`crates/jurisearch-cli/src/main.rs:3702-3720`) and makes deterministic hash population an explicit Stage 1 requirement (`work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-design.md:126-130`, `:203-211`, `:356-362`). The derived table carries a non-null snapshot hash (`:110`), and Stage 2 refuses to derive from null-hash rows, forcing re-enrichment first (`:238-242`). That is internally consistent with the current v12 nullable `decision_zones.text_hash` schema (`crates/jurisearch-storage/src/migrations.rs:450-466`) and with the target index state I re-probed: schema v12, `decision_zones=2`, `ok=2`, `text_hash_not_null=0`, `upstream_update_not_null=2`.

- **WARN: builder-version invalidation was promised but not stored. Resolved.** `zone_units` now includes `zone_unit_builder_version text NOT NULL` (`work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-design.md:111`), and Stage 2 explicitly treats a builder-version mismatch as stale even when `text_hash` is unchanged (`:238-244`). That matches the existing chunk precedent: `chunks.chunk_builder_version text NOT NULL` in the base schema and juri ingestion validation against the current builder version (`crates/jurisearch-storage/src/migrations.rs:46-57`; `crates/jurisearch-ingest/src/juri/mod.rs:286-290`).

- **WARN: BM25 cited the obsolete v2 shape. Resolved.** The design now adds `search_body` with a non-empty check (`work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-design.md:106`), defines `zone_units_bm25_idx` over `search_body` using the same French analyzer shape as migration v9 (`:154-176`), and has the lexical arm query `search_body @@@ ...` (`:278-279`). That matches the current production lexical contract: migration v8 moved `chunks_bm25_idx` to `contextualized_body`, migration v9 recreated it with `ascii_folding`, French stemmer, and French stopwords (`crates/jurisearch-storage/src/migrations.rs:317-369`), and retrieval queries `c.contextualized_body @@@ ...` (`crates/jurisearch-storage/src/retrieval.rs:571-581`, `:647-660`). The target index also currently has `chunks_bm25_idx` on `contextualized_body` with that analyzer.

## Additional Verification

- The design remains faithful to the current separation boundary. It introduces new `zone_units`, `zone_unit_embeddings`, and `zone_units_bm25_idx` tables while leaving the existing `chunks`, `chunk_embeddings`, `chunks_bm25_idx`, and `hybrid_candidates_json` path untouched (`work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-design.md:16-18`, `:92-94`, `:261-267`). The current dense stale scan and finalizer operate only on `chunks` / `chunk_embeddings` (`crates/jurisearch-storage/src/dense.rs:34-91`, `:93-192`), so the proposed parallel zone path does not implicitly perturb main retrieval.

- The Judilibre resolver and normalized-zone claims still match the shipped code. Current online enrichment is Cassation-source-gated to `cass|inca` (`crates/jurisearch-cli/src/main.rs:3538-3544`), resolves by parser-valid pourvoi plus decision date (`:3652-3683`), writes through `upsert_decision_zones` (`crates/jurisearch-storage/src/decision_zones.rs:98-162`), and normalizes only `motivations`, `moyens`, and `dispositif` fragments today (`crates/jurisearch-cli/src/main.rs:3771-3801`). The design correctly limits v1 indexing to those zones and defers `expose` / `introduction` / `annexes` until a normalizer extension (`work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-design.md:234-236`, `:344`, `:395`).

- The Phase 2 honesty invariant is preserved. Bulk juri ingestion still validates heuristic chunking (`crates/jurisearch-ingest/src/juri/mod.rs:190-196`, `:754-823`), and the Phase 2 gate still expects present bulk jurisprudence sources to report `zone_accurate=false` (`crates/jurisearch-cli/src/main.rs:8800-8824`). Because the design keeps official zone units out of `chunks` and reports `status.zone_retrieval` separately, it does not inflate the existing full-juridic corpus claim (`work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-design.md:305-321`).

- The target index is still pre-zone-schema, as expected for a design-only document: schema version 12, no `public.zone_units` table, and the lazy `decision_zones` overlay currently contains two `ok` rows with null `text_hash`. That aligns with the design's build-phase sequencing and the explicit Stage 1 hash-population requirement.

VERDICT: GO
