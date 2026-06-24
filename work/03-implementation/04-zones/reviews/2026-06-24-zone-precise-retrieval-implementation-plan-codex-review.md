# Code Review: Zone-Precise Retrieval Implementation Plan

## Findings

### BLOCKER: The backfill candidate predicate will not re-enrich fresh `ok` rows with `text_hash = NULL`

The plan correctly identifies the current gap: `enrich_decision_from_judilibre` writes `text_hash: None` for successful/`invalid_offsets` rows (`crates/jurisearch-cli/src/main.rs:3702-3720`, especially `:3714`), and `cache_zone_status` writes `text_hash: None` for negatives (`:3821-3838`, especially `:3831`). However, T1.2 defines `enrich_zone_candidates_json` as selecting only decisions with no fresh `decision_zones` row: `status IS NULL OR expires_at <= now()`. That excludes already-cached fresh `ok` rows whose `text_hash` is still NULL. Those rows will then also be excluded from derivation because `load_derivable_decision_zones_json` requires `decision_zones.status='ok' AND text_hash IS NOT NULL`.

Following the plan can therefore leave the existing lazy rows, and any rows written before the hash fix, permanently un-derived until their TTL expires. This contradicts the plan's own claim that "the 2 lazy NULL-hash rows are re-enriched before derivation" and can undercount zone coverage.

Actionable fix: amend T1.2/T2.2 so the enrich candidate predicate treats `status IN ('ok','invalid_offsets') AND text_hash IS NULL` as stale regardless of `expires_at`, and add an acceptance test that seeds a fresh `ok` `decision_zones` row with `text_hash NULL` and proves `enrich-zones` selects and rewrites it. Keep negative rows hashless and non-derivable.

### BLOCKER: T4 depends on private retrieval helpers while also forbidding `retrieval.rs` changes

T4.1 says the new `zone_retrieval.rs` will reuse `DecisionFilters` "verbatim" and the RRF/probes helpers `rrf_weights`, `format_sql_f64`, and `RetrievalOptions::effective_probes`. In the actual source, only `rrf_weights` and the `RetrievalOptions` fields are public. The pieces needed to build equivalent SQL are private to `retrieval.rs`: `format_sql_f64` is a private function (`crates/jurisearch-storage/src/retrieval.rs:37-41`), `DecisionFilters::predicate` is private (`:104-156`), and `effective_probes` is a private method on `HybridCandidateQuery`, not on `RetrievalOptions` (`:168-170`). `RRF_K` is also private (`:13`).

Because `zone_retrieval.rs` would be a sibling module, it cannot call those private items. At the same time, the plan's review gate says no `retrieval.rs` symbol is altered, so the proposed implementation is not buildable as written.

Actionable fix: add an explicit Z1 or Z4 task to either (a) move the shared SQL helpers into a new `retrieval_common.rs` with `pub(crate)` APIs and prove `hybrid_candidates_json` output is unchanged, or (b) duplicate the small formatting/filter/probes logic inside `zone_retrieval.rs` and test it. Update the isolation invariant from "no `retrieval.rs` symbol is altered" to "no default retrieval behavior or SQL output is altered" if shared-helper extraction is chosen.

### BLOCKER: The embedding plan cannot reuse `embed_and_insert_chunks_with_pool` as-is

The plan lists the OpenRouter embedding pool as reused as-is and T3.2 says to mirror `embed-chunks`, but the concrete helper is chunk-specific. `embed_and_insert_chunks_with_pool` accepts `Vec<ChunkEmbeddingInput>` and turns results into `ChunkEmbeddingInsert` (`crates/jurisearch-cli/src/main.rs:6294-6370`), then calls `insert_chunk_embeddings` (`:6371`). That storage function updates `chunks.embedding_fingerprint` and inserts into `chunk_embeddings` (`crates/jurisearch-storage/src/projection.rs:827-942`, especially `:888-935`). It cannot populate `zone_unit_embeddings`, and passing `zone_unit_id` values as `chunk_id` would fail against the `chunks` table.

Actionable fix: add explicit storage and CLI tasks for zone embedding writes: `ZoneUnitEmbeddingInput`, `ZoneUnitEmbeddingInsert`, `insert_zone_unit_embeddings`, and a zone-specific pool wrapper or generic pool callback that writes `zone_unit_embeddings` and updates `zone_units.embedding_fingerprint`. Add tests for missing/conflicting zone units and for idempotent upsert behavior before finalizing `zone_unit_embeddings_ivfflat_idx`.

### WARN: `search --zone` is planned only for `SearchArgs`, not for the session/serve contract

The CLI has both direct `SearchArgs` and JSONL/session `SessionSearchArgs`. `session_search_payload` rebuilds a `SearchArgs` from `SessionSearchArgs` field by field (`crates/jurisearch-cli/src/main.rs:4054-4085`), and `SessionSearchArgs` currently mirrors the search controls without any `zone` field (`:388-423`). If T4.2 only adds `--zone` to `SearchArgs`, direct CLI search may work while the agent-facing session/serve path silently lacks the new capability.

Actionable fix: either explicitly scope v1 to direct CLI only and add a regression test that unknown session `zone` input is rejected/ignored intentionally, or add `zone` to `SessionSearchArgs`, `session_search_payload`, the help/schema output, and session tests in the same Z4 task.

### WARN: The plan does not specify a zone-readiness gate for the new query path

The existing search path checks main-index readiness before it runs retrieval: `search_with_postgres` calls `ensure_query_readiness` based on the requested retrieval mode (`crates/jurisearch-cli/src/main.rs:3002-3009`) and then calls `hybrid_candidates_json` (`:3039-3057`). That readiness gate is about the current `chunks`/`chunk_embeddings` corpus, not `zone_units`/`zone_unit_embeddings`. If `--zone` is routed too late through this function, zone search can be blocked by main dense readiness and still fail to prove zone dense/BM25 readiness. If it is routed earlier, the plan still needs a dedicated zone readiness check.

Actionable fix: define a `zone_search_payload`/routing point that bypasses the main chunk readiness check and calls an explicit `ensure_zone_retrieval_readiness` based on `zone_units`, `zone_unit_embeddings`, `zone_units_bm25_idx`, and the requested fingerprint/model/dimension. Add tests for clear failure when zone embeddings or indexes are missing, plus a golden diff that default search remains unchanged.

## Notes

The main source claims I checked are otherwise aligned with the plan: schema is currently v12, the v9 BM25 analyzer block exists over `chunks.contextualized_body`, `decision_zones` is the v12 isolated overlay, the normalizer currently emits only `motivations`, `moyens`, and `dispositif`, the dense loader/finalizer are chunk-specific, `HybridCandidateQuery`/`DecisionFilters` exist in the cited shape, and the Phase 2 gate still checks bulk jurisprudence `zone_accurate=false` separately from any official-zone overlay.

VERDICT: FIXES_REQUIRED
