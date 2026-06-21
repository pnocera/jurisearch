# Claude Review — LEGI No-Text Article Skip

Verdict: GO

- Commit reviewed: `6b78996` "Skip no-text LEGI articles during ingest"
- Baseline: `d77e2f3`
- Reviewer: Claude (Opus 4.8), 2026-06-21

## Findings

No correctness, data-loss, provenance, resume, or regression blockers. All findings are
non-blocking and listed by severity.

### Correctness, resume, and accounting — verified sound

- **Narrow, exact classifier (low false-positive risk).** `is_no_text_article_error`
  (`crates/jurisearch-cli/src/main.rs:1253-1259`) matches only
  `MissingRequiredField { entity: "article", field: "BLOC_TEXTUEL/CONTENU" }`. That error has a
  single producer — `required_non_empty("article", "BLOC_TEXTUEL/CONTENU", self.body)` in
  `crates/jurisearch-ingest/src/legi/mod.rs:869` — which fires for both a missing
  `BLOC_TEXTUEL/CONTENU` element and a present-but-empty one, i.e. exactly "no searchable body."
  Any other article error (invalid id/date, missing `NATURE`, malformed/truncated XML → an `Xml`
  event error) falls through unchanged to the Failed + quarantine path. So corrupt payloads are
  still quarantined; only genuinely body-less, well-formed articles are skipped.

- **Run completes correctly.** The no-text branch (`main.rs:1046-1064`) increments
  `skipped_members` and `skipped_no_text_articles` and `return Ok(())` without touching
  `failed_members` or setting `fatal_error`. Run status is `Completed` iff
  `failed_members == 0 && fatal_error.is_none()` (`main.rs:817-821, 829-833`), so a run containing
  only no-text skips completes — matching the test.

- **Resume behavior is an improvement, not a hazard.** The member is recorded with
  `IngestMemberStatus::Skipped` (a terminal status). On resume, `ingest_resume_decision`
  (`crates/jurisearch-storage/src/ingest_accounting.rs:457-465`) maps `Skipped` →
  `Skip`/"compatible_complete", so the article is not re-parsed and not retried. This is strictly
  better than the prior Failed path (`Failed` → `Retry`), which would re-fail the same body-less
  article on every resume. On resume the count shifts from `skipped_no_text_articles` to
  `skipped_compatible_members`, which is consistent with how inserted members are also re-counted
  as compatible-complete; idempotent either way.

- **Provenance preserved; quarantine noise avoided.** The skipped member is recorded in
  `ingest_member` with `source_entity` = the `LEGIARTI` id derived from the member path and
  `status = skipped`, with no `ingest_error` row and no quarantine copy. Dropping the quarantine
  copy is appropriate: a body-less record is a known-benign case, and the source archive (system of
  record) is retained, so the raw bytes remain recoverable. The test asserts the member row, zero
  error rows, and absence of the quarantine subdirectory.

- **DB-error safety.** `record_legi_member(...)?` in the no-text branch still propagates a storage
  error as fatal, so an accounting-write failure fails the run rather than being swallowed —
  consistent with the other arms.

- **No regression.** The change is an additive branch plus an additive counter in the manifest and
  output payload. The sibling failure/quarantine contract test still passes unchanged.

### Low — no per-member "reason" marker for no-text skips (observability)
The `ingest_member` row for a no-text skip is `status=skipped` with a `LEGIARTI` `source_entity`,
indistinguishable in the DB from other skip kinds except by inference (skipped + `LEGIARTI` entity
+ no matching `documents` row). The aggregate `skipped_no_text_articles` counter covers reporting,
but if per-member DB filtering is ever needed, a reason/detail column or a distinct status would
help. Non-blocking.

### Low — missing false-positive guard test
No test asserts that a *different* article parse error (e.g. invalid `DATE_DEBUT`, missing
`NATURE`) is still recorded `Failed`/quarantined rather than misclassified as no-text. The
classifier is narrow, but a targeted test would lock that boundary against a future broadening of
`is_no_text_article_error`. The existing accounting/quarantine test exercises a failure path
generally but not this specific discrimination.

### Low — missing resume test
A second run over the same archive asserting the no-text article becomes
`skipped_compatible_members` (and is not re-failed) would pin the resume idempotency described
above. Generic resume logic is covered elsewhere; this is a nice-to-have.

### Low — `legi_article_id_from_member_path` duplicates id-shape logic
`main.rs:1261-1271` re-implements the `LEGIARTI` + 12-digit validation that already exists in the
parser (`validate_id` / `extract_known_source_uid` in `jurisearch-ingest`). It is byte-safe
(`str::get` returns `None` on out-of-range/boundary, and the digit check rejects non-ids) and only
feeds a best-effort `source_entity`, so risk is low; reusing a shared helper would reduce drift.
It uses `find` (first `"LEGIARTI"` occurrence), which is correct for the official layout
(`…/LEGI/ARTI/…/LEGIARTI############.xml`, where `"LEGIARTI"` is contiguous only in the filename) —
a one-line comment noting that assumption would help.

## Verification

- Inspected the full diff and surrounding code: the new error-arm branch (`main.rs:1045-1064`), the
  classifier and id helper (`main.rs:1253-1271`), the counter wiring in the struct/manifest/payload
  (`main.rs:636, 685, 869`), the run-status computation (`main.rs:817-833`), the resume entry path
  (`main.rs:886-951`), and `ingest_resume_decision` in storage
  (`ingest_accounting.rs:404-474`).
- Confirmed the originating parser error site and semantics
  (`jurisearch-ingest/src/legi/mod.rs:869`, `required_non_empty`).
- Reviewed the new test: it builds a single body-less article via
  `article_fixture_without_body()` (a `.replace` of the `BLOC_TEXTUEL` block — self-protecting,
  since fixture drift would make the body present and fail the test) and asserts run `completed`,
  `inserted_documents=0`, `skipped_members=1`, `skipped_no_text_articles=1` (output and
  `manifest.coverage`), `failed_members=0`, `quarantined_payloads=0`, zero `documents`, the
  `ingest_member` row `skipped:LEGIARTI000006419320`, zero `ingest_error` rows, and no quarantine
  subdirectory.
- Ran `cargo test -p jurisearch-cli --test cli_contract ingest_legi_archives_skips_no_text_articles_without_failing_run`
  → **1 passed** (live Postgres).
- Ran the full `cargo test -p jurisearch-cli --test cli_contract` suite → **20 passed, 2 ignored**,
  including the sibling `ingest_legi_archives_records_accounting_and_quarantines_failures`
  (no regression to the failure/quarantine path).
- Ran `cargo clippy --workspace --all-targets -- -D warnings` → **clean**.
- Confirmed the plan update accurately records the new skip-not-fail behavior and trims the
  corresponding "Remaining" item.

## Recommendations

1. Add a test asserting a non-`CONTENU` article parse error is still `Failed`/quarantined, guarding
   the classifier against future broadening.
2. Add a resume test asserting a no-text article re-counts as `skipped_compatible_members` and is
   not retried.
3. Consider a per-member reason marker (column or distinct status) if no-text skips ever need
   direct DB filtering; the counter is sufficient for now.
4. Reuse the parser's id-extraction helper (or add a comment about the first-`LEGIARTI`-match
   assumption) in `legi_article_id_from_member_path` to avoid duplicated id-shape logic.
