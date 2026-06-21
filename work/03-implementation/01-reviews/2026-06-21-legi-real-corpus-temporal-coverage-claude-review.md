# Review: Add LEGI real corpus temporal coverage

VERDICT: GO

- Commit reviewed: `e38c437` "Add LEGI real corpus temporal coverage"
- Baseline: `46ee4ef`
- Reviewer: Claude (Opus 4.8)
- Date: 2026-06-21

## Summary

Adds a second `#[ignore]` real-data test,
`real_archive_covers_article_status_and_temporal_variants`, to
`crates/jurisearch-ingest/tests/legi_archive_subset.rs`. It scans the local official LEGI
baseline until it observes a target set of article statuses, finite `valid_to` examples per
non-open-ended status, and `DATE_FIN 2999-01-01` sentinel normalization (capped at 250k visited
members). It records — rather than fails on — optional `2999-12-31`/multi-version evidence and
real-corpus ARTICLE parse gaps. The implementation plan is updated with the observed evidence
and remaining parser-gap work.

This is a test-only + docs change. It compiles clean, is clippy-clean, and I ran the new ignored
test against the verified archive: **it passed in 2.72s**, visiting 10,322 XML members and
parsing 8,175 ARTICLE members, observing all 7 target statuses, all 5 finite-`valid_to` statuses,
and the `2999-01-01` sentinel. Acceptable to proceed.

## Findings

No correctness, data-loss, regression, or missing-test blockers. No production (`src/`) code
changed; the non-ignored suite is unaffected.

### Low / informational

1. **Multi-version and same-day-version detection is structurally inert**
   (`crates/jurisearch-ingest/tests/legi_archive_subset.rs:230-256`). For LEGI articles
   `version_group == source_uid == LEGIARTI id` (`crates/jurisearch-ingest/src/legi/mod.rs:886`),
   i.e. each article version is its own group keyed by its own per-version id. Consequently:
   - `version_dates_by_group` always holds exactly one `valid_from` per group, so
     `dates.len() > 1` is never true and `multi_version_group` can never be set.
   - `same_day_versions_by_group` inserts `source_uid` (== the group key) into a set keyed by
     `(version_group, valid_from)`, so `source_uids.len() > 1` is never true and
     `same_day_version_group` can never be set.

   The live run confirms this: both came back `None`. These fields are only logged (not
   asserted), so there is **no false pass/fail** — but the plan's recorded conclusion that "no
   … same-day-version, or multi-version-group evidence appeared" is misleading: the detectors
   cannot fire by construction, regardless of corpus content. To actually detect multiple
   temporal versions of one logical article you must group by the shared chronicle/ancestor id
   (the article CID common across versions), not the per-version LEGIARTI id. Recommend either
   keying these maps by that ancestor id (if the parser exposes it) or removing the inert
   tracking and softening the plan note to "multi-version grouping not yet implemented," so a
   future reader does not conclude LEGI articles are single-version.

2. **The deeper scan surfaces real ARTICLE parser strictness gaps (correctly recorded, tracked).**
   The run reported 9 article parse errors in the first 10,322 members, all under
   `code_et_TNC_non_vigueur/` (non-codified, no-longer-in-force texts), failing on missing
   `BLOC_TEXTUEL/CONTENU`, `META_ARTICLE/TYPE`, and `META_ARTICLE/NUM`
   (recorded at `legi_archive_subset.rs:264-271`, not asserted). This is a pre-existing parser
   limitation now made visible, not a regression introduced here; the plan adds "close
   full-corpus ARTICLE parser gaps for official files that omit currently-required fields" to the
   remaining list. Recording-not-failing is the right call for an evidence step, but flagging it
   because it means a full-corpus `ingest` would currently reject a small fraction (~0.1% here)
   of real official articles — relevant to the later full-corpus correctness work.

3. **The test is baseline-specific and asserts presence, so a different archive can fail rather
   than skip** (`legi_archive_subset.rs:24-34, 299-320`). The required-status list (including
   rarer `MODIFIE_MORT_NE`, `PERIME`) was verified against the documented 2025-07-13 baseline; an
   archive supplied via `JURISEARCH_LEGI_ARCHIVE` that lacks one within 250k members would fail
   the missing-status assertion. It is `#[ignore]`, so it never runs in CI, and the `ABROGE_DIFF`
   exclusion comment shows the author is aware of baseline specificity. Acceptable; just note the
   targets may need adjustment for a different official dump.

## Recommendations

- Fix or remove the inert multi-version/same-day detectors (Finding 1) and correct the plan note
  accordingly; as written it could misdirect the upcoming "multi-link hierarchy assembly across
  article versions" work.
- Consider asserting an upper bound on the article parse-error *rate* (e.g. fail if
  errors/attempts exceeds a generous threshold) so this smoke also guards against a future
  regression that makes the parser reject a large share of real articles, while still tolerating
  the known ~0.1% gap. Optional — out of scope for this evidence step.
- Optional: the verified sample is dominated by the `TNC_non_vigueur` branch (tar ordering), which
  is why out-of-force statuses appear so early; a one-line note in the test or plan would prevent
  over-reading the sample as representative of the in-force corpus.

## Tests considered / run

- `cargo test -p jurisearch-ingest --test legi_archive_subset --no-run` — compiles.
- `cargo clippy -p jurisearch-ingest --tests` — clean, no warnings.
- `cargo test ... real_archive_covers_article_status_and_temporal_variants -- --ignored
  --nocapture` — **passed** (2.72s) against
  `/home/pierre/Apps/juridocs/opendata/LEGI/Freemium_legi_global_20250713-140000.tar.gz`.
  Observed: 10,322 XML members visited, 8,175 ARTICLE parsed, 9 article parse errors (missing
  `BLOC_TEXTUEL/CONTENU` / `META_ARTICLE/TYPE` / `META_ARTICLE/NUM`); statuses VIGUEUR, MODIFIE,
  ABROGE, ANNULE, MODIFIE_MORT_NE, PERIME, TRANSFERE all present; finite `valid_to` examples for
  MODIFIE/ABROGE/ANNULE/PERIME/TRANSFERE; `2999-01-01` sentinel found; `2999-12-31`,
  multi-version, and same-day-version all `None` (the latter two structurally, per Finding 1).
  This matches the commit/plan's reported evidence.
- Did not re-run the full ingest unit suite: no `src/` changed, so existing tests are unaffected.
