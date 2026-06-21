# Claude Review — TEXTELR Backfill EXPLAIN Evidence

Verdict: GO

- Step reviewed: uncommitted evidence + plan update
  - `work/03-implementation/02-evidence/2026-06-21-textelr-backfill-explain.md` (new)
  - `work/03-implementation/IMPLEMENTATION_PLAN.md` (1 status line moved Done, Remaining narrowed)
- Reviewer: Claude (Opus 4.8), 2026-06-21

This is an evidence + documentation step (no code change). The methodology is technically sound,
the captured plans are faithful to the real query, and the interpretation is honestly scoped — it
explicitly does **not** claim full-corpus safety and keeps the corpus-scale re-check as Remaining.
Nothing here should block the next implementation step. The one substantive caveat (the measured
plan is optimistic because the test index holds a single TEXTELR row) is acknowledged in the
evidence and correctly gated against the full-corpus maintenance backfill.

## Why GO

- **Real data, real query, real EXPLAIN.** Mini-indexes were built from official LEGI 20250713
  members (article/section_ta/texte), ingested through the real CLI, and `EXPLAIN (ANALYZE,
  BUFFERS)` was run against the production CTE in
  `backfill_legi_article_hierarchy_from_metadata_scoped` (`projection.rs:291-377`). I re-read the
  raw plan still present at `/tmp/jurisearch-textelr-heavy.130i8j/explain-full.txt` and confirmed the
  evidence file's abridged/annotated excerpt is **faithful**: node-for-node it maps to the direct
  Nested Loop (Seq Scan on `graph_edges`), the TEXTELR Anti Join, the three `jsonb_array_elements`
  scans (article_link loops=1/220 rows, nearest-section Limit loops=220, preceding-section Aggregate
  loops=33), the `documents_source_uid_idx` probe (Index Searches: 220), and the
  `graph_edges_from_idx` anti-join. Execution 91.8 ms / Buffers shared hit=2640.
- **It validates the prior review's index recommendation.** Migration 5's `documents_source_uid_idx`
  is the join path used for the LIEN_ART→document lookup, exactly as intended.
- **It surfaces a genuinely useful structural finding.** Current Code civil's TEXTELR has only 8
  structure links (7 `LIEN_SECTION_TA`, 0 `LIEN_ART`), i.e. large in-force codes do **not** drive the
  `LIEN_ART` fallback (their articles carry direct publisher edges and are excluded by the
  `NOT EXISTS`). The fallback load concentrates in smaller older/TNC texts — a real mitigating fact.
- **The interpretation is appropriately humble.** Lines 118–119 of the evidence explicitly note the
  scope predicate shows up as a join filter (not pushed down), that the JSONB lateral re-scans
  remain, and that the evidence "does not prove full-corpus backfill safety." The plan diff marks
  the evidence Done and narrows Remaining to "re-check on a true corpus-scale full backfill and add
  maintenance batching or structure-link materialization." No overclaim.

## Non-blocking suggestions

1. **State more prominently *why* the measured plan is optimistic.** The good plan shape
   (drive from `text_struct`, expand its links once, index-probe `documents`) is a direct consequence
   of the test index containing **exactly one** TEXTELR row (`Index Scan … text_struct … rows=1.00
   loops=1`). The single risk that motivated this EXPLAIN — whether, with tens of thousands of TEXTELR
   rows, the planner keeps this shape or instead drives from `documents` (≈ articles×TEXTELR) — is
   **not exercised** by a one-TEXTELR index. This is the most important unmeasured dimension and is
   currently only implied. A cheap stronger test before the first real `backfill-legi-hierarchy` full
   run: load a whole TNC/code branch (hundreds–thousands of TEXTELR + their articles) into one index
   and EXPLAIN the `full_scope` candidate query. (The plan's Remaining line covers this; the evidence
   could call it out explicitly.)
2. **The fallback-heavy text under-stresses section depth.** It has 1 `LIEN_SECTION_TA` for 220
   `LIEN_ART`, so the niv stack is trivially depth-1 and the preceding-section aggregate keeps 1 row
   per candidate. The O(sections × articles) dimension of the two section laterals (the part that
   grows with deep tables of contents) is not measured; a text with many sections would exercise it.
3. **The direct branch cost at corpus scale is unmeasured here.** In this text the direct branch
   matched 0 edges (`Seq Scan on graph_edges … Rows Removed by Filter: 4307`, downstream "never
   executed"). At `full_scope` on the real corpus, that branch sequentially scans the largest table
   (`graph_edges`) filtering on the un-indexed `payload->>'source_tag'`, which is plausibly a bigger
   cost than the TEXTELR JSON expansion. Worth folding into the same corpus-scale re-check, noting it
   is pre-existing rather than introduced by the TEXTELR work.
4. **Preserve the raw EXPLAIN artifacts.** `explain-full.txt` / `explain-text_scope.txt` live only in
   ephemeral `/tmp/jurisearch-textelr-heavy.130i8j/` and will be GC'd; copy them into
   `02-evidence/` (or append the raw output) so the abridged inline plans stay independently
   verifiable. A one-line note that the inline plans are abridged/annotated (not raw) would also help.

## Verification performed

- Read the evidence file and the plan diff; cross-checked every interpretation claim against the
  query source (`projection.rs:283-377`) and the **raw** EXPLAIN output in `/tmp/...explain-full.txt`.
- Confirmed the abridged plan is faithful and the cited metrics (220/33 rows, ~90 ms,
  `documents_source_uid_idx`, three lateral scans, anti-join) are accurate.
- Confirmed the structural claims (current Code civil TEXTELR shallow; fallback-heavy text 220
  `LIEN_ART`/1 `LIEN_SECTION_TA`) are internally consistent (220+1+2 LIEN_TXT = 223 links; 33 of 220
  articles fall after the single section → 33 candidates).
- Confirmed the plan update does not assert full-corpus safety and keeps the corpus-scale re-check +
  batching/materialization as Remaining — so no follow-up blocks the next step; only the eventual
  full `backfill-legi-hierarchy` maintenance run is (correctly) gated behind that re-check.
