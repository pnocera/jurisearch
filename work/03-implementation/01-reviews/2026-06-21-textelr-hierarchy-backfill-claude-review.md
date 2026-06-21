# Claude Review — Consume TEXTELR Links in Hierarchy Backfill

Verdict: GO

- Commit reviewed: `adba924` "Consume TEXTELR links in hierarchy backfill"
- Baseline: `c995a93`
- Reviewer: Claude (Opus 4.8), 2026-06-21

The change is correct, idempotent, regression-free for the existing direct-edge path, and a sound
next step toward TEXTELR-driven hierarchy assembly. The only substantive concerns are
performance/scalability (a known, partially-deferred area) and a few test gaps. None block.

## What I verified (correctness — sound)

- **Anchor reuse across both payload shapes works by design.** The TEXTELR branch puts the
  `LIEN_ART` structure-link JSON into the `edge_payload` column
  (`projection.rs:~317`). `hierarchy_backfill_anchor` (`projection.rs:508-529`) reads
  `payload.attributes[]` for a `debut` key. Because the prior commit preserves every raw DILA
  attribute on each structure link (`ParsedTextStructLink.attributes`), and real `LIEN_ART`
  elements carry `debut` (confirmed against `…/opendata` samples, e.g.
  `<LIEN_ART debut="1994-03-24" … id="LEGIARTI…" />`), the anchor resolves to the link's `debut`,
  falling back to `document.valid_from`. The storage test proves this drives correct temporal
  section-version selection: article `…52000003` (anchor `1804-03-21`) selects "Titre preliminaire"
  over the 2020 "Titre contemporain" version of the same `LEGISCTA` uid.
- **"Nearest preceding LIEN_SECTION_TA" pairing is right for flat DILA STRUCT.** Real `<STRUCT>`
  is a flat preorder sequence with `niv`; the correlated `ORDER BY ordinality DESC LIMIT 1` lateral
  (`projection.rs:~328-339`) selects the immediate container section. `IS NOT NULL` on the
  section target and the inner-join (`ON true` with `LIMIT 1`) correctly drop `LIEN_ART`s with no
  preceding resolvable section rather than emitting a null section.
- **Direct edges are preferred; no regression.** `source_rank` (0 direct, 1 TEXTELR) sorts direct
  candidates first within a document, and the unchanged `select_hierarchy_backfill_candidate`
  picks the first temporally-valid one. The pre-existing direct-edge SQL, params `$1/$2/$3`, and
  selection logic are byte-for-byte preserved inside the new CTE, and the prior test assertions
  (`documents_updated == 2`, 2020-vs-1804 selection) still pass.
- **Backward/scoping-safe JSON handling.** `coalesce(canonical_json->'structure_links','[]')`
  makes pre-structure-links TEXTELR rows yield zero candidates (graceful until the
  already-shipped parser/schema replay bump repopulates them). JSON-null `target_source_uid`
  never matches `= d.source_uid`, so unresolved links are skipped.
- **Text-only scope is isolated and idempotent.** With `$4 = text_source_uids` and empty
  `$2/$3`, the direct branch yields nothing and only TEXTELR-linked articles are enriched; the
  test confirms `documents_updated == 1` then `0` on replay, and that direct-edge docs are not
  spuriously touched. CLI scope tracking (`main.rs:~1021-1037`), `is_empty()` (`projection.rs:33-39`),
  and manifest/output counters are wired consistently and covered by the CLI contract test.
- Storage and CLI tests pass; `cargo clippy --workspace --all-targets -- -D warnings` is clean.

## Non-blocking findings (ordered by importance)

### 1. TEXTELR join scalability — address before any full-corpus / large-archive run
The TEXTELR branch (`projection.rs:~307-345`) is materially heavier than the indexed 3-table
direct join, and three structural facts compound at corpus scale:

- **No join key between `documents d` and `legi_metadata_roots text_struct`** other than the
  predicate buried in the lateral (`article_link.link->>'target_source_uid' = d.source_uid`), and
  **there is no index on `documents.source_uid`** (`migrations.rs:85-90` defines indexes on
  `kind`, `validity`, etc., but not `source_uid`). A nested-loop plan here degenerates to a
  sequential scan of `documents` per expanded link.
- **Scope does not prune the inputs.** The TEXTELR `WHERE` is an `OR` across three relations
  (`d.document_id = ANY($2) OR text_section.link->>'…' = ANY($3) OR text_struct.source_uid =
  ANY($4)`), which Postgres generally cannot push down to restrict `documents` or `text_struct`
  before the join. So even a small TEXTELR-only replay can pay close to full
  `articles × TEXTELR` expansion cost, defeating the point of scoping for performance.
- **The section-pairing lateral is O(links²) per TEXTELR**, since it re-`jsonb_array_elements`
  the same `structure_links` array for every `LIEN_ART` row. Large codes (thousands of links)
  make this expensive.

Concrete mitigations (any subset): add `CREATE INDEX … ON documents(source_uid)`; restructure so
scope prunes inputs (e.g. emit per-scope-dimension `UNION` branches, each with a single-relation
`ANY(...)` that the planner can index-drive, instead of one cross-relation `OR`); or precompute
TEXTELR→article→section pairs as persisted edges at ingest time so the backfill stays an indexed
join. At minimum, capture an `EXPLAIN (ANALYZE)` on a real code (e.g. Code civil) before the next
full ingest / `backfill-legi-hierarchy` maintenance run, since that command now executes this with
`$1 = true` (no scope filter at all). This overlaps the plan's already-tracked "maintenance
batching" item but is broader than batching alone.

### 2. Test gaps
- No test covers an article that has **both** a direct `LIEN_SECTION_TA` edge **and** a TEXTELR
  candidate, so the documented "direct first, TEXTELR fallback" precedence and the no-double-update
  guarantee are asserted only by code reading, not by a test.
- No test for the multi-section / deeper-`niv` case (e.g. an article whose nearest preceding
  section is a `niv=2` child), nor for a `LIEN_ART` with **no** preceding section (should yield no
  candidate). These pin the heuristic's boundaries the plan calls out as future work.
- No real-data assertion that the new path actually fires on `…/opendata` TEXTELR (the existing
  ignored archive test parses TEXTELR but does not run the backfill). A small ignored check that a
  real code enriches a TEXTELR-only article would raise confidence and surface the perf cost.

### 3. Minor
- The `tie_breaker` columns differ in form between branches (`edge.edge_id` vs
  `metadata_key || ':' || ordinality`); harmless since `source_rank` segregates them, but a one-line
  comment would prevent a future reader assuming a uniform key space.
- Consider a brief comment that the TEXTELR anchor deliberately rides on the preserved
  `attributes[]` `debut` (not the typed top-level `debut` field), so a future parser change that
  drops raw attributes wouldn't silently break anchor selection.

## Verification performed

- Read the full new CTE/`UNION ALL` query and the unchanged candidate-selection/anchor code
  (`projection.rs:281-554`), the scope struct/`is_empty` change, and the CLI scope-tracking +
  manifest wiring (`main.rs`).
- Confirmed the persisted structure-link JSON shape (from baseline `c995a93`) carries
  `attributes:[{key,value}]` including `debut`, and cross-checked real `LIEN_ART`/`LIEN_SECTION_TA`
  shapes and flat `<STRUCT>`+`niv` ordering under `/home/pierre/Apps/juridocs/opendata`.
- Checked schema indexes in `migrations.rs` (found `legi_metadata_roots(root_kind, source_uid)`
  but **no** `documents(source_uid)` index).
- Ran `cargo test -p jurisearch-storage persists_legi_metadata_roots_with_stable_keys` → 1 passed;
  `cargo test -p jurisearch-cli ingest_legi_archives_records_accounting_and_quarantines_failures`
  → 1 passed; `cargo clippy --workspace --all-targets -- -D warnings` → clean.
- Did not run an `EXPLAIN` against real-scale data (no full corpus loaded); the perf finding is
  derived from query structure and the index inventory.
