# Claude Review — Phase 1.2 Structural-Survival Test

- Step reviewed: uncommitted structural-survival test + plan update
  - `crates/jurisearch-storage/tests/structural_survival.rs` (new)
  - `work/03-implementation/IMPLEMENTATION_PLAN.md` (1 Done line added, Remaining trimmed)
- Reviewer: Claude (Opus 4.8), 2026-06-21

This is a test-only addition plus a documentation update. The test is correct, meaningful (not
trivially passing), exercises the full parse → chunk → storage → `context` chain at realistic depth,
and satisfies the plan's structural-survival acceptance item. It runs (not skips) and passes locally.
No source code changed, so there is no regression surface. Only minor non-blocking suggestions.

## What I verified

- **The assertions reflect real parser behavior, not a tautology.** The LEGI parser builds
  `hierarchy_path` by pushing the trimmed text of every `TITRE_TXT`/`TITRE_TM` inside `CONTEXTE` in
  document order (`crates/jurisearch-ingest/src/legi/mod.rs:770-773,812-815`), which is depth-agnostic.
  The test's deeply nested 5-level `<TM>` fixture therefore genuinely exercises
  `Code civil → Livre → Titre → Chapitre → Section`, and the **exact** `assert_eq!(hierarchy_path,
  expected_path)` (5 entries) confirms no spurious whitespace/empty entries leak in. This is a real
  extension over the existing 2-level parser unit fixture.
- **The fixture matches real DILA shape.** `<CONTEXTE><TEXTE><TITRE_TXT>…</TITRE_TXT><TM><TITRE_TM>…`
  nesting is exactly how official LEGI article files encode ancestry (consistent with the in-repo
  article fixture at `legi/mod.rs:2031-2034` and the real members I inspected in prior reviews). The
  Code→Livre→Titre→Chapitre→Section spine is the standard French code hierarchy.
- **Full-chain survival is checked at every stage:**
  1. Parser: `document.hierarchy_path` and `chunks[0].hierarchy_path` both equal the 5-level path;
     `contextualized_body` starts with the joined ancestry.
  2. Storage projection: real durable Postgres insert, then `chunks.hierarchy_path` contains the deep
     `Chapitre III…` level and `contextualized_body` is `Code civil > … > Section 1 … > Article 1`
     (verifying the leaf section **and** the article title are appended).
  3. `context_documents_json`: `ancestry` equals the full 5-level path, `target.title == "Article 1"`,
     `target.hierarchy_path[4] == "Section 1 : Des formalites"`, and `sibling_count == 0` (a clean
     no-sibling result with `include_siblings: true`, valid at `as_of = valid_from`).
- **Determinism / wiring:** the hardcoded `document_id` matches the computed
  `legi:LEGIARTI000000000001@2024-01-01`; the test reuses `common::discover_pg_config` and skips
  gracefully without Postgres, consistent with the other storage integration tests. Confirmed it
  actually executes (`1 passed`, not `0 ignored`).
- **Plan accuracy:** the new Done line correctly scopes the claim to a *synthetic* article and the
  four stages the test actually covers (no real-corpus overclaim), and "full-path structural-survival
  tests" is appropriately removed from Remaining.

## Non-blocking suggestions

1. **No deep-section sibling assertion.** The test ends with `sibling_count == 0`. Sibling grouping is
   `hierarchy_path`-equality (depth-agnostic) and is covered at 3 levels in `retrieval_smoke`, so this
   is only an incremental gap — but adding a second article in the same 5-level Section (and one in a
   sibling Section/Chapitre) would prove that deep-path equality groups/excludes siblings correctly
   end-to-end, which is the more interesting structural-survival property.
2. **Backfill-derived hierarchy is not exercised here.** The test inserts a CONTEXTE-complete article
   directly, so it validates the "parser already has the full path" route. The complementary route —
   an article with a thin/empty `CONTEXTE` whose hierarchy is reconstructed by the SECTION_TA/TEXTELR
   hierarchy backfill — survives through different code and is covered in `legi_metadata_projection`.
   Worth a one-line comment in the test (or plan) noting that structural survival has two sources so a
   future reader doesn't assume this test covers the backfill path.
3. **Partial string assertions.** `contextualized_body` is checked with `starts_with`/`contains`;
   asserting the exact full contextualized string (path + `> Article 1` + body) would tighten the
   guarantee. Minor — the current checks are robust and readable.

## Verification performed

- Read the full test and cross-checked each assertion against the parser's CONTEXTE/`TITRE_TM`
  hierarchy extraction, the chunk builder's `contextualized_body` format, and the
  `context_documents_json` output shape.
- Ran `cargo test -p jurisearch-storage --test structural_survival -- --nocapture` → **1 passed**
  (executed against live Postgres, not skipped). The author's broader `cargo test -p
  jurisearch-storage`, `cargo test --workspace`, and `cargo clippy --workspace --all-targets -D
  warnings` runs are consistent with a test-only, source-unchanged addition (no regression surface).

Verdict: GO
