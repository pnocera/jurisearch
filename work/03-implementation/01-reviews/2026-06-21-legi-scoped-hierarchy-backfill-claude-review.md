# Review: Scope LEGI hierarchy backfill to ingest run

VERDICT: GO

- Commit reviewed: `f06d405` "Scope LEGI hierarchy backfill to ingest run"
- Baseline: `891d643`
- Reviewer: Claude (Opus 4.8)
- Date: 2026-06-21

## Summary

The end-of-run hierarchy backfill is now scoped to what the current ingest run touched.
`ingest legi-archives` accumulates the article `document_id`s and `SECTION_TA` source UIDs it
persisted, builds a `LegiHierarchyBackfillScope`, and calls the new
`backfill_legi_article_hierarchy_from_metadata_scoped`; it skips the backfill entirely when the
scope is empty. The storage query gains a `WHERE` clause
(`$1::boolean OR d.document_id = ANY($2) OR edge.payload->>'to_source_uid' = ANY($3)`) and the
full backfill is preserved as a thin wrapper passing an empty (== full) scope. Scoped
document/section counts are reported in both command output and the stored manifest coverage.

The change is correct for the current single-section hierarchy model, compiles clean, is
clippy-clean, and both the storage and CLI contract tests pass against a live Postgres. It is
acceptable to proceed.

## Findings

No correctness, data-loss, regression, or missing-test blockers found.

### Verified correct (high-confidence)

- **Scope covers both change directions.** The `WHERE` predicate
  (`crates/jurisearch-storage/src/projection.rs:295-299`) re-evaluates an article when either
  (a) the article itself was (re)ingested — `d.document_id = ANY($2)` — or (b) the article links
  to a section UID ingested this run — `edge.payload->>'to_source_uid' = ANY($3)`. That is
  exactly the set of articles whose temporal section selection can change as a result of the run:
  a new `SECTION_TA` version pulls in every article pointing at that UID even if the article was
  not re-ingested. No reachable article is missed for the single-section case.

- **Single-section articles: scoped == full.** Because the join keys on
  `section.source_uid = edge.payload->>'to_source_uid'`, an article pointing at one touched
  section UID loads *all* versions of that section (they share the same `to_source_uid`), so the
  per-article candidate group is complete and `select_hierarchy_backfill_candidate` picks the
  same version a full backfill would. Confirmed by the new test: an out-of-scope article stays
  `absent` under the scoped run, then resolves to `Titre preliminaire` under the full backfill.

- **Skip-when-empty is correct.** A run that touches neither articles nor sections (e.g. a resume
  that only re-saw `TEXTE_VERSION`/`TEXTELR`, or skipped everything) produces an empty scope and
  skips the backfill (`crates/jurisearch-cli/src/main.rs:792`). Those root kinds do not feed the
  article→section enrichment, so skipping changes nothing. Counters default to 0, so the manifest
  stays consistent.

- **Parameterized + injection-safe.** All scope values are bound parameters (`$1/$2/$3`), never
  interpolated (`projection.rs:302`). Empty `text[]` binds cleanly and `= ANY('{}')` is simply
  false, so a scope with one populated and one empty vector (the common "only articles touched"
  case, exercised by the storage test) works without a special case.

- **Tracking is gated on success.** Article IDs are recorded only on the
  `Ok(ParsedLegiXml::Article)` path after `insert_legi_documents`
  (`crates/jurisearch-cli/src/main.rs:944,964`), and section UIDs only when `section_id` is
  `Some` (`:982,994-999`). Skipped/quarantined members are not added, which is the intended
  scope. Sets are `BTreeSet`, so reported counts are distinct.

- **Reporting parity.** The scoped counts are emitted identically in the manifest coverage block
  (`:669-670`) and the command payload (`:851-852`), and the CLI contract test asserts both.

## Recommendations (non-blocking)

1. **Resume durability after an interrupted run (Medium).** Previously every run ended with a
   *full* backfill, so an interruption was self-healing — the next run re-backfilled everything.
   With scoping, if a process crashes after a member is persisted/recorded-as-done but before the
   end-of-run backfill, a later resume will *skip* that member (not in scope) and never enrich it
   automatically. This is narrow (requires a crash in that window) and recoverable via the
   preserved full-backfill entry point, but it is a real reduction in self-healing. Consider
   either recording backfill completion per run (so an incomplete prior backfill re-widens the
   next scope) or documenting "after an interrupted ingest, run an explicit full hierarchy
   rebuild" as the operator recovery step. The plan already lists maintenance batching for full
   rebuilds, which would cover this.

2. **Multi-link articles via the touched-section path (Low/Medium).** An article that was *not*
   re-ingested this run but links to several *distinct* section UIDs, only some of which were
   touched, loads only the touched-section rows — a partial candidate group — so its scoped
   selection can differ from a full backfill. This only bites multi-`LIEN_SECTION_TA` articles,
   which is explicitly deferred work ("multi-link hierarchy assembly" remains in the plan) and
   appears not to occur in the current data shape (article `CONTEXTE` uses `TM`/`TITRE_TM`, not
   multiple `LIEN_SECTION_TA`). No data loss — the "strictly richer path" guard prevents
   downgrade and a maintenance full backfill reconciles. Worth a comment near the scope `WHERE`
   so the limitation is not forgotten when multi-link support lands.

3. **Empty-scope overloading (Low).** `backfill_..._scoped` treats an empty scope as *full*
   backfill (`projection.rs:275`), while the CLI treats an empty scope as *skip*. The semantics
   are handled correctly at both call sites, but a future caller could reasonably expect empty ==
   "do nothing" and accidentally trigger a full-corpus scan. A doc comment on the scoped function
   stating "empty scope = full backfill" would remove the footgun.

## Tests considered / run

- `cargo clippy -p jurisearch-storage -p jurisearch-cli --tests` — clean, no warnings.
- `cargo test -p jurisearch-storage --test legi_metadata_projection
  persists_legi_metadata_roots_with_stable_keys` — **passed** (live Postgres, ~1.3s). I confirmed
  this is a genuine scoping guard: it inserts an out-of-scope article (`LEGIARTI000052000002`)
  linked to the same section UID, runs a scoped backfill over two other documents
  (`documents_updated == 2`, `embeddings_invalidated == 1`), asserts the out-of-scope article's
  `hierarchy_path[1]` is still `absent`, then runs the *full* backfill and asserts it becomes
  `Titre preliminaire` (`documents_updated == 1`, `embeddings_invalidated == 0`). This directly
  exercises both exclusion and the full-vs-scoped distinction.
- `cargo test -p jurisearch-cli --test cli_contract
  ingest_legi_archives_records_accounting_and_quarantines_failures` — **passed** (~3.3s). Asserts
  `hierarchy_backfill_scoped_documents == 1` / `hierarchy_backfill_scoped_sections == 1` in both
  the command payload and `manifest.coverage`.
- I traced the scope-build/skip guard (`main.rs:779-803`) and the success-gated tracking inserts;
  the preserved full-backfill wrapper has no remaining non-test callers, consistent with its
  stated "maintenance and tests" role.
