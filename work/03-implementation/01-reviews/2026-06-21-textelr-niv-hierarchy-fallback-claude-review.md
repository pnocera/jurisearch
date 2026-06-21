# Claude Review — Use TEXTELR Levels for Fallback Hierarchy Paths

Verdict: GO

- Commit reviewed: `4c2e1c2` "Use TEXTELR levels for fallback hierarchy paths"
- Baseline: `b84b493`
- Reviewer: Claude (Opus 4.8), 2026-06-21

The change is correct, idempotent, regression-free for the direct-edge path, faithful to the DILA
`niv`/`LIEN_SECTION_TA` model, and a sound completion of the deeper-ancestry item the prior slice
deferred. Tests pass and `clippy --workspace --all-targets -D warnings` is clean. Only non-blocking
items below.

## What I verified (correctness — sound)

- **`niv` interpretation matches the DTD.** `…/DTD/jorf/lien_section_ta.dtd` documents `niv` as
  "Niveau calculé de la section dans le sommaire du texte" (REQUIRED), and `LIEN_SECTION_TA` is
  `(#PCDATA)` carrying the section title; `STRUCT (LIEN_SECTION_TA|LIEN_ART)*` is flat. So building
  an ancestry stack from ordered `level`/`text` (`projection.rs:709-742`) is the right model.
- **The niv stack reconstructs ancestry correctly,** including the non-trivial cases: a deeper level
  appends; a shallower/equal level truncates to `level-1` then pushes (`projection.rs:719-726`), so
  sibling transitions (e.g. `niv1 A`, `niv1 B`) correctly reset the active branch and a level gap
  (e.g. `niv1` then `niv3`) is handled gracefully via `.min(stack.len())` (append, no panic).
  Absent/negative `level` falls back to plain append. The stack's leaf is the nearest-preceding
  section (the one joined to `SECTION_TA`), so it is consistent with `text_section`.
- **Merge with persisted metadata is conservative.** `merge_hierarchy_with_overlap`
  (`projection.rs:744-752`) dedups the longest shared boundary between the section's persisted
  `hierarchy_path` (title stripped) and the niv stack, and the niv result is used **only when
  strictly longer** than the `SECTION_TA`-derived path (`projection.rs:657-662`). A second gate
  (`hierarchy.len() <= current_hierarchy.len()` → no-op, `projection.rs:666-668`) guarantees
  idempotency and never shrinks an existing path.
- **Direct-edge path is byte-for-byte unchanged.** The direct branch passes
  `text_section_links_json = NULL`, so `enriched_article_hierarchy_json` runs the original
  `section_hierarchy_from_json` logic (extracted verbatim into a helper). Confirmed by the
  unchanged direct-edge assertion (`…419320` → `Titre preliminaire:absent`, depth 2).
- **Direct precedence is now enforced structurally, not just by ordering.** The TEXTELR branch adds
  `AND NOT EXISTS (… graph_edges … source_tag='LIEN_SECTION_TA')` (`projection.rs` CTE), so an
  article with any direct publisher section edge is excluded from TEXTELR candidates entirely —
  text-only replays cannot override direct hierarchy. This uses the indexed `graph_edges_from_idx`
  anti-join and also prunes those articles before the expensive lateral expansion.
- **Test coverage is meaningful:** TEXTELR-only article `…52000003` gains the deeper niv path
  `["Code civil","Livre III","Titre preliminaire"]`; the direct-edge article keeps its depth-2 path;
  the pre-section `LIEN_ART` `…52000004` (no preceding `LIEN_SECTION_TA`) stays `absent`; and the
  niv enrichment is idempotent (`repeated … documents_updated == 0`, and the subsequent full
  backfill leaves `…52000003` untouched). `documents.source_uid` index (migration 5,
  `migrations.rs:233`) — my prior-review recommendation — is present.

## Non-blocking suggestions

### Design observations
1. **Direct-edge articles can never gain deeper niv ancestry.** The `NOT EXISTS` is a hard exclusion,
   so an article with a direct `LIEN_SECTION_TA` edge but a *shallow* `SECTION_TA.hierarchy_path`
   keeps the shallow path even when the same code's TEXTELR would supply a deeper one. This is a
   defensible authority choice (and matches the baseline's effective behavior), but it can leave
   hierarchy depth inconsistent within one code between direct-edge and TEXTELR-only articles. Worth
   a one-line note in the code/plan so a future reader knows it is intentional rather than an
   oversight. (Direct-edge sections normally carry their own `SECTION_TA` ancestry, so impact should
   be small.)
2. **Whole-path replacement on length.** When the niv stack is longer it replaces the section path
   wholesale (after overlap dedup); if the niv `text` labels and `SECTION_TA.hierarchy_path` labels
   disagree on an *intermediate* title (not just depth), the niv labels win. TEXTELR is the
   authoritative structural view so this is acceptable, but a comment noting the precedence would help.

### Test gaps (the logic's more interesting branches are untested)
3. No test exercises a niv **gap** (`niv1` → `niv3`) or a **sibling reset** (`niv1 A`, `niv1 B`),
   which are exactly the `truncate`/dedup cases that distinguish this slice from "nearest preceding
   section." The current fixture only walks a clean `niv1 → niv2`.
4. No test exercises `merge_hierarchy_with_overlap` with `overlap > 0` (the test's base `["Code
   civil"]` shares nothing with the suffix, so only the `overlap == 0` path runs), nor the
   "niv stack not richer than SECTION_TA → section used" false branch of the richness gate.
   A fixture where `SECTION_TA.hierarchy_path` already contains an intermediate level would cover both.

### Performance (carried over, still deferred)
5. The branch now does a **third** `jsonb_array_elements` over `structure_links` per fallback
   `LIEN_ART` (`text_sections` `jsonb_agg`), keeping the same O(links²)-per-TEXTELR class as the
   baseline but a larger constant. Combined with the prior review's note that the scope `OR` spans
   multiple relations (so scoped replays may not prune the join), this reinforces the plan's
   still-open action to capture `EXPLAIN (ANALYZE)` on a real loaded code (e.g. Code civil) before a
   corpus-scale `backfill-legi-hierarchy` run. Not introduced by this commit; just unchanged.

## Verification performed

- Read the full new SQL (`hierarchy_candidates` CTE with the added `text_sections` lateral and
  `NOT EXISTS`), the `enriched_article_hierarchy_json` refactor, and the new helpers
  `section_hierarchy_from_json` / `text_struct_hierarchy_from_links` /
  `merge_hierarchy_with_overlap` / `non_empty_json_str` (`projection.rs:648-767`).
- Hand-traced the test STRUCT (`niv1 "Livre III"`, `niv2 "Titre preliminaire"`, fallback article)
  through the stack build and overlap merge to `["Code civil","Livre III","Titre preliminaire"]`,
  matching the asserted output; and traced the sibling-reset / level-gap edge cases.
- Confirmed `niv` semantics and `LIEN_SECTION_TA (#PCDATA)` titles in
  `/home/pierre/Apps/juridocs/DTD/jorf/lien_section_ta.dtd` and the flat `STRUCT` content model;
  spot-checked real LEGI `texte/struct` members under `/home/pierre/Apps/juridocs/opendata`
  (observed `niv` present and required; simple decrees are flat `niv=1`, deeper levels are a code
  concern per the DTD's "calculated TOC level").
- Confirmed migration 5 `documents_source_uid_idx` (`migrations.rs:233`).
- Ran `cargo test -p jurisearch-storage persists_legi_metadata_roots_with_stable_keys` → 1 passed;
  `cargo clippy --workspace --all-targets -- -D warnings` → clean.
- Did not capture `EXPLAIN (ANALYZE)` against a real loaded corpus (none loaded); the perf note is
  structural and explicitly deferred by the plan.
