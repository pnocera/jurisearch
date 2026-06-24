# Code Review: Zone-Precise Retrieval Design

## Findings

### BLOCKER: `text_hash` is treated as an existing refresh key, but current enrichment never populates it

The design makes `text_hash` the atomic derivation/refresh key for `zone_units`: the schema requires `zone_units.text_hash NOT NULL`, the notes say it is copied from `decision_zones.text_hash`, Stage 2 rebuilds by comparing the decision row hash to the derived units, and D6 says incremental refresh is keyed on `update_date`/`text_hash` (`work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-design.md:109`, `:123-124`, `:189-192`, `:206-208`, `:310`).

That is not true against the current implementation or target index. The v12 schema has a nullable `decision_zones.text_hash` column (`crates/jurisearch-storage/src/migrations.rs:450-466`), but the shipped Judilibre enrichment path writes `text_hash: None` for successful `ok` rows and also for negative status rows (`crates/jurisearch-cli/src/main.rs:3702-3720`, `:3821-3837`). The target index confirms the current state: schema v12, `decision_zones_total=2`, `ok=2`, `text_hash_not_null=0`, `upstream_update_not_null=2`.

As written, Z2 can eagerly populate rows, but Z3 cannot derive `zone_units.text_hash NOT NULL` from today's helper, cannot perform the advertised `text_hash` mismatch rebuild, and cannot use `text_hash` to cascade refreshes.

Actionable fix: make hash population an explicit Stage 1 requirement instead of saying `decision_zones` already carries it "for exactly this". Specify the exact deterministic hash input, e.g. SHA-256 over Judilibre `text` plus normalized `zones_json`/provider decision id/update date, populate it in `enrich_decision_from_judilibre` for `ok` rows, and define how existing/null rows are handled (`NULL` means stale and must be re-enriched before derivation, or Stage 2 computes and backfills the hash from `raw_json` in the same transaction). Then make Stage 2's delete/reinsert predicate depend on that populated hash.

### WARN: the schema promises builder-version invalidation but omits the column

Stage 2 says zone derivation must carry `zone_unit_builder_version` so a derivation-logic change can force a full rebuild (`work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-design.md:206-210`). The `zone_units` schema sketch does not include that column (`:99-117`). The existing chunk model does have `chunk_builder_version text NOT NULL` in `chunks` and validates the juri chunk builder version during ingestion (`crates/jurisearch-storage/src/migrations.rs:46-57`; `crates/jurisearch-ingest/src/juri/mod.rs:286-290`), so the analogy is real, but the proposed table cannot support it.

Without the column, a future normalizer/fragmentation change has no durable way to distinguish "current text hash but old derivation logic" from "current text hash and current derivation logic". That undercuts the stated rebuild story even after the `text_hash` issue is fixed.

Actionable fix: add `zone_unit_builder_version text NOT NULL` to `zone_units`, include it in the uniqueness/rebuild criteria or stale-unit query, and specify that `build-zone-units` treats rows with a different builder version as stale even when `decision_zones.text_hash` is unchanged.

### WARN: the BM25 sketch cites the obsolete v2 index shape instead of the current lexical contract

The design sketches `zone_units_bm25_idx` as `ON zone_units USING bm25 (zone_unit_id, body) WITH (key_field = 'zone_unit_id')` and calls it parallel to `chunks_bm25_idx` migration v2 (`work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-design.md:147-155`). In the current schema, `chunks_bm25_idx` was replaced after v2: migration v8 moved BM25 to `contextualized_body`, and migration v9 recreated it with the French analyzer (`ascii_folding`, French stemmer, French stopwords) (`crates/jurisearch-storage/src/migrations.rs:317-340`, `:349-369`). The retrieval lexical arm also queries `c.contextualized_body @@@ ...`, not `c.body` (`crates/jurisearch-storage/src/retrieval.rs:571-581`, `:647-660`).

The proposed zone lexical index is still physically separate, so it does not violate Option B, but it is not actually parallel to the current production lexical behavior. It risks a silent quality regression for accents, morphology, and French legal terms in zone search, and it leaves the query field/analyzer contract underspecified.

Actionable fix: update the design to mirror the current v9 BM25 contract: either index `body` with the same `text_fields` analyzer under `zone_units_bm25_idx`, or add a `contextualized_body`/`search_body` column for zone units and index/query that field with the same analyzer. Also update the citation from migration v2 to migrations v8/v9 so implementers do not copy the stale index shape.

## Verified claims

- The target index source counts and resolver-reachable counts match the design's coverage story: `cass=141,616` with `117,674` parser-valid pourvois, `inca=384,312` with `377,027`, `capp=72,929` with only `120`, and `jade=545,939` with `0`; total resolver-reachable Cassation remains `494,701`.
- `decision_zones` is currently the expected lazy overlay: v12 schema, two current rows, both `ok`; this supports the design's "lazy 2-row state" claim but also exposes the `text_hash` problem above.
- The proposed separate tables would leave the current main retrieval structures untouched if implemented as described. The existing dense stale scan reads only `chunks LEFT JOIN chunk_embeddings` (`crates/jurisearch-storage/src/dense.rs:34-91`), `finalize_dense_rebuild` counts and updates only `chunks`/`chunk_embeddings` (`crates/jurisearch-storage/src/dense.rs:93-180`), and `hybrid_candidates_json` builds candidates from `chunks`, `chunk_embeddings`, and `documents` (`crates/jurisearch-storage/src/retrieval.rs:269-386`, `:556-724`).
- The Phase 2 honesty invariant is correctly characterized: bulk juri ingestion enforces `chunking_provenance="heuristic"` (`crates/jurisearch-ingest/src/juri/mod.rs:190-196`, `:754-823`), and the gate checks all present bulk juri sources for `zone_accurate=false` (`crates/jurisearch-cli/src/main.rs:8800-8824`). Keeping zone units out of `chunks` preserves that invariant.
- The resolver/helper names cited by the design exist and behave broadly as claimed: `decision_resolution_metadata_json` returns first parser-valid pourvoi plus date (`crates/jurisearch-storage/src/decision_zones.rs:48-76`), `upsert_decision_zones` is the write helper (`crates/jurisearch-storage/src/decision_zones.rs:98-162`), and the current normalizer stores `motivations`/`moyens`/`dispositif` fragments only (`crates/jurisearch-cli/src/main.rs:3771-3801`).

VERDICT: FIXES_REQUIRED
