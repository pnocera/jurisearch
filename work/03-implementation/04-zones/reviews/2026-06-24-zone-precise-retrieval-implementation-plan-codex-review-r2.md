# Code Review: Zone-Precise Retrieval Implementation Plan r2

## Findings

No blocking or warning findings.

## R1 Resolution Check

- R1 BLOCKER 1 is resolved. The revised `enrich_zone_candidates_json` predicate now explicitly selects fresh `ok`/`invalid_offsets` rows whose `decision_zones.text_hash IS NULL` regardless of TTL (`work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-implementation-plan.md:89-99`), and T2.2 adds the needed regression seed for a fresh `ok` NULL-hash row (`:176-180`). This matches the current source problem: `enrich_decision_from_judilibre` still writes `text_hash: None` on successful rows today (`crates/jurisearch-cli/src/main.rs:3705-3720`), while derivation is planned to require non-null hashes.
- R1 BLOCKER 2 is resolved. The plan now acknowledges that `format_sql_f64`, `RRF_K`, `DecisionFilters::predicate`, and `HybridCandidateQuery::effective_probes` are private in `retrieval.rs` and inserts T1.3 before T4 to extract or expose them as `pub(crate)` helpers (`work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-implementation-plan.md:124-135`). That is buildable against the current source, where those helpers are indeed private (`crates/jurisearch-storage/src/retrieval.rs:13`, `:37-41`, `:104-170`), while `rrf_weights` is already public (`:21-35`).
- R1 BLOCKER 3 is resolved. The plan no longer claims the chunk embedding writer can be reused as-is: it limits reuse to the OpenRouter HTTP generation layer (`work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-implementation-plan.md:27-31`) and adds zone-specific input/insert types plus `embed_and_insert_zone_units_with_pool` and `insert_zone_unit_embeddings` (`:206-223`). This matches the current implementation: `embed_and_insert_chunks_with_pool` builds `ChunkEmbeddingInsert` and calls `insert_chunk_embeddings`, which updates `chunks` and `chunk_embeddings` only (`crates/jurisearch-storage/src/projection.rs:827-942`).
- R1 WARN 1 is resolved. T4.2 now requires `zone` on both `SearchArgs` and `SessionSearchArgs`, threads it through `session_search_payload`, and updates help/schema plus session tests (`work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-implementation-plan.md:253-258`). That covers the current split surface where `session_search_payload` rebuilds `SearchArgs` field-by-field (`crates/jurisearch-cli/src/main.rs:4054-4085`).
- R1 WARN 2 is resolved. T4.2 now routes zone search through a dedicated `zone_search_payload` that bypasses the chunk readiness gate and calls `ensure_zone_retrieval_readiness` over `zone_units`, `zone_unit_embeddings`, `zone_units_bm25_idx`, and the requested embedding spec (`work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-implementation-plan.md:259-272`). This correctly avoids the current `search_with_postgres` path, which always checks chunk corpus readiness before calling `hybrid_candidates_json` (`crates/jurisearch-cli/src/main.rs:3002-3057`).

## Buildability And Ordering

The plan is now correctly ordered: schema/storage and helper extraction land in Z1; enrichment hash population and backfill land in Z2; derivation and zone embedding land in Z3; query routing waits until Z4; status/eval surfaces land in Z5. That order avoids querying unmaterialized tables, avoids deriving from hashless rows, and avoids exposing `--zone` before the dedicated readiness check exists.

The implementation scope is faithful to the current code. Schema v12 is current (`crates/jurisearch-storage/src/migrations.rs:3`), `decision_zones` is the existing v12 overlay (`:440-481`), the French BM25 analyzer block to mirror is present in v9 (`:350-369`), `zone_units.rs` and `zone_retrieval.rs` are new modules that will need `pub mod` exports in `lib.rs`, and the default retrieval path remains isolated by the T1.3 golden-output requirement plus the Z4 default-search golden diff (`work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-implementation-plan.md:36-41`, `:269-272`, `:337-341`).

## Notes For Implementation Review Gates

- If T1.3 moves `DecisionFilters` into `retrieval_common.rs`, keep a compatibility re-export or update all imports in the CLI/storage tests in the same diff; this is mechanical and does not change the plan verdict.
- In T1.2/T3.2, make `insert_zone_unit_embeddings` mirror the chunk writer's missing/conflicting-unit guard semantics rather than silently skipping bad staged IDs; the plan already asks for missing/conflicting tests, so this should be enforced during the Z3 diff review.
- In T4.2, keep cursor parsing inside the zone route aligned with the zone candidate cursor shape. The current generic cursor parser is tied to chunk/document cursor tags, so the Z4 review should verify either reuse is exact or zone cursors are parsed before entering the old `search_with_postgres` flow.

VERDICT: GO
