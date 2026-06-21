# Review: Select temporal LEGI section hierarchy

VERDICT: GO

- Commit reviewed: `4d4344f` "Select temporal LEGI section hierarchy"
- Baseline: `6e48053`
- Reviewer: Claude (Opus 4.8)
- Date: 2026-06-21

## Summary

The hierarchy backfill now picks the `SECTION_TA` metadata version whose validity window
contains the publisher `LIEN_SECTION_TA` edge `debut` date, falling back to the article's
own `valid_from`, and finally to the latest section row when dates are missing/malformed.
The SQL drops `DISTINCT ON` and returns all `(article × edge × section)` candidate rows;
per-article selection moves into Rust (`select_hierarchy_backfill_candidates` →
`select_hierarchy_backfill_candidate`). A regression fixture adds a second `SECTION_TA`
version for the same UID and asserts an 1804 article does not pick the 2020 title.

The change is correct, compiles clean, is clippy-clean, and the regression test passes
against a live Postgres. It is acceptable to proceed.

## Findings

No correctness, data-loss, regression, or missing-test blockers found.

### Verified correct (high-confidence)

- **Half-open interval matches existing temporal semantics.**
  `section_validity_contains` (`crates/jurisearch-storage/src/projection.rs:439-450`) uses
  `valid_from <= anchor` AND `anchor < valid_to`. This is byte-for-byte the same convention
  as the retrieval `as_of` filter (`crates/jurisearch-storage/src/retrieval.rs:52-53,79-80`:
  `valid_from <= as_of` and `valid_to > as_of`). Consistent, so an anchor on a version
  boundary maps to the newer version, as intended.

- **Sentinel end dates handled.** `SECTION_TA` `valid_to` is produced by `normalize_end_date`
  (`crates/jurisearch-ingest/src/legi/mod.rs:1282-1289,986`), which maps `2999-01-01` /
  `2999-12-31` to `None`. `section_validity_contains` treats `None` as open-ended via
  `is_none_or`, so open-ended sections correctly contain any in-range anchor.

- **String date comparison is sound.** `is_iso_date` (`projection.rs:452-460`) gates both
  operands to `YYYY-MM-DD` (length 10, dashes at indices 4/7, digits elsewhere) before any
  `<=` / `<`. For that fixed format, lexicographic ordering equals chronological ordering,
  so the string comparisons are valid. Over-long / malformed inputs are rejected (the `.all()`
  + `len() == 10` combination handles `"2020-01-01T..."`, short strings, and non-ASCII).

- **End-to-end wiring is real, not fixture-only.** The `debut` anchor is read from
  `payload.attributes[].{key,value}` (`projection.rs:416-437`). Production edges populate this:
  `collect_attributes` (`legi/mod.rs:1165-1182`) captures every link attribute including
  `debut`, and `insert_publisher_edge` serializes the whole `CanonicalGraphEdge`
  (incl. `attributes`, `source_tag`, `to_source_uid`) into `graph_edges.payload`
  (`projection.rs:713-733`). The SQL `attributes`/`source_tag`/`to_source_uid` reads line up.

- **Common case is preserved (no regression).** For the typical article (one link, one section
  version) the candidate group has a single row; selection returns it just as the old
  `DISTINCT ON` did. Behavior only diverges when multiple versions exist — the intended change.

- **Grouping correctness.** `select_hierarchy_backfill_candidates` relies on rows for one
  `document_id` being contiguous; the SQL `ORDER BY` leads with `d.document_id`
  (`projection.rs:270`), satisfying that. The `edge.edge_id` tiebreak makes the order fully
  deterministic, and exactly one candidate is emitted per document (no duplicate-update risk).

- **Idempotency.** The repeated-backfill assertions (`documents_updated == 0`,
  `embeddings_invalidated == 0`) still hold because `enriched_article_hierarchy_json` only
  rewrites when the metadata path is strictly richer; verified by the passing test.

## Recommendations (non-blocking)

1. **Add a fixture that exercises the exclusive end boundary.** Both sections in the new
   fixture have `fin="2999-01-01"`, which normalizes to `valid_to = None`. The test therefore
   only exercises the `valid_from <= anchor` gate, never the `anchor < valid_to` (exclusive
   upper bound) branch. A fixture where the older version has a real `fin` equal to the newer
   version's `debut` (e.g. older `1804-03-21..2020-01-01`, newer `2020-01-01..`) would lock in
   the half-open semantics and guard against a future regression to an inclusive end.
   Consider also covering the "edge has no `debut` → fall back to article `valid_from`" path.

2. **Tighten the fallback comment vs. behavior.** `select_hierarchy_backfill_candidate`
   (`projection.rs:393-414`) falls back to `candidates.remove(0)` (latest section) whenever no
   candidate's validity contains the anchor — including the case where the anchor is present
   but falls in a gap between versions, not only when "source evidence is incomplete" as the
   header comment (`projection.rs:254-256`) implies. Worth a one-line clarification; a
   closest-preceding-version fallback would arguably be more correct than latest-wins for the
   gap case, but latest-wins matches prior behavior and is fine for this slice.

3. **`is_iso_date` duplicates `validate_date`'s shape check.** `validate_date`
   (`legi/mod.rs:1291-1317`) already implements the same `YYYY-MM-DD` shape test (plus
   semantic month/day validation). They live in different crates so sharing is non-trivial, but
   a note/TODO to converge on one date helper would reduce drift risk.

4. **Cross-UID and multi-link selection is not version-scoped.** When an article links to
   several distinct section UIDs (e.g. an ancestor chain), the loop returns the first
   `(edge, section)` pair (in `valid_from DESC` order) whose section contains its own edge
   anchor — across UIDs, not scoped to one target. This is no worse than the prior
   latest-section-wins join and is explicitly deferred to the broader hierarchy-assembly slice,
   but flagging it so it isn't lost.

5. **Full-corpus scale.** Removing `DISTINCT ON` makes the query return the full
   `article × edge × section` product and loads all candidates into memory
   (`Vec::with_capacity(rows.len())`). Expansion is modest in practice (most articles are
   1×1), and IMPLEMENTATION_PLAN already tracks "scope or batch hierarchy backfill for
   full-corpus incremental runs," so this is acknowledged — no action for this step.

## Tests considered / run

- `cargo check -p jurisearch-storage --tests` — clean.
- `cargo clippy -p jurisearch-storage --tests` — clean, no warnings.
- `cargo test -p jurisearch-storage --test legi_metadata_projection
  persists_legi_metadata_roots_with_stable_keys` — **passed** against a live Postgres
  (1 passed, 0 failed; ~1.28s). I confirmed the test is a genuine regression guard: with the
  old latest-section-wins ordering it would select "Titre contemporain" (2020), whereas it
  asserts `hierarchy_path[1] == "Titre preliminaire"` (1804). I traced the selection by hand
  for the fixture (`debut=1804-03-21`; contemporary `valid_from=2020` fails `valid_from <=
  anchor`; preliminaire `valid_from=1804-03-21`, `valid_to=None` matches) — consistent with the
  passing result.
- Did not run the full storage suite (other tests are unrelated to this path and require the
  same managed-Postgres harness); the function signature is unchanged so the CLI caller
  (`crates/jurisearch-cli/src/main.rs:775`) is unaffected.
