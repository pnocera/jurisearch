# Review: Tolerate missing LEGI article display metadata

VERDICT: GO

- Commit reviewed: `1feefbc` "Tolerate missing LEGI article display metadata"
- Baseline: `2980ad1`
- Reviewer: Claude (Opus 4.8)
- Date: 2026-06-21

## Summary

ARTICLE canonicalization no longer hard-requires `META_ARTICLE/NUM` and `META_ARTICLE/TYPE`.
Missing/empty `NUM` falls back to a deterministic title `Article {LEGIARTI id}`; missing/empty
`TYPE` is preserved as `source_article_type = None` and recorded as `type=absent` in
`canonical_version`. Body text (`BLOC_TEXTUEL/CONTENU`) and the identity/temporal fields
(`NATURE`, `DATE_DEBUT`, `DATE_FIN`) remain required. A focused unit test is added and the plan
is updated with the real-data improvement (parse gaps 9 → 1 in the observed window).

The change is correct, narrowly scoped to display metadata, preserves byte-identical output for
well-formed articles (no re-index churn), compiles clean, is clippy-clean, and all unit tests
plus the verified real-data scan pass. Acceptable to proceed.

## Findings

No correctness, data-loss, regression, or missing-test blockers.

### Verified correct (high-confidence)

- **No regression for well-formed articles.** When `NUM` and `TYPE` are present, every derived
  value is byte-identical to the prior code: `title` = `Article {num}`
  (`crates/jurisearch-ingest/src/legi/mod.rs:874-877`), `source_article_type = Some(type)`
  (`:895`), and `canonical_version`'s `type=` segment renders the literal type
  (`:906-910`, `as_deref().unwrap_or("absent")` yields the type when present). So `canonical_json`
  and `canonical_version` are unchanged for existing records — no spurious compatibility bump and
  no re-index churn. The `legi_article:v1` scheme is correctly left as-is.

- **Identity/temporal fields stay required.** Only the two display fields moved from `required` to
  `optional_non_empty` (`:861-862`). `NATURE`, `DATE_DEBUT`, `DATE_FIN`, and the `LEGIARTI` id are
  still required, so `document_id` (`legi:{id}@{valid_from}`), `source_uid`, and validity remain
  well-formed. `NUM` was only ever an input to `title` (no `source_num` field exists), so dropping
  its requirement cannot affect identity or keys.

- **Deterministic, validate-safe fallback.** `title` is always `Some(...)`, so
  `article_chunk_context` (`:1071-1077`) still appends it and `contextualized_body` is well-formed.
  `CanonicalDocument::validate` (`:160-211`) does not require `title`/`type` and still enforces a
  non-empty `body` (`:199`), so the tolerant path validates only when real body text exists.

- **No downstream assumption on type.** `source_article_type` is already `Option<String>` in the
  schema (`:145`); a grep across `jurisearch-storage`, `jurisearch-cli`, and `jurisearch-core`
  found no consumer of `source_article_type`/`article_type`, so `None` is safe end-to-end.

- **Body-less gap correctly retained.** `required_non_empty("article", "BLOC_TEXTUEL/CONTENU", …)`
  (`:869`) still rejects empty/missing body, matching the documented "remaining parser gap."

### Low / recommendations (non-blocking)

1. **Unit test covers only the both-missing case** (`:1759-1778`). It removes both `<NUM>` and
   `<TYPE>` lines. Because the two fields are handled independently, separate cases
   (NUM-only-absent, TYPE-only-absent) and the present-but-empty-tag case (`<NUM></NUM>`, which
   `optional_non_empty` also collapses to `None`) are not directly exercised. Adding one or two
   more asserts would lock the per-field behavior. Low priority.

2. **No focused unit test pins the body-less rejection.** The intentional remaining boundary
   (empty `BLOC_TEXTUEL/CONTENU` still errors) is enforced in code and confirmed by the real-data
   scan, but a small unit test asserting `MissingRequiredField { field: "BLOC_TEXTUEL/CONTENU" }`
   would guard against a future change accidentally making the body optional too. Pre-existing
   gap, not introduced here; worth adding alongside this slice.

3. **`Article {LEGIARTI id}` is a machine-y display title.** It is a sound deterministic fallback
   and the right call over inventing a number, but a future UX pass may want a friendlier label
   (e.g. derived from the hierarchy path). Informational only.

## Tests considered / run

- `cargo clippy -p jurisearch-ingest --tests` — clean, no warnings.
- `cargo test -p jurisearch-ingest --lib legi::` — **21 passed**, including the new
  `accepts_articles_without_num_or_type_as_absent_metadata` (asserts the `Article {id}` title
  fallback, `source_article_type == None`, `type=absent` in `canonical_version`, the chunk
  `contextualized_body` containing the fallback title, and `validate().is_ok()`).
  `rejects_missing_required_fields` still passes because it asserts `NATURE` (checked before, and
  still required), not `NUM`/`TYPE` — consistent with the new behavior.
- `cargo test ... real_archive_covers_article_status_and_temporal_variants -- --ignored` —
  **passed**; reproduced the claimed improvement: **8,183 ARTICLE members parsed (up from 8,175)
  across 10,322 visited, with exactly 1 remaining parse error** — the body-less
  `LEGIARTI000006851607.xml` (`missing required field BLOC_TEXTUEL/CONTENU`). This confirms the 8
  previously-failing missing-`NUM`/`TYPE` records now parse and the 9 → 1 plan claim.
- Did not run storage/CLI suites: no code outside the LEGI parser changed and `source_article_type`
  has no downstream consumers.
